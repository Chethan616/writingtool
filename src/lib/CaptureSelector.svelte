<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  type CapturedImage = { mime_type: string; data_base64: string };

  let bgUrl = $state<string | null>(null);
  let dragging = $state(false);
  let startX = $state(0);
  let startY = $state(0);
  let curX = $state(0);
  let curY = $state(0);

  const rect = $derived({
    x: Math.min(startX, curX),
    y: Math.min(startY, curY),
    w: Math.abs(curX - startX),
    h: Math.abs(curY - startY),
  });

  let unlistenPrepare: UnlistenFn | null = null;

  /** Fetch the staged payload, paint it as the bg, then ask the backend to
   *  show the window. Called on first mount AND on every `capture://prepare`
   *  event (window is pre-created and reused). */
  async function prepare() {
    const payload = await invoke<CapturedImage | null>("get_pending_screenshot");
    if (!payload) {
      // No staged payload — nothing to do. Stay hidden.
      bgUrl = null;
      return;
    }
    const url = `data:${payload.mime_type};base64,${payload.data_base64}`;

    // Decode before paint so we never show a white frame.
    const img = new Image();
    img.src = url;
    try {
      await img.decode();
    } catch {
      await new Promise<void>((res) => {
        img.onload = () => res();
        img.onerror = () => res();
      });
    }

    // Reset selection state when reusing the window.
    dragging = false;
    startX = startY = curX = curY = 0;

    bgUrl = url;
    await tick();
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    await invoke("show_capture_window");
  }

  onMount(async () => {
    unlistenPrepare = await listen("capture://prepare", () => { void prepare(); });
    await prepare();
  });

  onDestroy(() => {
    if (unlistenPrepare) unlistenPrepare();
  });

  function onDown(e: PointerEvent) {
    dragging = true;
    startX = curX = e.clientX;
    startY = curY = e.clientY;
    (e.target as Element).setPointerCapture(e.pointerId);
  }
  function onMove(e: PointerEvent) {
    if (!dragging) return;
    curX = e.clientX;
    curY = e.clientY;
  }
  async function onUp(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    (e.target as Element).releasePointerCapture(e.pointerId);
    if (rect.w < 4 || rect.h < 4) {
      await invoke("screenshot_region_cancel");
      return;
    }
    try {
      await invoke("screenshot_region_finish", {
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.w),
        height: Math.round(rect.h),
        dpr: window.devicePixelRatio || 1,
      });
    } catch (err) {
      console.error(err);
      await invoke("screenshot_region_cancel");
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") void invoke("screenshot_region_cancel");
  }
</script>

<svelte:window onkeydown={onKey} />

{#if bgUrl}
  <div
    role="presentation"
    onpointerdown={onDown}
    onpointermove={onMove}
    onpointerup={onUp}
    class="fixed inset-0 select-none"
    style="background: url('{bgUrl}') center/cover no-repeat; background-size: 100vw 100vh; cursor: crosshair;"
  >
    <!-- Dimming layer with a "hole" cut to show selection clearly -->
    <div
      class="absolute inset-0"
      style="background: rgba(0, 0, 0, 0.45); clip-path: polygon(
        0 0, 100% 0, 100% 100%, 0 100%, 0 0,
        {rect.x}px {rect.y}px,
        {rect.x}px {rect.y + rect.h}px,
        {rect.x + rect.w}px {rect.y + rect.h}px,
        {rect.x + rect.w}px {rect.y}px,
        {rect.x}px {rect.y}px
      );"
    ></div>

    {#if dragging && rect.w > 0 && rect.h > 0}
      <div
        class="absolute pointer-events-none"
        style="
          left: {rect.x}px;
          top: {rect.y}px;
          width: {rect.w}px;
          height: {rect.h}px;
          border: 1.5px solid #fff;
          box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.4), 0 4px 20px rgba(0, 0, 0, 0.5);
        "
      >
        <div
          class="absolute -top-6 left-0 rounded bg-black/85 px-2 py-0.5 text-[11px] font-medium tabular-nums text-white"
          style="font-family: 'JetBrains Mono', Consolas, monospace;"
        >
          {Math.round(rect.w)} × {Math.round(rect.h)}
        </div>
      </div>
    {/if}

    <div class="pointer-events-none absolute bottom-6 left-1/2 -translate-x-1/2 rounded-full bg-black/75 px-4 py-2 text-[12px] text-white/90 backdrop-blur-sm">
      Drag to select &nbsp;·&nbsp; Esc to cancel
    </div>
  </div>
{/if}
