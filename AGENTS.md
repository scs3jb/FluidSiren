# AGENTS.md — working on FluidSiren

Guidance for AI agents (Claude) and contributors. FluidSiren is a local
voice-to-text dictation app for **Wayland / KDE Plasma**, written in Rust. Read
this before changing anything Wayland-, hotkey-, or window-related — the gotchas
below were expensive to learn and are easy to reintroduce.

## What it is

Press a hotkey → record from the mic → transcribe with Whisper (`whisper.cpp`) →
optionally clean up with a local LLM (Ollama) → type the text into the focused
window via `ydotool`. A KDE tray icon, a settings window, and a bottom-center
recording overlay round it out. Everything runs locally.

## Layout

```
src/
  main.rs        binary entry; arg modes (--file, --seconds, --settings); UI-mode wiring
  engine.rs      DictationEngine — actor thread owning the cpal stream + whisper ctx;
                 capture→transcribe→enhance→inject pipeline; Clone+Send handle
  audio.rs       cpal capture + resample to 16 kHz mono; --file/--seconds helpers
  asr.rs         Whisper provider (whisper-rs) + ggml model download
  enhance.rs     Ollama cleanup pass (graceful fallback if absent)
  inject.rs      ydotool text injection (+ FLUIDSIREN_NO_INJECT kill-switch)
  hotkey.rs      global hotkey: portal (ashpd) self-bind + evdev fallback; live re-bind
  scope.rs       systemd-scope self-heal for portal app identity
  config.rs      XDG config/data paths (~/.config/fluidsiren, ~/.local/share/fluidsiren)
  ui/mod.rs      Slint tray (ksni) + settings window (subprocess) wiring
ui/*.slint       Slint markup (settings, overlay component is unused — see overlay/)
overlay/         SEPARATE crate: GTK4 + wlr-layer-shell waveform overlay binary
packaging/       .desktop file (app id dev.altic.FluidSiren)
```

Workspace: root crate `fluidsiren` + member `overlay` (`fluidsiren-overlay`).
`cargo build` builds both (`default-members`).

## Build / run / test

```bash
cargo build --release                 # both binaries
cargo build --release --features cuda # Nvidia; or --features vulkan (AMD)
cargo test --bin fluidsiren           # unit tests (enhance, …)

# Headless test modes (no GUI; injection OFF unless --inject):
./target/release/fluidsiren --file clip.wav
./target/release/fluidsiren --seconds 5
```

Requires: GCC/CMake (whisper.cpp builds from source), GTK4 + `gtk4-layer-shell`,
ALSA dev. Runtime: PipeWire, `ydotoold`, KWin. CPU build is the universal default;
this is the primary dev/test target.

## Hard-won Wayland / KDE gotchas (do not relearn these)

1. **Slint/winit cannot map a window shown after the event loop starts**, nor
   resize/hide/minimize a mapped one (KWin ignores client minimize; forcing 1×1
   crashes with an xdg_toplevel protocol error). Consequences, both load-bearing:
   - The **settings window runs as a separate process** (`fluidsiren --settings`)
     — its window is shown *before its own* loop, so it maps. Don't try to show
     it in-process.
   - The **main app shows no Slint window** (tray-only). `slint::run_event_loop()`
     stays alive with zero windows until `quit_event_loop()`.

2. **The recording overlay is a separate GTK4 + `wlr-layer-shell` binary**
   (`overlay/`). layer-shell is the only way to get an always-on-top, anchored,
   click-through Wayland overlay; Slint can't. The main app spawns it on
   `Recording` and kills it on `Idle` (see `src/overlay.rs`).

3. **KDE GlobalShortcuts portal will not auto-bind a key** (security: the user is
   meant to bind it in System Settings). We bypass that with kglobalaccel's
   `setForeignShortcut` (the same call System Settings makes) to **self-bind on
   startup** — see `hotkey.rs::set_portal_shortcut`. The keycode is a `Qt::Key`
   value, optionally OR'd with modifier flags (Ctrl `0x04000000`, Shift
   `0x02000000`, Alt `0x08000000`, Meta `0x10000000`).

4. **Synthetic keys (`ydotool key`) do NOT trigger portal global shortcuts** —
   only real keypresses do. So you cannot headlessly verify the hotkey fires;
   verify the *binding* in `~/.config/kglobalshortcutsrc` instead and have a human
   press the key.

5. **Portal app identity comes from the systemd scope.** A process launched from a
   terminal inherits that terminal's scope, so the portal would attribute our
   shortcut to the wrong app (it once hijacked a terminal's binding). `scope.rs`
   re-execs into `app-dev.altic.FluidSiren-<pid>.scope`; the matching `.desktop`
   must be installed for the portal to accept the id.

6. **evdev hotkey/capture needs `/dev/input` read access** (the `input` group) —
   most users won't have it, so the **portal is the default working path**. `auto`
   tries evdev first (direct key) then portal.

7. **Typing into apps**: Wayland blocks synthetic input; `ydotool` (uinput) works
   under KWin (`wtype` does not). `FLUIDSIREN_NO_INJECT=1` disables it (set
   automatically by `--file`/`--seconds`).

## Verifying UI changes headlessly

You usually have a live Wayland session. Useful tricks:

- **Is a window mapped?** Load a KWin script via D-Bus and read its `print()` from
  the journal:
  ```bash
  gdbus call --session --dest org.kde.KWin --object-path /Scripting \
    --method org.kde.kwin.Scripting.loadScript /path/to/list.js
  # script: workspace.windowList().forEach(w => print("WIN|"+w.caption+"|"+w.resourceClass))
  # then run Script<id> and: journalctl --user _COMM=kwin_wayland --since "5s ago"
  ```
- **Drive the tray menu** via its DBusMenu (find the SNI via
  `org.kde.StatusNotifierWatcher`, then `com.canonical.dbusmenu.Event <id> clicked`).
- **Inspect/clean shortcut bindings**: `~/.config/kglobalshortcutsrc`,
  `qdbus6 org.kde.kglobalaccel …`, `kwriteconfig6 … --delete`.
- `spectacle -b -n -f -o shot.png` can screenshot, but KWin's background capture
  sometimes returns blank — don't rely on it.

## Conventions

- Keep modules small and single-purpose; the engine is an actor (commands in,
  events out) so hotkey/UI/CLI can all drive it. Match the surrounding style.
- All app state under XDG dirs keyed `dev/altic/fluidsiren`. App id everywhere is
  `dev.altic.FluidSiren`.
- Adding an ASR model = a new provider behind the engine's transcribe step; keep
  Whisper as the always-available default.
- After a change: `cargo build --release` clean (no warnings), run the relevant
  `--file`/`--seconds`/`cargo test`, and for window/hotkey work verify via the KWin
  / kglobalshortcutsrc methods above. Flag anything that genuinely needs a human
  (real keypress, visual overlay) rather than claiming it verified.
