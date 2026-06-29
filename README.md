# FluidSiren

> Inspired by [FluidVoice](https://github.com/altic-dev/FluidVoice) (macOS), and a
> from-scratch successor to **plasma-s2t-whisperx** — a leaner, native take that
> replaces and improves on it. Because Linux deserves nice things too.

Local, private voice-to-text dictation for **Wayland / KDE Plasma**. Press a
hotkey, talk, and your speech is transcribed on-device with Whisper and typed into
whatever window is focused — with an optional local-LLM cleanup pass and a live
waveform overlay. Nothing leaves your machine.

- **On-device ASR** — Whisper via `whisper.cpp` (CPU by default; CUDA / Vulkan opt-in).
- **Global hotkey** — self-binds on startup via the XDG GlobalShortcuts portal (KDE), rebindable from Settings; optional direct evdev key with push-to-talk.
- **Types into any app** — via `ydotool` (uinput), which works under KWin.
- **Optional cleanup** — local LLM via Ollama fixes punctuation/casing/fillers; degrades gracefully if absent.
- **Tray + overlay** — KDE StatusNotifierItem tray; a bottom-center GTK4 `layer-shell` pill with a live mic waveform while recording.

## Install

```bash
git clone https://github.com/scs3jb/FluidSiren && cd FluidSiren
scripts/install.sh                 # CPU; or: scripts/install.sh cuda | vulkan
ENABLE_AUTOSTART=1 scripts/install.sh   # …and start at login
fluidsiren                         # runs in the system tray
```

`install.sh` builds both binaries into `~/.local/bin` and installs the desktop
entry (required for the hotkey portal). For a system package on Arch, see
[`packaging/PKGBUILD`](packaging/PKGBUILD).

On first run it downloads a Whisper model (`base.en`, ~142 MB) to
`~/.local/share/fluidsiren/models/`. Config is `~/.config/fluidsiren/config.toml`.

### The hotkey

FluidSiren **self-binds your hotkey on startup** (default **F12**) through KDE's
GlobalShortcuts portal — it works out of the box, no System Settings step. Press it
to start/stop dictation.

To **change it**: tray → **Settings…** → *Dictation hotkey* → click the box and
press the key or combo you want (e.g. `Ctrl+Shift+D`, `Meta+Space`), then **Save**.
The change applies live.

**Advanced — a direct evdev key** (bypasses the portal, supports true
push-to-talk) needs read access to input devices:

```bash
sudo usermod -aG input $USER   # then log out and back in
```

With `hotkey_backend = "auto"` (default) FluidSiren then grabs the key directly via
evdev when it can; otherwise it falls back to the portal. (evdev reads the key
passively, so pick one no app uses; `hotkey_mode = "push_to_talk"` works here.)

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
| `hotkey_mode` | `toggle` | `toggle` \| `push_to_talk` (evdev only) |
| `hotkey_key` | `F12` | the hotkey (e.g. `F12`, `Ctrl+Shift+D`, `Meta+Space`); rebind from Settings |

The settings window runs as a separate process and edits this file; the running
app picks up changes (enhancement, hotkey) live.

## License

GPL-3.0-only.
