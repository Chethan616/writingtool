<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { fade, slide, fly } from "svelte/transition";
  import { quintOut, cubicOut } from "svelte/easing";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open as openExternal } from "@tauri-apps/plugin-shell";
  import { marked } from "marked";
  import Icon from "./Icon.svelte";
  import {
    askGemini,
    cancelTyping,
    pauseTyping,
    resumeTyping,
    fileToBase64,
    loadSettings,
    saveSettings,
    loadSessions,
    saveSession,
    deleteSession,
    moveOverlay,
    resizeOverlay,
    onGeminiChunk,
    onGeminiDone,
    onOverlayFocusInput,
    onOverlaySubmit,
    onSettingsUpdated,
    onTypingProgress,
    onScreenshotCaptured,
    onScreenshotError,
    screenshotFull,
    screenshotRegion,
    typeText,
    type ChatMessage,
    type ImageAttachment,
    type Settings,
    type ChatSession,
    type StoredTurn,
  } from "./api";

  type CodeBlock = { lang: string; text: string };
  type ImgItem = { mimeType: string; dataBase64: string; previewUrl: string; kind: "image" | "audio" };
  type Turn = {
    id: string;
    question: string;
    images: ImgItem[];
    answer: string;
    streaming: boolean;
    error?: string | null;
  };

  // ───── models registry (shared with Settings) ─────
  const MODELS: { id: string; label: string; sub: string }[] = [
    { id: "gemini-2.5-flash",      label: "Flash",      sub: "Fast · vision" },
    { id: "gemini-2.5-pro",        label: "Pro",        sub: "Deepest" },
    { id: "gemini-2.5-flash-lite", label: "Flash Lite", sub: "Cheapest" },
  ];
  function shortModel(id: string | undefined): string {
    const m = MODELS.find((x) => x.id === id);
    return m ? m.label : (id?.replace("gemini-2.5-", "") ?? "...");
  }

  // ───── state ─────
  let settings = $state<Settings | null>(null);
  let input = $state("");
  let turns = $state<Turn[]>([]);
  let activeRequest = $state<string | null>(null);
  let inputEl: HTMLInputElement | undefined = $state();
  let scrollEl: HTMLElement | undefined = $state();
  let shellEl: HTMLElement | undefined = $state();
  let attachments = $state<ImgItem[]>([]);
  let dragOver = $state(false);

  // ── popovers ──
  let modelMenuOpen = $state(false);
  let shotMenuOpen = $state(false);

  // ── voice recording ──
  let recording = $state(false);
  let recordCancelled = false;
  let mediaRecorder: MediaRecorder | null = null;
  let recordChunks: Blob[] = [];
  let recordStartedAt = $state(0);
  let recordSeconds = $state(0);
  let recordTimer: ReturnType<typeof setInterval> | null = null;
  const WAVE_BINS = 22;
  const WAVE_CENTER = (WAVE_BINS - 1) / 2;
  function waveShape(i: number): number {
    const d = Math.abs(i - WAVE_CENTER) / WAVE_CENTER;
    return 0.45 + 0.55 * (1 - d * d);
  }
  let amplitudes = $state<number[]>(Array(WAVE_BINS).fill(0));
  let peak = $state(0);
  let audioCtx: AudioContext | null = null;
  let analyser: AnalyserNode | null = null;
  let mediaStream: MediaStream | null = null;
  let rafId: number | null = null;

  let historyOpen = $state(false);
  let sessions = $state<ChatSession[]>([]);
  let currentSessionId = $state<string | null>(null);

  // ── write/typing toolbar state ──
  // idle → countdown → typing → (paused) → resuming(3s) → typing → done
  type WritePhase = "idle" | "countdown" | "typing" | "resuming";
  let writePhase = $state<WritePhase>("idle");
  let countdownSec = $state(0);
  let countdownStartSec = $state(3);
  let typingCur = $state(0);
  let typingTotal = $state(0);
  let typingPaused = $state(false);
  let countdownTimer: ReturnType<typeof setInterval> | null = null;
  let resumeTimer: ReturnType<typeof setInterval> | null = null;
  let resumeSec = $state(0);
  const RESUME_DELAY = 3;
  let pendingTypeText = $state("");

  const cancelled = new Set<string>();
  const unlisteners: Array<() => void> = [];

  // ───── lifecycle ─────
  onMount(async () => {
    settings = await loadSettings();
    sessions = await loadSessions();

    const win = getCurrentWindow();
    const unFocus = await win.onFocusChanged(({ payload: focused }) => {
      if (focused && writePhase === "typing" && !typingPaused) {
        void pauseTyping();
      }
    });
    unlisteners.push(unFocus);

    unlisteners.push(await onGeminiChunk((p) => {
      if (cancelled.has(p.request_id)) return;
      const t = turns.find((x) => x.id === p.request_id);
      if (t) t.answer += p.text;
      scrollToBottom();
    }));

    unlisteners.push(await onGeminiDone(async (p) => {
      const t = turns.find((x) => x.id === p.request_id);
      if (t) {
        t.streaming = false;
        if (!cancelled.has(p.request_id)) t.error = p.error;
      }
      if (activeRequest === p.request_id) activeRequest = null;
      cancelled.delete(p.request_id);
      await autoSave();
    }));

    unlisteners.push(await onOverlaySubmit(() => void send()));
    unlisteners.push(await onOverlayFocusInput(async () => {
      settings = await loadSettings();
      sessions = await loadSessions();
      await tick();
      inputEl?.focus();
    }));
    unlisteners.push(await onSettingsUpdated(async () => {
      settings = await loadSettings();
    }));

    unlisteners.push(await onScreenshotCaptured((p) => {
      const previewUrl = `data:${p.mime_type};base64,${p.data_base64}`;
      attachments = [...attachments, {
        mimeType: p.mime_type,
        dataBase64: p.data_base64,
        previewUrl,
        kind: "image",
      }];
      void getCurrentWindow().show();
      void getCurrentWindow().setFocus();
    }));

    unlisteners.push(await onScreenshotError((msg) => {
      console.error("screenshot:", msg);
    }));

    unlisteners.push(await onTypingProgress((p) => {
      typingCur = p.current;
      typingTotal = p.total;
      typingPaused = p.paused;
      if (p.done) writePhase = "idle";
      else if (writePhase === "countdown" || writePhase === "resuming") {
        // keep current phase — user will see countdown finish
      } else {
        writePhase = "typing";
      }
    }));

    setTimeout(() => inputEl?.focus(), 50);

    // ── Dynamic window size: shrink/grow to content. ──
    if (shellEl && typeof ResizeObserver !== "undefined") {
      const ro = new ResizeObserver((entries) => {
        for (const e of entries) {
          const rect = e.target.getBoundingClientRect();
          const w = Math.ceil(rect.width);
          const h = Math.ceil(rect.height);
          if (w > 0 && h > 0) void resizeOverlay(w, h);
        }
      });
      ro.observe(shellEl);
      unlisteners.push(() => ro.disconnect());
    }

    // Close popovers when clicking elsewhere
    const onDocClick = (ev: MouseEvent) => {
      const t = ev.target as HTMLElement | null;
      if (!t) return;
      if (!t.closest("[data-popover='model']")) modelMenuOpen = false;
      if (!t.closest("[data-popover='shot']"))  shotMenuOpen  = false;
    };
    document.addEventListener("mousedown", onDocClick, true);
    unlisteners.push(() => document.removeEventListener("mousedown", onDocClick, true));
  });

  onDestroy(() => {
    unlisteners.forEach((u) => u());
    revokeAllPreviews();
    if (countdownTimer) clearInterval(countdownTimer);
    if (resumeTimer) clearInterval(resumeTimer);
  });

  function revokeAllPreviews() {
    for (const a of attachments) if (a.previewUrl) URL.revokeObjectURL(a.previewUrl);
    for (const t of turns) for (const i of t.images) URL.revokeObjectURL(i.previewUrl);
  }

  async function scrollToBottom() {
    await tick();
    scrollEl?.scrollTo({ top: scrollEl.scrollHeight, behavior: "smooth" });
  }

  function newRequestId() {
    return Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
  }

  // ───── send / cancel ─────
  async function send() {
    if (activeRequest) return;
    const prompt = input.trim();
    if (!prompt && attachments.length === 0) return;

    settings = await loadSettings();
    if (!settings.apiKey) {
      turns.push({
        id: newRequestId(),
        question: prompt || "(image)",
        images: [],
        answer: "",
        streaming: false,
        error: "Set your Gemini API key in Settings first.",
      });
      input = "";
      await scrollToBottom();
      return;
    }

    const history: ChatMessage[] = turns
      .filter((t) => !t.error && t.answer)
      .flatMap((t) => [
        { role: "user" as const, text: t.question },
        { role: "model" as const, text: t.answer },
      ]);

    const id = newRequestId();
    const submittedImages = attachments;
    turns.push({
      id,
      question: prompt,
      images: submittedImages,
      answer: "",
      streaming: true,
      error: null,
    });
    activeRequest = id;
    input = "";
    attachments = [];
    await scrollToBottom();

    try {
      await askGemini({
        requestId: id,
        apiKey: settings.apiKey,
        model: settings.model,
        history,
        prompt,
        images: submittedImages.map((a) => ({
          mimeType: a.mimeType,
          dataBase64: a.dataBase64,
        })),
        systemPrompt: settings.systemPrompt,
      });
    } catch (e) {
      const t = turns.find((x) => x.id === id);
      if (t) { t.streaming = false; t.error = String(e); }
      activeRequest = null;
    }
  }

  function stopGeneration() {
    if (!activeRequest) return;
    cancelled.add(activeRequest);
    const t = turns.find((x) => x.id === activeRequest);
    if (t) t.streaming = false;
    activeRequest = null;
  }

  // ───── sessions / new chat ─────
  async function autoSave() {
    if (turns.length === 0) return;
    const firstQ = turns[0].question || "(image)";
    const title = firstQ.slice(0, 60).replace(/\s+/g, " ").trim() || "untitled";
    if (!currentSessionId) currentSessionId = newRequestId();
    const stored: StoredTurn[] = turns.map((t) => ({
      id: t.id,
      question: t.question,
      images: t.images.map((i) => ({ mimeType: i.mimeType, dataBase64: i.dataBase64 })),
      answer: t.answer,
      error: t.error,
    }));
    const existing = sessions.find((s) => s.id === currentSessionId);
    const session: ChatSession = {
      id: currentSessionId,
      title,
      createdAt: existing?.createdAt ?? Date.now(),
      updatedAt: Date.now(),
      turns: stored,
    };
    await saveSession(session);
    sessions = await loadSessions();
  }

  async function newChat() {
    if (activeRequest) stopGeneration();
    if (writePhase !== "idle") cancelWrite();
    revokeAllPreviews();
    turns = [];
    input = "";
    attachments = [];
    currentSessionId = null;
    setTimeout(() => inputEl?.focus(), 30);
  }

  async function loadSessionById(sid: string) {
    const s = sessions.find((x) => x.id === sid);
    if (!s) return;
    revokeAllPreviews();
    turns = s.turns.map((t) => ({
      id: t.id,
      question: t.question,
      images: t.images.map((i) => ({
        mimeType: i.mimeType,
        dataBase64: i.dataBase64,
        previewUrl: dataUrl(i.mimeType, i.dataBase64),
      })),
      answer: t.answer,
      streaming: false,
      error: t.error ?? null,
    }));
    currentSessionId = s.id;
    historyOpen = false;
    await scrollToBottom();
  }

  function dataUrl(mime: string, b64: string) {
    return `data:${mime};base64,${b64}`;
  }

  async function removeSession(sid: string, e?: Event) {
    e?.stopPropagation();
    await deleteSession(sid);
    sessions = await loadSessions();
    if (currentSessionId === sid) {
      currentSessionId = null;
      turns = [];
    }
  }

  function relativeTime(ts: number) {
    const diff = Date.now() - ts;
    const m = Math.floor(diff / 60_000);
    if (m < 1) return "just now";
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    const d = Math.floor(h / 24);
    return `${d}d ago`;
  }

  // ───── attachments ─────
  async function attachBlob(blob: Blob) {
    if (!blob.type.startsWith("image/")) return;
    const dataBase64 = await fileToBase64(blob);
    const previewUrl = URL.createObjectURL(blob);
    attachments = [...attachments, { mimeType: blob.type, dataBase64, previewUrl, kind: "image" }];
  }

  // ── voice recording ──
  async function toggleRecording() {
    if (recording) { stopRecording(); return; }
    try {
      mediaStream = await navigator.mediaDevices.getUserMedia({ audio: true });

      const Ctor = (window.AudioContext || (window as any).webkitAudioContext) as typeof AudioContext;
      audioCtx = new Ctor();
      analyser = audioCtx.createAnalyser();
      analyser.fftSize = 128;
      analyser.smoothingTimeConstant = 0.72;
      const src = audioCtx.createMediaStreamSource(mediaStream);
      src.connect(analyser);
      const freqBuf = new Uint8Array(analyser.frequencyBinCount);
      const startBin = 1;
      const endBin = Math.min(freqBuf.length - 1, Math.floor(freqBuf.length * 0.55));
      const usable = endBin - startBin;
      const smoothed = new Array(WAVE_BINS).fill(0);
      const tickViz = () => {
        if (!analyser || !recording) return;
        analyser.getByteFrequencyData(freqBuf);
        let peakNext = 0;
        const next: number[] = new Array(WAVE_BINS);
        for (let i = 0; i < WAVE_BINS; i++) {
          const lo = startBin + Math.floor((i       / WAVE_BINS) * usable);
          const hi = startBin + Math.floor(((i + 1) / WAVE_BINS) * usable);
          let sum = 0;
          for (let k = lo; k <= hi; k++) sum += freqBuf[k];
          let v = sum / Math.max(1, hi - lo + 1) / 255;
          v = Math.pow(v, 0.75);
          smoothed[i] = v > smoothed[i] ? v : smoothed[i] * 0.78 + v * 0.22;
          next[i] = smoothed[i];
          if (smoothed[i] > peakNext) peakNext = smoothed[i];
        }
        amplitudes = next;
        peak = peak > peakNext ? peak * 0.85 + peakNext * 0.15 : peakNext;
        rafId = requestAnimationFrame(tickViz);
      };

      const mime = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
        ? "audio/webm;codecs=opus"
        : "audio/webm";
      const mr = new MediaRecorder(mediaStream, { mimeType: mime });
      recordChunks = [];
      recordCancelled = false;
      mr.ondataavailable = (e) => { if (e.data.size > 0) recordChunks.push(e.data); };
      mr.onstop = async () => {
        const wasCancelled = recordCancelled;
        teardownRecording();
        if (wasCancelled) {
          recording = false;
          recordSeconds = 0;
          recordCancelled = false;
          return;
        }
        const blob = new Blob(recordChunks, { type: "audio/webm" });
        if (blob.size === 0) { recording = false; return; }
        const dataBase64 = await fileToBase64(blob);
        attachments = [...attachments, {
          mimeType: "audio/webm",
          dataBase64,
          previewUrl: "",
          kind: "audio",
        }];
        recording = false;
        recordSeconds = 0;
        setTimeout(() => inputEl?.focus(), 30);
      };
      mediaRecorder = mr;
      mr.start();
      recording = true;
      recordStartedAt = Date.now();
      recordSeconds = 0;
      recordTimer = setInterval(() => {
        recordSeconds = Math.floor((Date.now() - recordStartedAt) / 1000);
        if (recordSeconds >= 120) stopRecording();
      }, 200);
      tickViz();
    } catch (e) {
      console.error("mic permission denied or unavailable:", e);
      teardownRecording();
      recording = false;
    }
  }

  function stopRecording() {
    if (mediaRecorder && mediaRecorder.state !== "inactive") {
      mediaRecorder.stop();
    }
  }

  function cancelRecording() {
    recordCancelled = true;
    stopRecording();
  }

  function teardownRecording() {
    if (rafId !== null) { cancelAnimationFrame(rafId); rafId = null; }
    if (recordTimer)    { clearInterval(recordTimer); recordTimer = null; }
    if (mediaStream)    { mediaStream.getTracks().forEach((t) => t.stop()); mediaStream = null; }
    if (audioCtx)       { try { audioCtx.close(); } catch { /* noop */ } audioCtx = null; }
    analyser = null;
    amplitudes = Array(WAVE_BINS).fill(0);
    peak = 0;
  }

  function fmtSecs(s: number) {
    const m = Math.floor(s / 60);
    const r = s % 60;
    return `${m}:${r.toString().padStart(2, "0")}`;
  }

  function removeAttachment(idx: number) {
    const a = attachments[idx];
    if (a?.previewUrl) URL.revokeObjectURL(a.previewUrl);
    attachments = attachments.filter((_, i) => i !== idx);
  }

  async function onPaste(e: ClipboardEvent) {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (const item of items) {
      if (item.kind === "file" && item.type.startsWith("image/")) {
        e.preventDefault();
        const f = item.getAsFile();
        if (f) await attachBlob(f);
      }
    }
  }

  async function onDrop(e: DragEvent) {
    e.preventDefault();
    dragOver = false;
    const files = e.dataTransfer?.files;
    if (!files) return;
    for (const f of files) await attachBlob(f);
  }

  // ───── markdown ─────
  function extractCodeBlocks(text: string): CodeBlock[] {
    const blocks: CodeBlock[] = [];
    const re = /```([a-zA-Z0-9_+-]*)\n([\s\S]*?)```/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      blocks.push({ lang: m[1] || "code", text: m[2] });
    }
    return blocks;
  }
  function firstCodeBlockOrAll(text: string): string {
    const b = extractCodeBlocks(text);
    if (b.length > 0) return b[0].text.replace(/\s+$/, "");
    return text;
  }
  function renderMd(text: string): string {
    marked.setOptions({ gfm: true, breaks: true });
    return marked.parse(text) as string;
  }

  function onAnswerClick(e: MouseEvent) {
    const a = (e.target as HTMLElement | null)?.closest("a");
    if (!a) return;
    const href = a.getAttribute("href");
    if (!href || href.startsWith("#")) return;
    e.preventDefault();
    void openExternal(href).catch(() => {});
  }

  // ───── write code (countdown -> typing) ─────
  async function startWrite(text: string) {
    if (writePhase !== "idle" || !settings) return;
    pendingTypeText = text;
    const sec = Math.max(0, settings.countdownSec);
    if (sec === 0) {
      writePhase = "typing";
      await typeText(text, settings.delayMs, settings.jitterMs, settings.humanMode);
      return;
    }
    writePhase = "countdown";
    countdownStartSec = sec;
    countdownSec = sec;
    countdownTimer = setInterval(async () => {
      countdownSec -= 1;
      if (countdownSec <= 0) {
        if (countdownTimer) clearInterval(countdownTimer);
        countdownTimer = null;
        if (writePhase !== "countdown") return;
        writePhase = "typing";
        if (!settings) return;
        try { await typeText(pendingTypeText, settings.delayMs, settings.jitterMs, settings.humanMode); }
        catch (e) { console.error(e); writePhase = "idle"; }
      }
    }, 1000);
  }

  function cancelWrite() {
    if (countdownTimer) { clearInterval(countdownTimer); countdownTimer = null; }
    if (resumeTimer)    { clearInterval(resumeTimer);    resumeTimer    = null; }
    if (writePhase === "typing" || writePhase === "resuming") void cancelTyping();
    writePhase = "idle";
    countdownSec = 0;
    resumeSec = 0;
  }

  function togglePause() {
    if (writePhase === "typing" && !typingPaused) {
      void pauseTyping();
      return;
    }
    if ((writePhase === "typing" && typingPaused) || writePhase === "resuming") {
      // Start a 3s resume countdown so the user can refocus their target field.
      if (writePhase === "resuming") return; // already counting
      writePhase = "resuming";
      resumeSec = RESUME_DELAY;
      if (resumeTimer) clearInterval(resumeTimer);
      resumeTimer = setInterval(() => {
        resumeSec -= 1;
        if (resumeSec <= 0) {
          if (resumeTimer) { clearInterval(resumeTimer); resumeTimer = null; }
          writePhase = "typing";
          void resumeTyping();
        }
      }, 1000);
    }
  }

  function cancelResume() {
    if (resumeTimer) { clearInterval(resumeTimer); resumeTimer = null; }
    resumeSec = 0;
    writePhase = "typing"; // still paused
  }

  async function hide() { await getCurrentWindow().hide(); }

  // ───── keyboard ─────
  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (modelMenuOpen) { modelMenuOpen = false; return; }
      if (shotMenuOpen)  { shotMenuOpen  = false; return; }
      if (writePhase === "resuming") { cancelResume(); return; }
      if (writePhase !== "idle") { cancelWrite(); return; }
      if (activeRequest) { stopGeneration(); return; }
      if (historyOpen) { historyOpen = false; return; }
      void hide();
      return;
    }
    const mod = e.ctrlKey || e.metaKey;
    if (!mod) {
      if (e.key === "Enter" && !e.shiftKey && document.activeElement === inputEl) {
        e.preventDefault();
        void send();
      }
      return;
    }
    const step = e.shiftKey ? 80 : 30;
    if (e.key === "ArrowLeft")  { e.preventDefault(); void moveOverlay(-step, 0);  return; }
    if (e.key === "ArrowRight") { e.preventDefault(); void moveOverlay( step, 0);  return; }
    if (e.key === "ArrowUp")    { e.preventDefault(); void moveOverlay(0, -step);  return; }
    if (e.key === "ArrowDown")  { e.preventDefault(); void moveOverlay(0,  step);  return; }
    switch (e.key.toLowerCase()) {
      case "n": e.preventDefault(); newChat(); break;
      case "h": e.preventDefault(); historyOpen = !historyOpen; break;
      case "p":
        if ((writePhase === "typing" || writePhase === "resuming") && e.shiftKey) {
          e.preventDefault(); togglePause();
        }
        break;
      case ".": e.preventDefault(); if (activeRequest) stopGeneration(); break;
    }
  }

  // ───── export ─────
  function exportMarkdown(): string {
    const lines: string[] = [];
    lines.push(`# Writing Agent chat — ${new Date().toLocaleString()}\n`);
    for (const t of turns) {
      lines.push(`## You\n\n${t.question || "_(image only)_"}\n`);
      if (t.error) lines.push(`> Error: ${t.error}\n`);
      else if (t.answer) lines.push(`## Gemini\n\n${t.answer}\n`);
    }
    return lines.join("\n");
  }
  async function copyChatMarkdown() {
    if (turns.length === 0) return;
    try { await navigator.clipboard.writeText(exportMarkdown()); } catch (e) { console.error(e); }
  }
  function downloadChatMarkdown() {
    if (turns.length === 0) return;
    const md = exportMarkdown();
    const blob = new Blob([md], { type: "text/markdown;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
    a.download = `writing-agent-chat-${stamp}.md`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    setTimeout(() => URL.revokeObjectURL(url), 1500);
  }

  // ── model picker ──
  async function selectModel(id: string) {
    if (!settings) return;
    settings = { ...settings, model: id };
    modelMenuOpen = false;
    await saveSettings({ model: id });
  }

  // ── screenshot helpers ──
  async function fullScreenshot() {
    shotMenuOpen = false;
    try { await screenshotFull(); } catch (e) { console.error(e); }
  }
  async function regionScreenshot() {
    shotMenuOpen = false;
    try { await screenshotRegion(); } catch (e) { console.error(e); }
  }

  // ───── derived ─────
  const typingPct = $derived(typingTotal > 0 ? Math.min(100, Math.round((typingCur / typingTotal) * 100)) : 0);
  const countdownPct = $derived(countdownStartSec > 0
    ? Math.max(0, Math.min(100, ((countdownStartSec - countdownSec) / countdownStartSec) * 100))
    : 0);
  const resumePct = $derived(RESUME_DELAY > 0
    ? Math.max(0, Math.min(100, ((RESUME_DELAY - resumeSec) / RESUME_DELAY) * 100))
    : 0);
</script>

<svelte:window onkeydown={onKey} onpaste={onPaste} />

<div
  bind:this={shellEl}
  class="shell relative flex flex-col items-start gap-2 p-2 select-none"
  style="width: {historyOpen ? '828px' : '580px'}; min-height: {(modelMenuOpen || shotMenuOpen) ? '320px' : 'auto'};"
  ondragover={(e) => { e.preventDefault(); dragOver = true; }}
  ondragleave={() => (dragOver = false)}
  ondrop={onDrop}
  role="region"
  aria-label="Overlay"
>
  <!-- ════ Pill bar (taller, narrower) ════ -->
  <div
    class="pill-glass relative flex items-center gap-2 rounded-2xl px-3.5 py-3 drag {recording ? 'recording-ring' : ''}"
    style="width: 564px; {recording ? `--rec-peak: ${peak.toFixed(3)};` : ''}"
  >
    <span class="h-1.5 w-1.5 shrink-0 rounded-full {recording ? 'dot-error' : (activeRequest ? 'dot-warn' : 'dot-ok')}"></span>

    <div class="no-drag flex flex-1 min-w-0 items-center gap-2">
      {#each attachments as a, i (i + ":" + (a.previewUrl || a.kind))}
        {#if a.kind === "audio"}
          <div class="relative flex h-7 shrink-0 items-center gap-1.5 rounded-lg border border-white/15 bg-white/8 px-2 text-[11px] text-white/85">
            <Icon name="mic" size={12} />
            <span class="tabular-nums">audio</span>
            <button class="ml-1 grid h-4 w-4 place-items-center rounded-full bg-zinc-900/90 text-[10px] leading-none text-white ring-1 ring-white/15" onclick={() => removeAttachment(i)} title="Remove">×</button>
          </div>
        {:else}
          <div class="relative h-7 w-7 shrink-0 overflow-hidden rounded-lg border border-white/15 shadow-lg">
            <img src={a.previewUrl} alt="attachment" class="h-full w-full object-cover" />
            <button class="absolute -right-1 -top-1 grid h-4 w-4 place-items-center rounded-full bg-zinc-900/90 text-[10px] leading-none text-white shadow-md ring-1 ring-white/20" onclick={() => removeAttachment(i)} title="Remove">×</button>
          </div>
        {/if}
      {/each}

      {#if recording}
        <div class="flex flex-1 min-w-0 items-center gap-2 record-fade">
          <div class="wave-stage" aria-hidden="true">
            {#each amplitudes as a, i}
              {@const h = waveShape(i) * (0.18 + 0.82 * a)}
              <span
                class="wave-bar"
                style="
                  --h: {h.toFixed(3)};
                  --shape: {waveShape(i).toFixed(3)};
                  --idx: {i};
                "
              ></span>
            {/each}
          </div>
          <span class="rec-time tabular-nums" title="elapsed">
            {fmtSecs(recordSeconds)}
          </span>
          <button
            class="no-drag rec-cancel"
            onclick={cancelRecording}
            title="Cancel recording (discard)"
            aria-label="Cancel recording"
          >
            <Icon name="close" size={11} />
          </button>
        </div>
      {:else}
        <input
          bind:this={inputEl}
          bind:value={input}
          placeholder={attachments.length > 0 ? "Optional question…" : "Ask Gemini…"}
          class="flex-1 min-w-0 bg-transparent text-[13.5px] placeholder:text-white/40 outline-none"
        />
      {/if}
    </div>

    <!-- Model picker chip (toggles inline drawer) -->
    <button
      class="no-drag chip flex items-center gap-1 px-2 py-1 text-[10px] font-medium uppercase tracking-wider hover:bg-white/12 transition {modelMenuOpen ? 'is-open' : ''}"
      data-popover="model"
      onclick={() => { modelMenuOpen = !modelMenuOpen; shotMenuOpen = false; }}
      aria-haspopup="menu"
      aria-expanded={modelMenuOpen}
      title="Switch model"
    >
      <span>{shortModel(settings?.model)}</span>
      <Icon name={modelMenuOpen ? "chevronUp" : "chevronDown"} size={10} />
    </button>

    {#if activeRequest}
      <button class="no-drag btn-danger rounded-full px-3 py-1.5 text-xs" onclick={stopGeneration} title="Stop (Esc / Ctrl+.)">
        <Icon name="stop" size={11} fill="currentColor" /> Stop
      </button>
    {:else}
      <button class="no-drag btn-primary rounded-full px-3 py-1.5 text-xs" onclick={send} disabled={!input.trim() && attachments.length === 0} title="Ask Gemini  (Enter / Ctrl+Shift+Enter)">
        <Icon name="send" size={11} /> Ask
      </button>
    {/if}

    <!-- Screenshot split-button: main click = full, caret = drawer -->
    <div class="no-drag flex items-stretch" data-popover="shot">
      <button
        class="icon-btn"
        style="border-radius: 8px 0 0 8px; width: 26px;"
        onclick={fullScreenshot}
        title="Screenshot full screen  (Ctrl+Shift+S)"
        aria-label="Screenshot full screen"
      >
        <Icon name="monitor" size={13} />
      </button>
      <button
        class="split-caret {shotMenuOpen ? 'is-open' : ''}"
        onclick={() => { shotMenuOpen = !shotMenuOpen; modelMenuOpen = false; }}
        aria-haspopup="menu"
        aria-expanded={shotMenuOpen}
        title="More screenshot options"
      >
        <Icon name={shotMenuOpen ? "chevronUp" : "chevronDown"} size={10} />
      </button>
    </div>

    <button
      class="no-drag icon-btn {recording ? 'mic-recording' : ''}"
      onclick={toggleRecording}
      title={recording ? `Stop recording  (${fmtSecs(recordSeconds)})` : "Record voice"}
      aria-label="Voice"
      aria-pressed={recording}
    >
      {#if recording}
        <span class="rec-dot"></span>
      {:else}
        <Icon name="mic" size={13} />
      {/if}
    </button>

    <button class="no-drag icon-btn" class:active={historyOpen} onclick={() => (historyOpen = !historyOpen)} title="History  (Ctrl+H)" aria-label="History" aria-pressed={historyOpen}>
      <Icon name="history" size={13} />
    </button>
    <button class="no-drag icon-btn" onclick={newChat} title="New chat  (Ctrl+N)" aria-label="New chat">
      <Icon name="plus" size={13} />
    </button>
    <button class="no-drag icon-btn" onclick={hide} title="Hide  (Esc)" aria-label="Hide">
      <Icon name="close" size={13} />
    </button>
  </div>

  <!-- ════ Inline drawers (grow the window, never clip) ════ -->
  <!-- ════ Floating Dropdown Menus ════ -->
  {#if modelMenuOpen}
    <div
      class="float-glass body-panel rounded-2xl p-1.5 absolute z-50"
      style="top: 60px; right: 120px; width: 220px;"
      data-popover="model"
      transition:slide={{ duration: 200, easing: quintOut }}
    >
      <div class="px-2.5 pb-1 pt-1 text-[10px] font-medium uppercase tracking-[0.10em] text-white/50">Model</div>
      {#each MODELS as m}
        <button
          class="menu-item w-full {settings?.model === m.id ? 'is-active' : ''}"
          onclick={() => selectModel(m.id)}
        >
          <span class="grid h-4 w-4 place-items-center text-white/80">
            {#if settings?.model === m.id}<Icon name="check" size={12} />{/if}
          </span>
          <span class="flex-1 min-w-0 text-left">
            <span class="block leading-tight text-white">{m.label}</span>
            <span class="sub">{m.sub}</span>
          </span>
        </button>
      {/each}
    </div>
  {/if}

  {#if shotMenuOpen}
    <div
      class="float-glass body-panel rounded-2xl p-1.5 absolute z-50"
      style="top: 60px; right: 70px; width: 220px;"
      data-popover="shot"
      transition:slide={{ duration: 200, easing: quintOut }}
    >
      <div class="px-2.5 pb-1 pt-1 text-[10px] font-medium uppercase tracking-[0.10em] text-white/50">Screenshot</div>
      <button class="menu-item w-full" onclick={fullScreenshot}>
        <Icon name="monitor" size={13} />
        <span class="flex-1 min-w-0 text-left">
          <span class="block leading-tight text-white">Full screen</span>
          <span class="sub">Capture entire screen · Ctrl+Shift+S</span>
        </span>
      </button>
      <button class="menu-item w-full" onclick={regionScreenshot}>
        <Icon name="crop" size={13} />
        <span class="flex-1 min-w-0 text-left">
          <span class="block leading-tight text-white">Partial screenshot</span>
          <span class="sub">Drag to select region · Ctrl+Shift+R</span>
        </span>
      </button>
    </div>
  {/if}

  <!-- ════ Body: history + answers (Decoupled layout) ════ -->
  {#if historyOpen || turns.length > 0}
    <div
      class="flex gap-2 relative w-full"
      style="height: 380px;"
      transition:slide={{ duration: 240, easing: quintOut }}
    >
      {#if turns.length > 0}
        <main
          bind:this={scrollEl}
          class="panel-glass subtle-scroll relative overflow-y-auto p-4 shrink-0"
          style="width: 564px;"
          transition:fade={{ duration: 180 }}
        >
          <div class="absolute right-2 top-2 z-10 flex items-center gap-1">
            <button
              class="icon-btn"
              style="width: 24px; height: 24px;"
              onclick={copyChatMarkdown}
              title="Copy chat as Markdown"
              aria-label="Copy chat"
            >
              <Icon name="copy" size={12} />
            </button>
            <button
              class="icon-btn"
              style="width: 24px; height: 24px;"
              onclick={downloadChatMarkdown}
              title="Download chat as .md"
              aria-label="Download chat"
            >
              <Icon name="download" size={12} />
            </button>
          </div>
          {#each turns as turn (turn.id)}
            <article class="mb-5 last:mb-0 fade-in">
              <div class="mb-2 text-[11px] uppercase tracking-wide text-white/55">You asked</div>
              {#if turn.images.length > 0}
                <div class="mb-2 flex flex-wrap gap-2">
                  {#each turn.images as img}
                    <img src={img.previewUrl} alt="" class="h-20 rounded-md border border-white/10 object-cover" />
                  {/each}
                </div>
              {/if}
              {#if turn.question}
                <div class="mb-3 whitespace-pre-wrap text-sm" style="color: rgba(245,245,247,0.92);">{turn.question}</div>
              {/if}

              {#if turn.error}
                <div class="rounded-lg border border-red-400/50 bg-red-500/15 p-3 text-xs text-red-100">
                  {turn.error}
                </div>
              {:else}
                <div class="mb-2 flex items-center gap-2">
                  <span class="text-[11px] uppercase tracking-wide text-white/55">Gemini</span>
                  {#if turn.streaming}<span class="h-1 w-1 animate-pulse rounded-full bg-white/70"></span>{/if}
                </div>

                <div class="md-body text-sm leading-relaxed" style="color: #e8e8ee;" onclick={onAnswerClick} role="presentation">
                  {@html renderMd(turn.answer || "…")}
                </div>

                {#if !turn.streaming && turn.answer}
                  {@const blocks = extractCodeBlocks(turn.answer)}
                  <div class="mt-3 flex flex-wrap items-center gap-2">
                    {#if blocks.length > 0}
                      {#each blocks as b, i}
                        <button class="btn rounded-lg px-3 py-1.5 text-xs" onclick={() => startWrite(b.text)}>
                          <Icon name="keyboard" size={12} />
                          Write {blocks.length > 1 ? `block ${i + 1}` : "code"} · {b.lang}
                        </button>
                      {/each}
                    {:else}
                      <button class="btn rounded-lg px-3 py-1.5 text-xs" onclick={() => startWrite(firstCodeBlockOrAll(turn.answer))}>
                        <Icon name="keyboard" size={12} /> Write answer
                      </button>
                    {/if}
                    <button class="btn rounded-lg px-3 py-1.5 text-xs" onclick={() => navigator.clipboard.writeText(firstCodeBlockOrAll(turn.answer))}>
                      <Icon name="copy" size={12} /> Copy
                    </button>
                  </div>
                {/if}
              {/if}
            </article>
          {/each}
        </main>
      {/if}

      {#if historyOpen}
        <aside
          class="panel-glass history-panel flex flex-col overflow-hidden shrink-0"
          style="width: 240px; margin-left: auto;"
          transition:fly={{ x: 16, duration: 220, easing: cubicOut }}
        >
          <header class="flex items-center justify-between border-b border-white/8 px-3 py-2.5">
            <span class="text-[10.5px] font-medium uppercase tracking-[0.10em] text-white/55">History</span>
            <button
              class="icon-btn"
              style="width:22px;height:22px"
              onclick={() => (historyOpen = false)}
              aria-label="Close history"
              title="Close (Ctrl+H)"
            >
              <Icon name="chevronRight" size={12} />
            </button>
          </header>
          <div class="subtle-scroll flex-1 overflow-y-auto p-2 space-y-1">
            {#if sessions.length === 0}
              <div class="px-2 py-6 text-center text-xs text-white/45">
                No past chats yet.
              </div>
            {:else}
              {#each sessions as s (s.id)}
                <div class="group relative rounded-lg hover:bg-white/8 transition {currentSessionId === s.id ? 'bg-white/10' : ''}">
                  <button
                    class="flex w-full items-start gap-2 rounded-lg px-2 py-2 pr-9 text-left"
                    onclick={() => loadSessionById(s.id)}
                  >
                    <div class="mt-0.5 text-white/40"><Icon name="chat" size={13} /></div>
                    <div class="flex-1 min-w-0">
                      <div class="truncate text-[12.5px] text-white/90">{s.title}</div>
                      <div class="text-[10px] text-white/45">{relativeTime(s.updatedAt)} · {s.turns.length} turn{s.turns.length === 1 ? "" : "s"}</div>
                    </div>
                  </button>
                  <button
                    class="invisible absolute right-1.5 top-1.5 grid h-6 w-6 place-items-center rounded text-white/40 hover:bg-red-500/20 hover:text-red-200 group-hover:visible transition"
                    onclick={(e) => removeSession(s.id, e)}
                    title="Delete"
                    aria-label="Delete session"
                  >
                    <Icon name="trash" size={12} />
                  </button>
                </div>
              {/each}
            {/if}
          </div>
        </aside>
      {/if}

    </div>
  {/if}

  <!-- ════ Write/Typing panel — separated pill BELOW the overlay ════ -->
  {#if writePhase !== "idle"}
    <div
      class="float-glass body-panel flex items-center gap-3 rounded-2xl px-4 py-2.5 shrink-0"
      style="width: 564px;"
      transition:slide={{ duration: 220, easing: quintOut }}
    >
      {#if writePhase === "countdown"}
        <div class="relative" aria-hidden="true">
          <div class="resume-ring" style="--p: {countdownPct.toFixed(1)};">
            <span class="resume-num">{countdownSec}</span>
          </div>
        </div>
        <div class="flex-1 min-w-0 text-[12.5px] leading-tight">
          <div class="font-medium text-white">Writing in {countdownSec}s</div>
          <div class="mt-0.5 text-[11px] text-white/55">Click the target answer box now.</div>
        </div>
        <button class="btn rounded-md px-3 py-1.5 text-xs" onclick={cancelWrite}>Cancel</button>
      {:else if writePhase === "resuming"}
        <div class="relative" aria-hidden="true">
          <div class="resume-ring" style="--p: {resumePct.toFixed(1)};">
            <span class="resume-num">{resumeSec}</span>
          </div>
        </div>
        <div class="flex-1 min-w-0 text-[12.5px] leading-tight">
          <div class="font-medium text-white">Resuming in {resumeSec}s</div>
          <div class="mt-0.5 text-[11px] text-white/55">Put your cursor back in the target field.</div>
        </div>
        <button class="btn rounded-md px-3 py-1.5 text-xs" onclick={cancelResume} title="Stay paused">Hold</button>
        <button class="btn-danger rounded-md px-3 py-1.5 text-xs font-medium" onclick={cancelWrite}>Stop</button>
      {:else}
        <div class="flex items-center gap-2">
          <div class="grid h-8 w-8 place-items-center rounded-lg bg-white/8 text-white/85 border border-white/10">
            <Icon name={typingPaused ? "pause" : "keyboard"} size={13} fill={typingPaused ? "currentColor" : "none"} />
          </div>
        </div>
        <div class="flex-1 min-w-0 text-[12.5px] leading-tight">
          <div class="flex items-baseline gap-1.5">
            <span class="font-medium text-white">{typingPaused ? "Paused" : "Typing"}{settings?.humanMode ? " · human" : ""}</span>
            <span class="tabular-nums text-white/55">{typingCur}/{typingTotal}</span>
          </div>
          <div class="mt-1 h-[3px] w-full overflow-hidden rounded-full bg-white/8">
            <div class="h-full {typingPaused ? 'bg-white/40' : 'bg-white/85'} transition-[width] duration-200" style="width: {typingPct}%"></div>
          </div>
        </div>
        <button class="btn rounded-md px-3 py-1.5 text-xs" onclick={togglePause} title={typingPaused ? "Resume (Ctrl+Shift+P)" : "Pause (Ctrl+Shift+P)"}>
          <Icon name={typingPaused ? "play" : "pause"} size={11} fill="currentColor" />
          {typingPaused ? "Resume" : "Pause"}
        </button>
        <button class="btn-danger rounded-md px-3 py-1.5 text-xs font-medium" onclick={cancelWrite} title="Stop (Esc)">
          <Icon name="stop" size={11} fill="currentColor" />
          Stop
        </button>
      {/if}
    </div>
  {/if}

  <!-- ════ Drop hint ════ -->
  {#if dragOver}
    <div
      class="panel-glass pointer-events-none absolute inset-2 z-40 flex items-center justify-center border-2 border-dashed border-white/25 text-sm text-white/85"
      transition:fade={{ duration: 120 }}
    >
      <Icon name="image" size={18} />
      <span class="ml-2">Drop image to attach</span>
    </div>
  {/if}
</div>

<style>
  /* Shell width changes instantly so Tauri doesn't spam OS window resizes. */
  .shell {
    /* width transition removed to fix massive lag */
  }
  .md-body :global(pre) {
    background: rgba(0, 0, 0, 0.55);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 10px;
    padding: 10px 12px;
    margin: 0.6em 0;
    overflow-x: auto;
    color: rgb(232 232 235);
    font-family: "JetBrains Mono", "Cascadia Code", Consolas, Menlo, monospace;
    font-size: 12.5px;
    line-height: 1.55;
  }
  .md-body :global(code) {
    background: rgba(0, 0, 0, 0.4);
    padding: 1px 5px;
    border-radius: 4px;
    color: rgb(245 245 247);
    font-family: "JetBrains Mono", "Cascadia Code", Consolas, Menlo, monospace;
    font-size: 12.5px;
  }
  .md-body :global(pre code) { background: transparent; padding: 0; color: inherit; }
  .md-body :global(p) { margin: 0.4em 0; }
  .md-body :global(ul), .md-body :global(ol) { margin: 0.4em 0 0.4em 1.2em; }
  .md-body :global(li) { margin: 0.15em 0; }
  .md-body :global(strong) { color: white; }
  .md-body :global(a) { color: rgb(180 200 255); text-decoration: underline; }
</style>
