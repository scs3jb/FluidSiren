//! Speech-to-text providers. Whisper (whisper.cpp) is the always-available
//! default; Parakeet (NVIDIA NeMo transducer via sherpa-onnx) is an optional
//! provider selected by `config.provider`.

use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::sync::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// A loaded speech-to-text model. Implementations transcribe 16 kHz mono f32
/// audio. `Send` so the engine can own it on its worker thread.
pub trait Transcriber: Send {
    fn transcribe(&self, audio_16k_mono: &[f32]) -> Result<String>;

    /// Run one throwaway inference so the first real dictation doesn't pay for
    /// lazy allocations / kernel selection. Called on the engine thread at start.
    fn warm_up(&self) {
        let _ = self.transcribe(&vec![0.0; crate::audio::WHISPER_SAMPLE_RATE as usize]);
    }
}

/// Build the transcriber selected by `config.provider`, downloading its model on
/// first use. Falls back to Whisper for an unknown provider.
pub async fn load_transcriber(cfg: &Config) -> Result<Box<dyn Transcriber>> {
    match cfg.provider.as_str() {
        "parakeet" => {
            #[cfg(feature = "parakeet")]
            {
                Ok(Box::new(crate::parakeet::Parakeet::load(cfg).await?))
            }
            #[cfg(not(feature = "parakeet"))]
            {
                Err(anyhow!("Parakeet support not built in (rebuild with --features parakeet)"))
            }
        }
        other => {
            if other != "whisper" {
                tracing::warn!("unknown provider '{other}', using whisper");
            }
            let path = ensure_whisper_model(cfg).await?;
            println!("Loading whisper model '{}'...", cfg.whisper_model);
            Ok(Box::new(Whisper::load(&path, cfg)?))
        }
    }
}

/// Ensure the configured ggml whisper model exists locally, downloading if needed.
async fn ensure_whisper_model(cfg: &Config) -> Result<std::path::PathBuf> {
    let path = cfg.model_path()?;
    if path.exists() {
        return Ok(path);
    }
    let url = format!("{HF_BASE}/ggml-{}.bin", cfg.whisper_model);
    eprintln!("Downloading whisper model '{}' (one-time)...", cfg.whisper_model);

    let resp = reqwest::get(&url).await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("model download failed ({}): {}", resp.status(), url));
    }
    // Stream to disk so the whole (hundreds of MB) model never sits in RAM.
    let tmp = path.with_extension("part");
    stream_to_file(resp, &tmp).await.with_context(|| format!("downloading {url}"))?;
    tokio::fs::rename(&tmp, &path).await?;
    eprintln!("Saved model to {}", path.display());
    Ok(path)
}

/// Stream an HTTP response body to `path` chunk-by-chunk so a large download never
/// has to be held fully in memory. The caller renames/extracts afterward.
pub(crate) async fn stream_to_file(resp: reqwest::Response, path: &Path) -> Result<()> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::create(path).await.context("creating download file")?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading download chunk")?;
        file.write_all(&chunk).await.context("writing download chunk")?;
    }
    file.flush().await.context("flushing download")?;
    Ok(())
}

/// A loaded whisper model.
pub struct Whisper {
    /// Decoder state, created once and reused: allocating the KV cache and
    /// compute buffers on every dictation costs more than short clips' decode.
    state: Mutex<WhisperState>,
    language: String,
    /// Initial prompt biasing the decoder toward the user's vocabulary.
    prompt: String,
    beam_size: i32,
}

impl Whisper {
    pub fn load(model_path: &Path, cfg: &Config) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(
            model_path.to_str().context("non-utf8 model path")?,
            WhisperContextParameters::default(),
        )
        .context("loading whisper model")?;
        let state = ctx.create_state().context("creating whisper state")?;
        Ok(Self {
            state: Mutex::new(state),
            language: cfg.language.clone(),
            prompt: vocabulary_prompt(&cfg.vocabulary),
            beam_size: cfg.whisper_beam_size as i32,
        })
    }
}

/// Whisper conditions on the previous text; a short glossary-style prompt makes
/// it spell names and jargon the way the user does. Empty when no vocabulary.
fn vocabulary_prompt(words: &[String]) -> String {
    let words: Vec<&str> = words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        String::new()
    } else {
        format!("Glossary: {}.", words.join(", "))
    }
}

impl Transcriber for Whisper {
    fn transcribe(&self, audio_16k_mono: &[f32]) -> Result<String> {
        let mut state = self.state.lock().unwrap();
        let strategy = if self.beam_size > 1 {
            SamplingStrategy::BeamSearch { beam_size: self.beam_size, patience: -1.0 }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        params.set_n_threads(thread_count());
        params.set_translate(false);
        if self.language != "auto" {
            params.set_language(Some(&self.language));
        }
        // Each dictation is independent: don't carry text over from the last one
        // (the reused state would otherwise prompt with it).
        params.set_no_context(true);
        // Never emit "[BLANK_AUDIO]", "(music)" and other non-speech tokens.
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        if !self.prompt.is_empty() {
            params.set_initial_prompt(&self.prompt);
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state.full(params, audio_16k_mono).context("whisper inference")?;

        let n = state.full_n_segments().context("segment count")?;
        let mut text = String::new();
        for i in 0..n {
            let seg = state.full_get_segment_text(i).unwrap_or_default();
            if !is_non_speech_annotation(&seg) {
                text.push_str(&seg);
            }
        }
        Ok(text.trim().to_string())
    }
}

/// Inference threads. Matrix kernels scale with physical cores; SMT siblings
/// mostly contend for the same units, so cap at a sane count rather than using
/// every logical CPU.
pub(crate) fn thread_count() -> i32 {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8) as i32
}

/// Whole segments like "[BLANK_AUDIO]", "(wind blowing)" or "[Music]" are
/// Whisper describing non-speech, never something the user dictated.
fn is_non_speech_annotation(segment: &str) -> bool {
    let t = segment.trim();
    t.is_empty()
        || (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('*') && t.ends_with('*'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_segments_are_dropped() {
        assert!(is_non_speech_annotation(" [BLANK_AUDIO]"));
        assert!(is_non_speech_annotation("(music playing)"));
        assert!(is_non_speech_annotation("*sigh*"));
        assert!(!is_non_speech_annotation(" Hello (there) world."));
    }

    #[test]
    fn vocabulary_prompt_formats_glossary() {
        assert_eq!(vocabulary_prompt(&[]), "");
        let words = vec!["FluidSiren".to_string(), " ".to_string(), "Parakeet".to_string()];
        assert_eq!(vocabulary_prompt(&words), "Glossary: FluidSiren, Parakeet.");
    }
}
