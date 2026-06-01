<script lang="ts">
  import { onMount } from "svelte";
  import { fade, fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import Icon from "./Icon.svelte";
  import {
    loadSettings,
    saveSettings,
    toggleOverlay,
    quitApp,
    DEFAULT_SETTINGS,
    type Settings,
  } from "./api";

  type Section = "api" | "typing" | "hotkeys" | "about";

  let settings: Settings = $state({ ...DEFAULT_SETTINGS });
  let active: Section = $state("api");
  let saving = $state(false);
  let savedFlash = $state(false);
  let revealKey = $state(false);
  let dirty = $state(false);
  let loaded = $state(false);

  onMount(async () => {
    settings = await loadSettings();
    // Land on API tab if no key set — gentle nudge.
    if (!settings.apiKey) active = "api";
    loaded = true;
  });

  $effect(() => {
    JSON.stringify(settings);
    if (loaded) dirty = true;
  });

  async function save() {
    if (saving) return;
    saving = true;
    await saveSettings(settings);
    saving = false;
    savedFlash = true;
    dirty = false;
    setTimeout(() => (savedFlash = false), 1400);
  }

  const SECTIONS: { id: Section; label: string; icon: string }[] = [
    { id: "api",     label: "API",     icon: "chat" },
    { id: "typing",  label: "Typing",  icon: "keyboard" },
    { id: "hotkeys", label: "Hotkeys", icon: "history" },
    { id: "about",   label: "About",   icon: "settings" },
  ];

  const HOTKEYS: { label: string; keys: string }[] = [
    { label: "Show / hide overlay (global)",    keys: "Ctrl Shift Space" },
    { label: "Ask Gemini (global)",             keys: "Ctrl Shift Enter" },
    { label: "Screenshot (full, global)",       keys: "Ctrl Shift S" },
    { label: "Screenshot (region, global)",     keys: "Ctrl Shift R" },
    { label: "Send question (focused)",         keys: "Enter" },
    { label: "Move overlay (focused, hold)",    keys: "Ctrl Arrow" },
    { label: "New chat",                        keys: "Ctrl N" },
    { label: "Toggle history",                  keys: "Ctrl H" },
    { label: "Pause / resume typing",           keys: "Ctrl Shift P" },
    { label: "Stop generating",                 keys: "Ctrl ." },
    { label: "Cancel / hide overlay",           keys: "Esc" },
  ];
</script>

<div class="app-bg flex h-full">
  <!-- ── Sidebar ── -->
  <nav class="flex w-44 shrink-0 flex-col p-4 pt-5" style="border-right: 1px solid rgba(255,255,255,0.05);">
    <div class="mb-7 flex items-center gap-2.5 px-1">
      <!-- Logo: black square with white centered dot -->
      <div class="grid h-8 w-8 place-items-center rounded-lg" style="background:#000; border:1px solid rgba(255,255,255,0.10);">
        <span class="block h-2 w-2 rounded-full bg-white"></span>
      </div>
      <div class="flex flex-col leading-tight">
        <span class="text-[13.5px] font-semibold text-white tracking-tight">Writing Agent</span>
        <span class="text-[10px] text-white/35">v0.1.0</span>
      </div>
    </div>

    {#each SECTIONS as s}
      <button
        class="mb-0.5 flex w-full items-center gap-2.5 rounded-md px-2.5 py-1.5 text-left text-[13px] transition
              {active === s.id
                ? 'bg-white/8 text-white'
                : 'text-white/55 hover:bg-white/4 hover:text-white/85'}"
        onclick={() => (active = s.id)}
      >
        <span class="opacity-75"><Icon name={s.icon} size={13} /></span>
        {s.label}
        {#if s.id === "api" && !settings.apiKey && loaded}
          <span class="ml-auto h-1.5 w-1.5 rounded-full bg-amber-400" aria-label="API key not set"></span>
        {/if}
      </button>
    {/each}

    <div class="flex-1"></div>

    <div class="space-y-0.5 pt-3" style="border-top: 1px solid rgba(255,255,255,0.05);">
      <button
        class="flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[12px] text-white/50 hover:bg-white/4 hover:text-white/80"
        onclick={() => toggleOverlay()}
      >
        <Icon name="keyboard" size={12} />
        Test overlay
      </button>
      <button
        class="flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[12px] text-white/40 hover:bg-white/4 hover:text-white/70"
        onclick={() => { if (confirm("Quit Writing Agent? Overlay will close too.")) quitApp(); }}
      >
        <Icon name="close" size={12} />
        Quit
      </button>
    </div>
  </nav>

  <!-- ── Content pane ── -->
  <main class="flex flex-1 flex-col min-w-0">
    <header class="flex items-center justify-between px-7 pt-6 pb-4" style="border-bottom: 1px solid rgba(255,255,255,0.04);">
      <h1 class="text-[18px] font-semibold leading-none text-white tracking-tight">
        {SECTIONS.find((s) => s.id === active)?.label}
      </h1>
      <div class="flex items-center gap-3">
        {#if savedFlash}
          <span class="text-[11px] text-emerald-300">Saved</span>
        {/if}
        <button
          class={dirty ? "btn-primary rounded-md px-4 py-1.5 text-[12.5px]" : "btn rounded-md px-4 py-1.5 text-[12.5px] opacity-40"}
          onclick={save}
          disabled={!dirty || saving}
        >
          {saving ? "Saving…" : "Save"}
        </button>
      </div>
    </header>

    <div class="subtle-scroll flex-1 overflow-y-auto px-7 pb-7 pt-5">
      {#key active}
        <div
          class="space-y-4"
          in:fly={{ y: 6, duration: 220, easing: cubicOut }}
          out:fade={{ duration: 90 }}
        >
      {#if active === "api"}
        {#if !settings.apiKey && loaded}
          <div class="card-glass p-4 text-[12px] text-white/85 flex items-start gap-2.5" style="border-color: rgba(245,158,11,0.30);">
            <span class="mt-0.5 h-1.5 w-1.5 shrink-0 rounded-full bg-amber-400 ring-2 ring-amber-400/20"></span>
            <div>
              <div class="mb-0.5 font-medium text-white">Paste your Gemini key to get started</div>
              <div class="text-white/55 leading-snug">
                Free at <span class="text-white/80">aistudio.google.com/apikey</span>. The overlay won't answer questions until a key is saved.
              </div>
            </div>
          </div>
        {/if}

        <section class="card-glass p-5">
          <h2 class="mb-1 text-[13px] font-semibold text-white">Gemini API key</h2>
          <p class="mb-3.5 text-[11.5px] text-white/50">
            Stored locally, only sent to Google's API.
          </p>
          <div class="flex gap-2">
            <input
              id="api-key"
              type={revealKey ? "text" : "password"}
              bind:value={settings.apiKey}
              placeholder="AIza..."
              class="input flex-1 px-3 py-2 text-[13px]"
              autocomplete="off"
              spellcheck="false"
            />
            <button class="btn rounded-md px-3 text-xs" onclick={() => (revealKey = !revealKey)}>
              {revealKey ? "Hide" : "Show"}
            </button>
          </div>
        </section>

        <section class="card-glass p-5">
          <div class="mb-2 flex items-baseline justify-between">
            <h2 class="text-[13px] font-semibold text-white">System prompt</h2>
            <span class="text-[10.5px] text-white/40">optional — overrides default</span>
          </div>
          <p class="mb-3 text-[11.5px] text-white/50 leading-snug">
            Shape how Gemini answers. Leave blank to use the built-in default (code-first, concise).
          </p>
          <textarea
            bind:value={settings.systemPrompt}
            placeholder="e.g., You are a senior Python engineer. Reply with idiomatic code and short rationale."
            rows="4"
            class="input w-full resize-y px-3 py-2 text-[12.5px] font-mono leading-relaxed"
            style="font-family: 'JetBrains Mono', 'Cascadia Code', Consolas, monospace;"
          ></textarea>
        </section>

        <section class="card-glass p-5">
          <h2 class="mb-3.5 text-[13px] font-semibold text-white">Model</h2>
          <div class="grid grid-cols-1 gap-1.5">
            {#each [
              { id: "gemini-2.5-flash",      title: "Flash",      sub: "Fast · cheap · supports vision" },
              { id: "gemini-2.5-pro",        title: "Pro",        sub: "Deepest reasoning · rate-limited free tier" },
              { id: "gemini-2.5-flash-lite", title: "Flash Lite", sub: "Cheapest" },
            ] as m}
              <button
                class="flex items-center justify-between rounded-md border px-3.5 py-2.5 text-left transition
                  {settings.model === m.id
                    ? 'border-white/25 bg-white/6'
                    : 'border-white/6 bg-white/2 hover:bg-white/4'}"
                onclick={() => (settings.model = m.id)}
              >
                <div>
                  <div class="text-[13px] font-medium text-white">{m.title}</div>
                  <div class="text-[11px] text-white/45">{m.sub}</div>
                </div>
                <div class="h-3.5 w-3.5 rounded-full {settings.model === m.id ? 'bg-white' : 'border border-white/25'}"></div>
              </button>
            {/each}
          </div>
        </section>
      {/if}

      {#if active === "typing"}
        <section class="card-glass p-5 space-y-5">
          <div>
            <div class="flex items-baseline justify-between">
              <h3 class="text-[13px] font-medium text-white">Per-character delay</h3>
              <span class="tabular-nums text-[13px] text-white/75">{settings.delayMs} ms {settings.delayMs < 25 ? "⚠" : ""}</span>
            </div>
            <input type="range" min="1" max="80" step="1" bind:value={settings.delayMs} class="mt-2.5 w-full" />
            <p class="mt-1.5 text-[11px] text-white/40">
              Below ~25 ms browsers drop chars. Sweet spot: <span class="text-white/65">30–40 ms</span>.
            </p>
          </div>

          <div class="pt-5" style="border-top: 1px solid rgba(255,255,255,0.05);">
            <div class="flex items-baseline justify-between">
              <h3 class="text-[13px] font-medium text-white">Jitter</h3>
              <span class="tabular-nums text-[13px] text-white/75">±{settings.jitterMs} ms</span>
            </div>
            <input type="range" min="0" max="40" step="1" bind:value={settings.jitterMs} class="mt-2.5 w-full" />
            <p class="mt-1.5 text-[11px] text-white/40">Random extra delay per char.</p>
          </div>

          <div class="pt-5" style="border-top: 1px solid rgba(255,255,255,0.05);">
            <div class="flex items-baseline justify-between">
              <h3 class="text-[13px] font-medium text-white">Countdown before typing</h3>
              <span class="tabular-nums text-[13px] text-white/75">{settings.countdownSec} s</span>
            </div>
            <input type="range" min="0" max="10" step="1" bind:value={settings.countdownSec} class="mt-2.5 w-full" />
            <p class="mt-1.5 text-[11px] text-white/40">Time to click into the target answer box.</p>
          </div>
        </section>

        <section class="card-glass p-5">
          <div class="flex items-start justify-between gap-4">
            <div class="flex-1">
              <h3 class="text-[13px] font-medium text-white">Human-like typing</h3>
              <p class="mt-1 text-[11.5px] text-white/50 leading-snug">
                Adds neighbor-key typos with backspace corrections, longer pauses around punctuation, occasional thinking pauses. Slower but reads as a person.
              </p>
            </div>
            <button
              role="switch"
              aria-checked={settings.humanMode}
              class="switch"
              onclick={() => (settings.humanMode = !settings.humanMode)}
              aria-label="Toggle human-like typing"
            ></button>
          </div>
        </section>

        <section class="card-glass p-5">
          <div class="flex items-start justify-between gap-4">
            <div class="flex-1">
              <h3 class="text-[13px] font-medium text-white">Auto-pause on overlay focus</h3>
              <p class="mt-1 text-[11.5px] text-white/50 leading-snug">
                If you click on the overlay while it's typing (to drag, scroll, etc.), typing automatically pauses. Resume from the toolbar or <kbd>Ctrl Shift P</kbd>.
              </p>
            </div>
            <span class="text-[11px] text-emerald-300/85">Always on</span>
          </div>
        </section>
      {/if}

      {#if active === "hotkeys"}
        <section class="card-glass p-5">
          <div class="divide-y" style="--tw-divide-opacity:1;">
            {#each HOTKEYS as h, i}
              <div class="flex items-center justify-between py-2.5" style={i > 0 ? "border-top: 1px solid rgba(255,255,255,0.05);" : ""}>
                <span class="text-[13px] text-white/80">{h.label}</span>
                <div class="flex gap-1">
                  {#each h.keys.split(" ") as k}
                    <kbd>{k}</kbd>
                  {/each}
                </div>
              </div>
            {/each}
          </div>
        </section>
        <p class="px-1 text-[11px] text-white/40">
          Hardcoded for now. Custom rebinding is on the roadmap.
        </p>
      {/if}

      {#if active === "about"}
        <section class="card-glass p-5">
          <div class="flex items-center gap-3">
            <div class="grid h-12 w-12 place-items-center rounded-xl" style="background:#000; border:1px solid rgba(255,255,255,0.12);">
              <span class="block h-3 w-3 rounded-full bg-white"></span>
            </div>
            <div>
              <h3 class="text-[15px] font-semibold text-white tracking-tight">Writing Agent</h3>
              <p class="text-[11px] text-white/50">Stealth Gemini overlay · BYOK</p>
            </div>
          </div>

          <div class="mt-5 grid grid-cols-1 gap-1.5 text-[12px]">
            <div class="flex justify-between rounded-md px-3 py-2" style="background: rgba(255,255,255,0.03); border:1px solid rgba(255,255,255,0.04);">
              <span class="text-white/55">Stealth</span>
              <span class="flex items-center gap-1.5 text-white/85">
                <span class="h-1.5 w-1.5 rounded-full dot-ok"></span>
                SetWindowDisplayAffinity
              </span>
            </div>
            <div class="flex justify-between rounded-md px-3 py-2" style="background: rgba(255,255,255,0.03); border:1px solid rgba(255,255,255,0.04);">
              <span class="text-white/55">Tray</span>
              <span class="flex items-center gap-1.5 text-white/85">
                <span class="h-1.5 w-1.5 rounded-full dot-ok"></span>
                Enabled
              </span>
            </div>
            <div class="flex justify-between rounded-md px-3 py-2" style="background: rgba(255,255,255,0.03); border:1px solid rgba(255,255,255,0.04);">
              <span class="text-white/55">Storage</span>
              <span class="text-white/70 truncate">%APPDATA%\com.writingtool.app\</span>
            </div>
          </div>

          <p class="mt-5 rounded-md p-3 text-[11px] leading-snug text-amber-100/85"
             style="background: rgba(245,158,11,0.06); border:1px solid rgba(245,158,11,0.20);">
            <strong>Heads up:</strong> kernel-level proctoring software (Respondus, Honorlock) can still see this overlay. Standard browser-based proctoring cannot.
          </p>

          <div class="mt-5 flex gap-2">
            <button class="btn rounded-md px-3 py-1.5 text-[12px]" onclick={() => toggleOverlay()}>Test overlay</button>
            <button class="btn-danger rounded-md px-3 py-1.5 text-[12px]" onclick={() => { if (confirm("Quit Writing Agent?")) quitApp(); }}>Quit</button>
          </div>
        </section>

        <p class="px-1 text-center text-[10.5px] text-white/35">
          Closes to tray. Right-click the tray icon for Quit / Toggle Overlay.
        </p>
      {/if}
        </div>
      {/key}
    </div>
  </main>
</div>
