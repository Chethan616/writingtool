# Writing Agent

A stealth, translucent desktop overlay that asks **Gemini** about whatever's on
your screen and types the answer into any focused window keystroke-by-keystroke.

Built for Windows. Tauri 2 (Rust + WebView2) + Svelte 5 + Tailwind v4. ~5 MB.

> Personal / practice use. See [Caveats](#caveats).

---

## Features

- **Invisible to screen capture.** `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`
  hides the overlay from OBS, Teams share, screenshots, Game Bar — anything on
  the standard DWM / BitBlt capture path (Windows 10 build 19041+).
- **BYOK Gemini.** Bring your own API key. Non-streaming `generateContent`
  with safety-relaxed settings and proper error surfacing (429s, blocked
  prompts, auth failures).
- **Bypass "paste blocked" sites.** A `Write` button next to each answer
  simulates real keystrokes via `SendInput` Unicode packets — no clipboard
  involved, so sites that block paste can't tell.
- **Human-like typing.** Optional mode: neighbor-key typos with backspace
  corrections, longer pauses around punctuation, occasional "thinking" pauses.
- **Live pause / resume / stop** during typing, with progress bar.
- **Auto-pause on focus.** If you click into the overlay mid-type, typing
  pauses automatically so keystrokes don't get lost.
- **Vision.** Paste or drag-drop an image — Gemini sees it.
- **Session history.** Every chat auto-saved; browse / restore / delete in a
  side drawer (up to 50 sessions).
- **System tray** + close-to-tray.
- **Single instance.** Re-launching focuses the existing window instead of
  spawning a duplicate.

---

## Hotkeys

| Action | Shortcut |
|---|---|
| Show / hide overlay (global) | `Ctrl + Shift + Space` |
| Ask Gemini (global)          | `Ctrl + Shift + Enter` |
| Send question (focused)      | `Enter` |
| New chat                     | `Ctrl + N` |
| Toggle history               | `Ctrl + H` |
| Pause / resume typing        | `Ctrl + Shift + P` |
| Stop generating              | `Ctrl + .` |
| Cancel / hide overlay        | `Esc` |

---

## Install

### Prebuilt
Grab the installer from `src-tauri/target/release/bundle/` after building:

- **NSIS** (recommended): `Writing Agent_0.1.0_x64-setup.exe`
- **MSI**: `Writing Agent_0.1.0_x64_en-US.msi`

Or run the portable executable directly:
`src-tauri/target/release/writingagent.exe`.

---

## Quick start

1. Launch. Settings opens. Paste your Gemini key
   ([get one free](https://aistudio.google.com/apikey)) → **Save**.
2. Press `Ctrl + Shift + Space` anywhere. Translucent overlay appears.
3. Type a question (or paste an image), hit `Enter`. Answer streams in.
4. Click **Write code** → 3-second countdown → click into the target text
   box → text types in.

Closing the Settings window hides it to the tray.
Right-click the tray icon for Quit.

---

## Build from source

### Prereqs
- Node 22+ and npm
- Rust 1.77+ (stable)
- Visual Studio 2022 Build Tools with the "Desktop development with C++"
  workload
- WebView2 runtime (preinstalled on Windows 11)

### Steps
```powershell
git clone <repo>
cd writing-agent
npm install
npm run tauri:dev    # dev with HMR
npm run tauri:build  # produces installers
```

First Rust compile is 2–5 min; subsequent builds are fast.

---

## Architecture

```
.
├── package.json               # npm workspace
├── tsconfig.json
├── vite.config.js
├── svelte.config.js
├── settings.html              # main window entry
├── overlay.html               # overlay window entry
├── src/
│   ├── app.css                # design system (tokens + components)
│   ├── settings.ts            # mounts Settings.svelte
│   ├── overlay.ts             # mounts Overlay.svelte
│   └── lib/
│       ├── api.ts             # invoke wrappers, store, listeners
│       ├── Icon.svelte        # Lucide-style SVG icons
│       ├── Settings.svelte    # main window UI
│       └── Overlay.svelte     # overlay window UI
└── src-tauri/
    ├── Cargo.toml
    ├── tauri.conf.json        # two windows declared here
    ├── build.rs
    ├── capabilities/default.json
    └── src/
        ├── main.rs            # entry
        ├── lib.rs             # commands, setup, hotkeys, tray
        ├── gemini.rs          # REST client
        └── win32.rs           # stealth + SendInput typing
```

### Two-window design
- **main** — regular chrome, Settings.
- **overlay** — frameless, transparent, always-on-top, `WS_EX_TOOLWINDOW`,
  `WS_EX_NOACTIVATE`, `WDA_EXCLUDEFROMCAPTURE`. Shows on hotkey.

### Typing path
JS calls `type_text(text, delay, jitter, human)` → Rust spawns a worker
thread → `TypingState::type_text` loop sends one `SendInput` per char with
`KEYEVENTF_UNICODE`. Pause / cancel via atomic flags. Progress emitted via
`typing://progress` events.

### Auto-pause
`getCurrentWindow().onFocusChanged(...)` in `Overlay.svelte` calls
`pauseTyping()` whenever the overlay gains focus during the typing phase.

---

## Caveats

- **Kernel-level proctoring** (Respondus, Honorlock, ProctorU's AI Edge)
  can still see the overlay. Standard browser-based proctoring cannot.
- **API key on disk.** Stored unencrypted in
  `%APPDATA%\com.writingtool.app\settings.json`. Fine for a personal BYOK
  build; do not ship this binary with a shared key.
- **Free-tier Pro is heavily throttled.** Use Flash for routine queries;
  Pro only when needed.
- **Browser typing speed.** Below ~25 ms per char browsers drop / coalesce
  events. Default is 35 ms. Settings will warn on values below 25.
- **Tool is dual-use.** Designed for practice sessions and personal study.

---

## License

MIT — see [LICENSE](LICENSE).
