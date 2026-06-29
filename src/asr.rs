//! Speech-to-text providers. Whisper (whisper.cpp) is the always-available
//! default; Parakeet (NVIDIA NeMo transducer via sherpa-onnx) is an optional
//! provider selected by `config.provider`.

use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// A loaded speech-to-text model. Implementations transcribe 16 kHz mono f32
/// audio. `Send` so the engine can own it on its worker thread.
pub trait Transcriber: Send {
    fn transcribe(&self, audio_16k_mono: &[f32]) -> Result<String>;
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
            Ok(Box::new(Whisper::load(&path, &cfg.language)?))
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
    let bytes = resp.bytes().await?;
    let tmp = path.with_extension("part");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, &path).await?;
    eprintln!("Saved model to {}", path.display());
    Ok(path)
}

/// A loaded whisper model.
pub struct Whisper {
    ctx: WhisperContext,
    language: String,
}

impl Whisper {
    pub fn load(model_path: &Path, language: &str) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(
            model_path.to_str().context("non-utf8 model path")?,
            WhisperContextParameters::default(),
        )
        .context("loading whisper model")?;
        Ok(Self { ctx, language: language.to_string() })
    }
}

impl Transcriber for Whisper {
    fn transcribe(&self, audio_16k_mono: &[f32]) -> Result<String> {
        let mut state = self.ctx.create_state().context("creating whisper state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        params.set_n_threads(threads as i32);
        params.set_translate(false);
        if self.language != "auto" {
            params.set_language(Some(&self.language));
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state.full(params, audio_16k_mono).context("whisper inference")?;

        let n = state.full_n_segments().context("segment count")?;
        let mut text = String::new();
        for i in 0..n {
            text.push_str(&state.full_get_segment_text(i).unwrap_or_default());
        }
        Ok(text.trim().to_string())
    }
}
