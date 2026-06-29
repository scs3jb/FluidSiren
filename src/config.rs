//! Configuration + model storage paths (XDG-compliant).

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Whisper ggml model name (resolved against the whisper.cpp HF repo).
    pub whisper_model: String,
    /// Language hint for whisper ("en", "auto", ...).
    pub language: String,
    /// Whether to run the Ollama enhancement pass (M3).
    pub enhance: bool,
    /// Ollama model used for enhancement.
    pub ollama_model: String,
    /// Ollama base URL.
    pub ollama_url: String,
    /// Hotkey backend: "auto" | "portal" | "evdev".
    pub hotkey_backend: String,
    /// Hotkey trigger mode: "toggle" | "push_to_talk".
    pub hotkey_mode: String,
    /// evdev key name (evdev backend only), e.g. "KEY_PAUSE", "KEY_SCROLLLOCK".
    pub hotkey_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            whisper_model: "base.en".to_string(),
            language: "en".to_string(),
            enhance: false,
            ollama_model: "llama3.2:3b".to_string(),
            ollama_url: "http://localhost:11434".to_string(),
            hotkey_backend: "auto".to_string(),
            hotkey_mode: "toggle".to_string(),
            hotkey_key: "KEY_F12".to_string(),
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
