//! Gemini REST client.
//!
//! Uses non-streaming `generateContent` (more reliable than SSE across model
//! variants) and synthesizes a token-by-token feel on the client by emitting
//! the response in small chunks via the `gemini://chunk` Tauri event.
//!
//! Safety settings are set to `BLOCK_ONLY_HIGH` because the typical workload
//! is coding questions, not safety-sensitive content.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter};

const DEFAULT_SYSTEM_PROMPT: &str = "You are helping a CS student during a coding practice test. \
Reply with working, copy-pasteable code first inside a single fenced ```lang code block, \
then a brief 2-3 sentence explanation after. No markdown headers, no preamble.";

#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user" or "model"
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageAttachment {
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    #[serde(rename = "dataBase64")]
    pub data_base64: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct StreamChunkPayload {
    pub request_id: String,
    pub text: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct StreamDonePayload {
    pub request_id: String,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<Candidate>>,
    #[serde(rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: Option<i64>,
    message: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PromptFeedback {
    #[serde(rename = "blockReason")]
    block_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: Option<Content>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Content {
    parts: Option<Vec<Part>>,
}

#[derive(Debug, Deserialize)]
struct Part {
    text: Option<String>,
}

pub async fn stream(
    app: AppHandle,
    request_id: String,
    api_key: String,
    model: String,
    history: Vec<ChatMessage>,
    prompt: String,
    images: Vec<ImageAttachment>,
    system_prompt: String,
) -> Result<()> {
    if api_key.trim().is_empty() {
        return Err(anyhow!("missing Gemini API key — set it in Settings"));
    }

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
        model.trim()
    );

    let mut contents: Vec<serde_json::Value> = history
        .into_iter()
        .map(|m| {
            json!({
                "role": if m.role == "model" { "model" } else { "user" },
                "parts": [{"text": m.text}],
            })
        })
        .collect();

    let mut current_parts: Vec<serde_json::Value> =
        Vec::with_capacity(1 + images.len());
    for img in &images {
        current_parts.push(json!({
            "inlineData": {
                "mimeType": img.mime_type,
                "data": img.data_base64,
            }
        }));
    }
    let prompt_text = if prompt.trim().is_empty() && !images.is_empty() {
        "Solve / explain what's in the image. If it's a coding question, reply with working code in a fenced block.".to_string()
    } else {
        prompt
    };
    current_parts.push(json!({"text": prompt_text}));
    contents.push(json!({
        "role": "user",
        "parts": current_parts,
    }));

    let sys = if system_prompt.trim().is_empty() {
        DEFAULT_SYSTEM_PROMPT.to_string()
    } else {
        system_prompt
    };
    let body = json!({
        "contents": contents,
        "systemInstruction": {
            "parts": [{"text": sys}],
        },
        "generationConfig": {
            "temperature": 0.4,
        },
        "safetySettings": [
            {"category": "HARM_CATEGORY_HARASSMENT",        "threshold": "BLOCK_ONLY_HIGH"},
            {"category": "HARM_CATEGORY_HATE_SPEECH",       "threshold": "BLOCK_ONLY_HIGH"},
            {"category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "BLOCK_ONLY_HIGH"},
            {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_ONLY_HIGH"}
        ]
    });

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| anyhow!("reqwest build: {e}"))?;

    let res = client
        .post(&url)
        .header("x-goog-api-key", api_key.trim())
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("request failed: {e}"))?;

    let status = res.status();
    let raw = res.text().await.unwrap_or_default();

    if !status.is_success() {
        // Try to parse Google's error envelope for a friendlier message
        if let Ok(parsed) = serde_json::from_str::<GeminiResponse>(&raw) {
            if let Some(err) = parsed.error {
                let msg = err.message.unwrap_or_else(|| raw.clone());
                let st = err.status.unwrap_or_default();
                let code = err.code.unwrap_or(status.as_u16() as i64);
                if code == 429 || st == "RESOURCE_EXHAUSTED" {
                    return Err(anyhow!(
                        "Rate limit (429). Free-tier {} is heavily throttled — wait a minute or switch to gemini-2.5-flash.",
                        model
                    ));
                }
                if code == 404 || st == "NOT_FOUND" {
                    return Err(anyhow!(
                        "Model '{}' not found. Try gemini-2.5-flash or gemini-2.5-pro.",
                        model
                    ));
                }
                if code == 400 {
                    return Err(anyhow!("Bad request: {msg}"));
                }
                if code == 401 || code == 403 {
                    return Err(anyhow!("Auth failed ({code}). Check your API key in Settings."));
                }
                return Err(anyhow!("Gemini error {code} {st}: {msg}"));
            }
        }
        return Err(anyhow!("Gemini HTTP {status}: {raw}"));
    }

    let parsed: GeminiResponse = serde_json::from_str(&raw)
        .map_err(|e| anyhow!("could not parse Gemini response: {e}\nraw: {raw}"))?;

    if let Some(fb) = &parsed.prompt_feedback {
        if let Some(reason) = &fb.block_reason {
            return Err(anyhow!("Prompt blocked by safety filter: {reason}"));
        }
    }

    let candidates = parsed.candidates.unwrap_or_default();
    if candidates.is_empty() {
        return Err(anyhow!("Gemini returned no candidates"));
    }

    let mut full = String::new();
    let mut finish_note: Option<String> = None;
    for cand in candidates {
        if let Some(reason) = cand.finish_reason {
            if reason != "STOP" && reason != "MAX_TOKENS" {
                finish_note = Some(reason);
            }
        }
        if let Some(content) = cand.content {
            if let Some(parts) = content.parts {
                for part in parts {
                    if let Some(t) = part.text {
                        full.push_str(&t);
                    }
                }
            }
        }
    }

    if full.is_empty() {
        if let Some(reason) = finish_note {
            return Err(anyhow!(
                "Empty response (finishReason: {reason}). Try rephrasing or use a different model.",
            ));
        }
        return Err(anyhow!("Empty response from Gemini"));
    }

    // Chunk the answer so the UI animates in (poor man's streaming).
    let chars: Vec<char> = full.chars().collect();
    let total = chars.len();
    let chunk_size = (total / 30).max(8);
    let mut i = 0;
    while i < total {
        let end = (i + chunk_size).min(total);
        let piece: String = chars[i..end].iter().collect();
        app.emit(
            "gemini://chunk",
            StreamChunkPayload {
                request_id: request_id.clone(),
                text: piece,
            },
        )
        .ok();
        i = end;
        tokio::time::sleep(std::time::Duration::from_millis(12)).await;
    }

    Ok(())
}

pub fn emit_done(app: &AppHandle, request_id: String, error: Option<String>) {
    app.emit(
        "gemini://done",
        StreamDonePayload { request_id, error },
    )
    .ok();
}
