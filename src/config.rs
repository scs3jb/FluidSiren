//! Configuration + model storage paths (XDG-compliant).

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// ASR provider: "whisper" | "parakeet".
    pub provider: String,
    /// Whisper ggml model name (resolved against the whisper.cpp HF repo).
    pub whisper_model: String,
    /// Language hint for whisper ("en", "auto", ...).
    pub language: String,
    /// Whisper beam width. 1 = greedy (fastest); 5 matches whisper.cpp's CLI and
    /// is a little more accurate at roughly 2× the decode time.
    pub whisper_beam_size: u32,
    /// Parakeet model: "v2" (English, most accurate) | "v3" (25 European languages).
    pub parakeet_model: String,
    /// Names, jargon and spellings to prefer. Biases Whisper (initial prompt) and
    /// the Ollama cleanup pass.
    pub vocabulary: Vec<String>,
    /// Transcribe finished phrases while you're still talking, so stopping only
    /// has to process the last phrase. Cuts post-dictation latency on long takes.
    pub incremental: bool,
    /// Whether to run the Ollama enhancement pass (M3).
    pub enhance: bool,
    /// Ollama model used for enhancement.
    pub ollama_model: String,
    /// Ollama base URL.
    pub ollama_url: String,
    /// How long Ollama keeps the model resident after a request ("5m", "30m",
    /// "1h", or "-1" for forever). Longer avoids slow cold-reloads between
    /// dictations at the cost of holding RAM/VRAM.
    pub ollama_keep_alive: String,
    /// Hotkey backend: "auto" | "portal" | "evdev".
    pub hotkey_backend: String,
    /// Hotkey trigger mode: "toggle" | "push_to_talk".
    pub hotkey_mode: String,
    /// Hotkey, e.g. "F12", "Ctrl+Shift+D", "Meta+Space". Rebind from Settings.
    pub hotkey_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "whisper".to_string(),
            whisper_model: "base.en".to_string(),
            language: "en".to_string(),
            whisper_beam_size: 1,
            parakeet_model: "v2".to_string(),
            vocabulary: Vec::new(),
            incremental: true,
            enhance: false,
            ollama_model: "llama3.2:3b".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            ollama_keep_alive: "30m".to_string(),
            hotkey_backend: "auto".to_string(),
            hotkey_mode: "toggle".to_string(),
            hotkey_key: "F12".to_string(),
        }
    }
}

fn dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "altic", "fluidsiren")
        .context("could not resolve XDG project directories")
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading config {}", path.display()))?;
            Ok(toml::from_str(&text)?)
        } else {
            let cfg = Self::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(dirs()?.config_dir().join("config.toml"))
    }

    /// Last-modified time of the config file, or `None` if it doesn't exist / can't
    /// be stat'd. A cheap change check for the live-reload pollers, so they only
    /// re-read and re-parse the file when it actually changed.
    pub fn config_mtime() -> Option<std::time::SystemTime> {
        let path = Self::config_path().ok()?;
        std::fs::metadata(path).ok()?.modified().ok()
    }

    /// Directory where downloaded ggml models live.
    pub fn models_dir() -> Result<PathBuf> {
        let dir = dirs()?.data_dir().join("models");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn model_path(&self) -> Result<PathBuf> {
        Ok(Self::models_dir()?.join(format!("ggml-{}.bin", self.whisper_model)))
    }
}
