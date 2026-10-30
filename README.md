# FluidSiren

> Inspired by [FluidVoice](https://github.com/altic-dev/FluidVoice) (macOS), and a
> from-scratch successor to **plasma-s2t-whisperx** — a leaner, native take that
> replaces and improves on it. Because Linux deserves nice things too.

Local, private **English** voice-to-text dictation for **Wayland / KDE Plasma**.
Press a hotkey, talk, and your speech is transcribed on-device (Whisper or
Parakeet) and typed into whatever window is focused — with an optional local-LLM
cleanup pass and a live waveform overlay. Nothing leaves your machine.

- **On-device ASR** — **Parakeet TDT** (fast, accurate English; NVIDIA NeMo via sherpa-onnx) or **Whisper** (`whisper.cpp`). Pick in Settings. CPU by default; CUDA / Vulkan opt-in.
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

To uninstall, run [`scripts/uninstall.sh`](scripts/uninstall.sh) (add `--purge`
to also remove the config, downloaded models, and Ollama; add `--keep-config`
alongside `--purge` to remove everything but keep `~/.config/fluidsiren`).

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
./target/release/fluidsiren --file a.wav b.wav   # several clips: path, seconds, text per line
./target/release/fluidsiren --file long.wav --live   # replay through the incremental path
# add --inject to actually type; or set FLUIDSIREN_NO_INJECT=1 to force-disable typing
```

## Configuration

`~/.config/fluidsiren/config.toml`:

| Key | Default | Notes |
| --- | --- | --- |
| `provider` | `whisper` | `whisper` \| `parakeet` (switch in Settings; restart to apply) |
| `whisper_model` | `base.en` | any whisper.cpp ggml name (`tiny.en`, `small.en`, `large-v3-turbo-q5_0`, …) |
| `whisper_beam_size` | `1` | 1 = greedy; 5 = beam search |
| `parakeet_model` | `v2` | `v2` (English) \| `v3` (25 European languages) |
| `vocabulary` | `[]` | preferred spellings for names/jargon |
| `incremental` | `true` | transcribe finished phrases while still recording |
| `language` | `en` | or `auto` |
| `enhance` | `false` | Ollama cleanup pass |
| `ollama_model` / `ollama_url` | `llama3.2:3b` / `http://localhost:11434` | |
| `hotkey_backend` | `auto` | `auto` \| `portal` \| `evdev` |
| `hotkey_mode` | `toggle` | `toggle` \| `push_to_talk` (evdev only) |
| `hotkey_key` | `F12` | the hotkey (e.g. `F12`, `Ctrl+Shift+D`, `Meta+Space`); rebind from Settings |

The settings window runs as a separate process and edits this file; the running
app picks up changes (enhancement, hotkey) live.

## Speech models

FluidSiren is focused on **English** dictation, with two on-device providers:

- **Parakeet TDT 0.6B** (recommended): an NVIDIA NeMo transducer run through
  sherpa-onnx. On CPU it is about 4× faster than Whisper `base.en` and makes
  about a third of the errors. Pick `parakeet_model`:
  - `v2`: English, about 460 MB, the most accurate.
  - `v3`: 25 European languages, auto-detected, about 640 MB.

  The model downloads on first use to `~/.local/share/fluidsiren/models/`.
  Select **parakeet** in Settings.
- **Whisper** (the default provider, always built in): runs on `whisper.cpp`.
  - The `.en` models (`tiny.en` to `medium.en`) are English-only.
  - `large-v3-turbo-q5_0` is the best accuracy-to-size option, if you have a GPU.
  - `large-v3` is the most accurate and the slowest.

  Whisper encodes a fixed 30 s window, so even short clips cost the full window
  on CPU.

Measured on 73 LibriSpeech clips, resampled to 48 kHz with mic noise and
leading/trailing silence added, on a 4-core CPU:

| Model | WER | Time per clip |
| --- | --- | --- |
| Parakeet v2 | 3.6 % | 0.5 s |
| Whisper `base.en` | 11.4 % | 2.0 s |
| Whisper `small.en` | ~7 % (20-clip subset) | 7 s |

The audio pipeline is shared by both providers:

- **Band-limited resampling.** The mic's 44.1/48 kHz audio is low-passed before
  it is decimated to 16 kHz, so hiss above 8 kHz doesn't alias into the speech
  band.
- **Silence trimming and a no-speech skip.** A lightweight VAD cuts the
  hotkey-press silence. An empty recording never reaches the model, so Whisper
  can't hallucinate "Thank you." from silence.
- **Incremental transcription** (`incremental = true`). In long dictations,
  each phrase is transcribed during the next pause while you keep talking, so
  stopping only waits for the last phrase.
- **Vocabulary** (`vocabulary = ["Kubernetes", "Siobhan"]`). Preferred
  spellings for names and jargon. They feed Whisper's initial prompt and the
  Ollama cleanup prompt.

Other Whisper and Ollama settings:

- Whisper decoding is greedy by default. `whisper_beam_size = 5` matches
  whisper.cpp's CLI, but it measured no accuracy gain for about 15 % more time.
- With Ollama cleanup, reasoning models such as `qwen3:4b` or `qwen3:1.7b`
  are run with thinking disabled. That makes them low-latency alternatives to
  `llama3.2:3b`.

The transcriber is a small pluggable trait (`asr::Transcriber`), so adding a
provider is straightforward. Build without Parakeet (smaller binary, no
sherpa-onnx) via `cargo build --release --no-default-features`.

## License

GPL-3.0-only.
