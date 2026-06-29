//! Speech-to-text. v1 provider: Whisper via whisper-rs (whisper.cpp).
//!
//! Models are downloaded on demand from the official whisper.cpp HF repo.

use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Ensure the configured ggml model exists locally, downloading if needed.
pub async fn ensure_model(cfg: &Config) -> Result<std::path::PathBuf> {
    let path = cfg.model_path()?;
    if path.exists() {
        return Ok(path);
    }
    let url = format!("{HF_BASE}/ggml-{}.bin", cfg.whisper_model);
    tracing::info!("downloading whisper model {} ...", cfg.whisper_model);
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

/// A loaded whisper model ready to transcribe 16 kHz mono f32 audio.
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

    pub fn transcribe(&self, audio_16k_mono: &[f32]) -> Result<String> {
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
