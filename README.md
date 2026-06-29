# FluidSiren

> Inspired by [FluidVoice](https://github.com/altic-dev/FluidVoice) (macOS), and a
> from-scratch successor to **plasma-s2t-whisperx** — a leaner, native take that
> replaces and improves on it. Because Linux deserves nice things too.

Local, private voice-to-text dictation for **Wayland / KDE Plasma**. Press a
hotkey, talk, and your speech is transcribed on-device with Whisper and typed into
whatever window is focused — with an optional local-LLM cleanup pass and a live
waveform overlay. Nothing leaves your machine.

- **On-device ASR** — Whisper via `whisper.cpp` (CPU by default; CUDA / Vulkan opt-in).
- **Global hotkey** — XDG GlobalShortcuts portal (KDE) with an evdev push-to-talk fallback.
- **Types into any app** — via `ydotool` (uinput), which works under KWin.
- **Optional cleanup** — local LLM via Ollama fixes punctuation/casing/fillers; degrades gracefully if absent.
- **Tray + overlay** — KDE StatusNotifierItem tray; a bottom-center GTK4 `layer-shell` pill with a live mic waveform while recording.

## Quick start

```bash
# 1. Build (also builds the fluidsiren-overlay binary)
cargo build --release

# 2. Install the desktop file so the KDE GlobalShortcuts portal can identify the app
install -Dm644 packaging/dev.altic.FluidSiren.desktop \
  ~/.local/share/applications/dev.altic.FluidSiren.desktop
sed -i "s|^Exec=.*|Exec=$(pwd)/target/release/fluidsiren|" \
  ~/.local/share/applications/dev.altic.FluidSiren.desktop
update-desktop-database ~/.local/share/applications 2>/dev/null || true

# 3. Run (lives in the system tray)
cargo run --release
```

On first run it downloads a Whisper model (`base.en`, ~142 MB) to
`~/.local/share/fluidsiren/models/`. Config is `~/.config/fluidsiren/config.toml`.

### Enabling the hotkey

KDE deliberately won't let an app grab a global key on its own, so pick one:

- **Direct key (recommended, "just works")** — join the `input` group so FluidSiren
  can read the key itself, then re-login:
  ```bash
  sudo usermod -aG input $USER   # then log out and back in
  ```
  With `hotkey_backend = "auto"` (default) FluidSiren uses evdev when it can, so
  **F12** then triggers dictation directly — no KDE setup. (evdev reads the key
  passively, so pick a key no app uses; F12 is free by default. `hotkey_mode =
  "push_to_talk"` is also supported here.)

- **No group change** — bind it once via the portal: *System Settings → Keyboard →
  Shortcuts → FluidSiren* (F12 is pre-filled as the default; assign and apply).

## Requirements

- **Wayland + KDE Plasma** (KWin), PipeWire audio.
- **ydotool** + a running `ydotoold`, with `/dev/uinput` access (the logind uaccess
  ACL usually grants this; check `getfacl /dev/uinput`).
- **GTK4 + layer-shell** for the overlay — Arch: `gtk4 gtk4-layer-shell`.
- *(optional)* **Ollama** for cleanup: `ollama pull llama3.2:3b && ollama serve`, then
  set `enhance = true` in the config.

## Build options

```bash
cargo build --release                    # CPU (works everywhere; default)
cargo build --release --features cuda    # Nvidia GPU
cargo build --release --features vulkan  # AMD GPU
```

Both `fluidsiren` and `fluidsiren-overlay` land in `target/release/` and must be
installed side-by-side.

## Test modes (no GUI, injection off by default)

```bash
./target/release/fluidsiren --file clip.wav    # transcribe a WAV
./target/release/fluidsiren --seconds 5        # record 5s from the mic
# add --inject to actually type; or set FLUIDSIREN_NO_INJECT=1 to force-disable typing
```

## Configuration

`~/.config/fluidsiren/config.toml`:

| Key | Default | Notes |
| --- | --- | --- |
| `whisper_model` | `base.en` | any whisper.cpp ggml name (`tiny.en`, `small.en`, `large-v3`, …) |
| `language` | `en` | or `auto` |
| `enhance` | `false` | Ollama cleanup pass |
| `ollama_model` / `ollama_url` | `llama3.2:3b` / `http://localhost:11434` | |
| `hotkey_backend` | `auto` | `auto` \| `portal` \| `evdev` |
| `hotkey_mode` | `toggle` | `toggle` \| `push_to_talk` |
| `hotkey_key` | `KEY_F12` | evdev key name (evdev backend) |

## License

GPL-3.0-only.
