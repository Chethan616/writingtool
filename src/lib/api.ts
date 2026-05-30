import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Store } from "@tauri-apps/plugin-store";

export type ChatMessage = { role: "user" | "model"; text: string };

export type ImageAttachment = {
  mimeType: string;
  dataBase64: string;
  previewUrl?: string;
};

let storePromise: Promise<Store> | null = null;
export function getStore(): Promise<Store> {
  if (!storePromise) storePromise = Store.load("settings.json", { autoSave: true });
  return storePromise;
}

export type Settings = {
  apiKey: string;
  model: string;
  delayMs: number;
  jitterMs: number;
  countdownSec: number;
  humanMode: boolean;
  systemPrompt: string;
};

export const DEFAULT_SETTINGS: Settings = {
  apiKey: "",
  model: "gemini-2.5-flash",
  delayMs: 35,   // ↑ reliable for browser-side typing; lower = corruption risk
  jitterMs: 10,
  countdownSec: 3,
  humanMode: false,
  systemPrompt: "",
};

export async function loadSettings(): Promise<Settings> {
  const s = await getStore();
  const merged: Settings = { ...DEFAULT_SETTINGS };
  for (const key of Object.keys(DEFAULT_SETTINGS) as (keyof Settings)[]) {
    const v = await s.get(key as string);
    if (v !== undefined && v !== null) (merged as any)[key] = v;
  }
  // One-time migration: the original default (18 ms) corrupted typing in browsers.
  if (merged.delayMs < 25) {
    merged.delayMs = 35;
    await s.set("delayMs", 35);
    await s.save();
  }
  return merged;
}

export async function saveSettings(patch: Partial<Settings>): Promise<void> {
  const s = await getStore();
  for (const [k, v] of Object.entries(patch)) {
    await s.set(k, v as any);
  }
  await s.save();
  await emit("settings://updated");
}

export function onSettingsUpdated(cb: () => void): Promise<UnlistenFn> {
  return listen("settings://updated", () => cb());
}

// ───── Sessions ─────────────────────────────────────────────

export type StoredImage = { mimeType: string; dataBase64: string };
export type StoredTurn = {
  id: string;
  question: string;
  images: StoredImage[];      // raw base64, no preview URL
  answer: string;
  error?: string | null;
};
export type ChatSession = {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  turns: StoredTurn[];
};

const MAX_SESSIONS = 50;

export async function loadSessions(): Promise<ChatSession[]> {
  const s = await getStore();
  const raw = (await s.get("sessions")) as ChatSession[] | null | undefined;
  return Array.isArray(raw) ? raw : [];
}

export async function saveSession(session: ChatSession): Promise<void> {
  const s = await getStore();
  const list = await loadSessions();
  const idx = list.findIndex((x) => x.id === session.id);
  if (idx >= 0) list[idx] = session;
  else list.unshift(session);
  list.sort((a, b) => b.updatedAt - a.updatedAt);
  await s.set("sessions", list.slice(0, MAX_SESSIONS));
  await s.save();
}

export async function deleteSession(id: string): Promise<void> {
  const s = await getStore();
  const list = await loadSessions();
  await s.set("sessions", list.filter((x) => x.id !== id));
  await s.save();
}

// ───── Gemini ─────────────────────────────────────────────

export async function askGemini(args: {
  requestId: string;
  apiKey: string;
  model: string;
  history: ChatMessage[];
  prompt: string;
  images: { mimeType: string; dataBase64: string }[];
  systemPrompt?: string;
}) {
  return invoke<void>("ask_gemini", {
    requestId: args.requestId,
    apiKey: args.apiKey,
    model: args.model,
    history: args.history,
    prompt: args.prompt,
    images: args.images,
    systemPrompt: args.systemPrompt ?? "",
  });
}

// ── Screenshots ──
export type CapturedImage = { mime_type: string; data_base64: string };
export async function screenshotFull(): Promise<CapturedImage> {
  return invoke<CapturedImage>("screenshot_full");
}
export async function screenshotRegion(): Promise<void> {
  return invoke<void>("screenshot_region");
}
export function onScreenshotCaptured(cb: (p: CapturedImage) => void): Promise<UnlistenFn> {
  return listen<CapturedImage>("screenshot://captured", (e) => cb(e.payload));
}
export function onScreenshotError(cb: (msg: string) => void): Promise<UnlistenFn> {
  return listen<string>("screenshot://error", (e) => cb(e.payload));
}

// ── Window movement / sizing ──
export async function moveOverlay(dx: number, dy: number) {
  return invoke<void>("move_overlay", { dx, dy });
}
export async function resizeOverlay(width: number, height: number) {
  return invoke<void>("resize_overlay", { width, height });
}

// ───── Typing ─────────────────────────────────────────────

export async function typeText(
  text: string,
  delayMs: number,
  jitterMs: number,
  human: boolean,
) {
  return invoke<void>("type_text", { text, delayMs, jitterMs, human });
}
export async function cancelTyping() { return invoke<void>("cancel_typing"); }
export async function pauseTyping()  { return invoke<void>("pause_typing");  }
export async function resumeTyping() { return invoke<void>("resume_typing"); }

export type TypingProgress = {
  current: number;
  total: number;
  paused: boolean;
  done: boolean;
  error?: string | null;
};
export function onTypingProgress(cb: (p: TypingProgress) => void): Promise<UnlistenFn> {
  return listen<TypingProgress>("typing://progress", (e) => cb(e.payload));
}

// ───── Misc ─────────────────────────────────────────────

export async function toggleOverlay() { return invoke<void>("toggle_overlay"); }
export async function showMain() { return invoke<void>("show_main"); }
export async function applyStealth() { return invoke<void>("apply_stealth"); }
export async function quitApp() { return invoke<void>("quit_app"); }

export type ChunkPayload = { request_id: string; text: string };
export type DonePayload = { request_id: string; error: string | null };
export function onGeminiChunk(cb: (p: ChunkPayload) => void): Promise<UnlistenFn> {
  return listen<ChunkPayload>("gemini://chunk", (e) => cb(e.payload));
}
export function onGeminiDone(cb: (p: DonePayload) => void): Promise<UnlistenFn> {
  return listen<DonePayload>("gemini://done", (e) => cb(e.payload));
}
export function onOverlaySubmit(cb: () => void): Promise<UnlistenFn> {
  return listen("overlay://submit", () => cb());
}
export function onOverlayFocusInput(cb: () => void): Promise<UnlistenFn> {
  return listen("overlay://focus-input", () => cb());
}

export function fileToBase64(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const comma = result.indexOf(",");
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}
