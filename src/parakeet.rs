//! Parakeet TDT (NVIDIA NeMo transducer) ASR via sherpa-onnx — fast, accurate
//! dictation. Two int8 models, fetched on first use (~460-640 MB):
//!   * v2 — English only; the most accurate English model of the two.
//!   * v3 — 25 European languages (auto-detected), slightly behind v2 on English.

use crate::asr::Transcriber;
use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};
use std::path::PathBuf;
use std::sync::Mutex;

const MODEL_BASE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

/// sherpa-onnx release name for the configured model version.
fn model_name(version: &str) -> &'static str {
    match version {
        "v3" => "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        other => {
            if other != "v2" {
                tracing::warn!("unknown parakeet_model '{other}', using v2");
            }
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8"
        }
    }
}

/// A loaded Parakeet recognizer. `transcribe` needs `&mut`, so it's behind a
/// `Mutex` to fit the `&self` [`Transcriber`] interface.
pub struct Parakeet {
    rec: Mutex<TransducerRecognizer>,
}

impl Parakeet {
    pub async fn load(cfg: &Config) -> Result<Self> {
        let dir = ensure_model(model_name(&cfg.parakeet_model)).await?;
        let p = |f: &str| dir.join(f).to_string_lossy().into_owned();
        let threads = crate::asr::thread_count();
        let config = TransducerConfig {
            encoder: p("encoder.int8.onnx"),
            decoder: p("decoder.int8.onnx"),
            joiner: p("joiner.int8.onnx"),
            tokens: p("tokens.txt"),
            num_threads: threads,
            sample_rate: 16000,
            feature_dim: 80,
            decoding_method: "greedy_search".into(),
            model_type: "nemo_transducer".into(),
            ..Default::default()
        };
        println!("Loading Parakeet {} model...", cfg.parakeet_model);
        let rec = TransducerRecognizer::new(config).map_err(|e| anyhow!("loading Parakeet: {e}"))?;
        Ok(Self { rec: Mutex::new(rec) })
    }
}

impl Transcriber for Parakeet {
    fn transcribe(&self, audio_16k_mono: &[f32]) -> Result<String> {
        let mut rec = self.rec.lock().unwrap();
        Ok(rec.transcribe(16000, audio_16k_mono).trim().to_string())
    }
}

/// Ensure the Parakeet model is downloaded and extracted; return its directory.
async fn ensure_model(name: &str) -> Result<PathBuf> {
    let models = Config::models_dir()?;
    let dir = models.join(name);
    if dir.join("encoder.int8.onnx").exists() {
        return Ok(dir);
    }

    let archive = models.join(format!("{name}.tar.bz2"));
    let url = format!("{MODEL_BASE_URL}/{name}.tar.bz2");
    eprintln!("Downloading Parakeet model {name} (~460-640 MB, one-time)...");
    let resp = reqwest::get(&url).await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("Parakeet download failed: {}", resp.status()));
    }
    // Stream to disk to avoid holding the whole ~460 MB archive in RAM.
    crate::asr::stream_to_file(resp, &archive).await.context("downloading Parakeet archive")?;

    eprintln!("Extracting Parakeet model...");
    let status = std::process::Command::new("tar")
        .arg("xjf")
        .arg(&archive)
        .arg("-C")
        .arg(&models)
        .status()
        .context("running tar to extract Parakeet model")?;
    if !status.success() {
        return Err(anyhow!("tar extraction failed"));
    }
    let _ = std::fs::remove_file(&archive);

    if !dir.join("encoder.int8.onnx").exists() {
        return Err(anyhow!("Parakeet model missing files after extraction"));
    }
    eprintln!("Parakeet model ready at {}", dir.display());
    Ok(dir)
}
