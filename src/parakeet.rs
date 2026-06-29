//! Parakeet TDT (NVIDIA NeMo transducer) ASR via sherpa-onnx — fast, accurate
//! English dictation. The model (~460 MB download, int8) is fetched on first use.

use crate::asr::Transcriber;
use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};
use std::path::PathBuf;
use std::sync::Mutex;

const MODEL_NAME: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2";

/// A loaded Parakeet recognizer. `transcribe` needs `&mut`, so it's behind a
/// `Mutex` to fit the `&self` [`Transcriber`] interface.
pub struct Parakeet {
    rec: Mutex<TransducerRecognizer>,
}

impl Parakeet {
    pub async fn load(_cfg: &Config) -> Result<Self> {
        let dir = ensure_model().await?;
        let p = |f: &str| dir.join(f).to_string_lossy().into_owned();
        let threads = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
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
        println!("Loading Parakeet model...");
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
async fn ensure_model() -> Result<PathBuf> {
    let models = Config::models_dir()?;
    let dir = models.join(MODEL_NAME);
    if dir.join("encoder.int8.onnx").exists() {
        return Ok(dir);
    }

    let archive = models.join(format!("{MODEL_NAME}.tar.bz2"));
    eprintln!("Downloading Parakeet model (~460 MB, one-time)...");
    let resp = reqwest::get(MODEL_URL).await.with_context(|| format!("GET {MODEL_URL}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("Parakeet download failed: {}", resp.status()));
    }
    let bytes = resp.bytes().await?;
    tokio::fs::write(&archive, &bytes).await.context("writing Parakeet archive")?;

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
