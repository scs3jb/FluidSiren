# Porting FluidSiren to Android: feasibility notes

Short answer: **feasible**. The engine pieces port well, but the shell around
them is a rewrite: Android has no global hotkeys, no tray, no ydotool and no
Ollama. The natural Android shape is a **voice keyboard (IME)**.

## What carries over

| Desktop piece | Android equivalent |
| --- | --- |
| Parakeet via sherpa-onnx | sherpa-onnx ships Android AARs and a Kotlin API, with prebuilt demo APKs (streaming, VAD + ASR, Parakeet, Moonshine, Whisper, Qualcomm NPU builds). Parakeet TDT 0.6B int8 runs on mid-range phones. |
| Whisper via whisper.cpp | whisper.cpp has an Android JNI example. It is slower than Parakeet on CPU, as on desktop. |
| `vad.rs`, `audio.rs` resampler, incremental segmentation, prompt/heuristics in `enhance.rs` | Pure Rust with no OS dependencies. Build with `cargo-ndk` and expose to Kotlin with `uniffi`. |
| cpal capture | `AudioRecord` (Kotlin) at 16 kHz mono. Android resamples natively, so the sinc resampler is unnecessary there. |
| ydotool injection | `InputMethodService.currentInputConnection.commitText()` |
| Global hotkey + tray + overlay | A mic button on the IME's keyboard view. Optionally also register a `RecognitionService` so other apps' "voice input" uses it. |
| Ollama cleanup | No Ollama. Run an on-device small LLM (llama.cpp, or MediaPipe LLM Inference with a 1-2B Qwen or Gemma), or ship without cleanup. |

## Constraints

- `RECORD_AUDIO` is a runtime permission, and an IME cannot show the
  permission prompt itself, so it needs a small launcher or settings Activity
  for onboarding.
- An IME can only record while it is shown, which is fine for dictation.
- Model size: Parakeet v2 int8 is about 640 MB on disk. Download it on first
  run instead of bundling it in the APK. The Play Store limits the base APK to
  150 MB, so use asset packs or an HTTP download.
- 16 KB page-size alignment is required for native libs targeting Android 15+.
  Recent sherpa-onnx AARs comply. Rebuild any Rust `.so` with NDK r27+.

## Recommended architecture

1. **Kotlin/Compose IME** linking the **sherpa-onnx AAR** directly, using
   Silero VAD plus Parakeet TDT v2 (English) or v3 (multilingual) int8. Add
   the Nemotron streaming model or Moonshine if live partial results are
   wanted.
2. Move FluidSiren's platform-independent core (VAD/segmentation,
   post-processing, cleanup prompt and output guard, model registry and
   downloads) into a `fluidsiren-core` crate. Desktop links it directly;
   Android gets it through `cargo-ndk` + `uniffi`.
3. Migrate desktop from `sherpa-rs` (now archived upstream) to the official
   **`sherpa-onnx` Rust crate**. Desktop and Android then share one engine
   version and model format.

## Prior art (check licenses before borrowing code)

| Project | Engine | License | Notes |
| --- | --- | --- | --- |
| Transcribro | whisper.cpp + Silero VAD, Kotlin | ISC | Keyboard + RecognitionService; closest reference. |
| WhisperIME (woheller69) | Whisper | MIT per its F-Droid listing | Verify the license in the repo. |
| FUTO Voice Input | Whisper | FUTO Source First License | Source-available, not FOSS. Do not copy code. |

FluidSiren is GPL-3.0, so code from ISC/MIT projects can be incorporated with
attribution.
