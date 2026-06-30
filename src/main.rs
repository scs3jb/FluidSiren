//! FluidSiren for Linux.
//!
//! The capture→transcribe→enhance→inject pipeline lives in `engine`. In normal
//! use the Slint UI (`ui`) owns the event loop, drives the engine from a tray +
//! overlay, and the global hotkey (`hotkey`) triggers dictation from anywhere.
//! `--file` / `--seconds` are non-interactive test modes that bypass the UI.

mod asr;
mod audio;
mod config;
mod engine;
mod enhance;
mod hotkey;
mod inject;
mod ollama;
mod overlay;
#[cfg(feature = "parakeet")]
mod parakeet;
mod scope;
mod ui;

use anyhow::{anyhow, Result};
use config::Config;
use engine::{DictationEngine, EngineEvent};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // ashpd/zbus emit noisy (harmless) portal property-cache warnings.
                .unwrap_or_else(|_| "info,ashpd=error,zbus=error".into()),
        )
        .with_target(false)
        .init();

    // Parse mode early. UI mode needs its own systemd scope so the GlobalShortcuts
    // portal attributes our shortcut to FluidSiren — not to whatever launched us
    // (a terminal inside another app's scope would otherwise be hijacked). This
    // may re-exec the process; do it before any heavy work like model loading.
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Settings UI runs as its own process (own event loop) — no model/hotkey/scope.
    let cfg = Arc::new(Mutex::new(Config::load()?));
    if args.first().map(String::as_str) == Some("--settings") {
        return ui::run_settings(cfg);
    }

    println!("FluidSiren (Linux) — config: {}", Config::config_path()?.display());
    let test_mode = matches!(args.first().map(String::as_str), Some("--file") | Some("--seconds"));
    if !test_mode {
        scope::reexec_in_own_scope_if_needed(&args);
    }

    // Model download + load (snapshot of the shared config).
    let snapshot = cfg.lock().unwrap().clone();
    let rt = tokio::runtime::Runtime::new()?;
    let transcriber = rt.block_on(asr::load_transcriber(&snapshot))?;
    drop(rt); // the engine builds its own runtime; release this one.

    // If transcript enhancement is enabled, bring up the local Ollama server now
    // so the first dictation cleans up instead of silently falling back to the raw
    // transcript. Best-effort: a failure here never blocks dictation.
    if snapshot.enhance {
        match ollama::start() {
            Ok(()) => println!("Ollama enhancement enabled — local server ensured running."),
            Err(e) => eprintln!("Ollama enhancement enabled, but the server could not be started: {e:#}"),
        }
        // Preload the model in the background so the first dictation is fast.
        // Detached: a cold model load can take tens of seconds; never block startup.
        let warm_cfg = snapshot.clone();
        std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                match rt.block_on(enhance::warm_up(&warm_cfg)) {
                    Ok(()) => println!("Ollama model warmed up (kept alive for {}).", warm_cfg.ollama_keep_alive),
                    Err(e) => eprintln!("Ollama warm-up failed (first dictation may be slow): {e:#}"),
                }
            }
        });
    }

    if !inject::available() {
        eprintln!(
            "WARNING: ydotool not usable — transcripts will not be typed.\n\
             Install ydotool + run ydotoold, and ensure /dev/uinput access."
        );
    }

    // Non-interactive test/automation modes — never touch the UI or hotkey.
    if test_mode {
        if !args.iter().any(|a| a == "--inject") {
            std::env::set_var("FLUIDSIREN_NO_INJECT", "1");
            eprintln!("(test mode: injection disabled; pass --inject to type into the focused window)");
        }
        let engine = DictationEngine::spawn(
            cfg.clone(),
            transcriber,
            Box::new(|ev| {
                if let EngineEvent::Error(e) = ev {
                    eprintln!("Engine error: {e}");
                }
            }),
        );
        match args.first().map(String::as_str) {
            Some("--file") => {
                let path = args
                    .get(1)
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow!("usage: fluidsiren --file <path.wav>"))?;
                print_transcript(engine.process_blocking(audio::load_wav(&path)?));
            }
            Some("--seconds") => {
                let secs: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5.0);
                println!("Recording {secs}s from default mic...");
                print_transcript(engine.process_blocking(audio::record_for(secs)?));
            }
            _ => {}
        }
        engine.shutdown();
        return Ok(());
    }

    // UI mode: the Slint UI owns the event loop and builds the engine through
    // this closure (so the overlay handle exists before the engine and can be
    // captured by the event callback). The global hotkey starts here and is
    // re-bound live when the settings window changes it.
    let cfg_for_engine = cfg.clone();
    let cfg_for_hotkey = cfg.clone();

    ui::run(cfg.clone(), move |callback| {
        let engine = DictationEngine::spawn(cfg_for_engine, transcriber, callback);
        spawn_hotkey_watcher(engine.clone(), cfg_for_hotkey);
        engine
    })
}

/// Spawn the hotkey listener from the current config; `None` if it couldn't start.
fn spawn_hotkey(engine: &DictationEngine, cfg: &Config) -> Option<hotkey::HotkeyHandle> {
    let mode = cfg.hotkey_mode.parse::<hotkey::HotkeyMode>().unwrap_or_default();
    let backend = cfg.hotkey_backend.parse::<hotkey::HotkeyBackend>().unwrap_or_default();
    match hotkey::spawn_with(engine.clone(), mode, backend, &cfg.hotkey_key) {
        Ok(handle) => {
            println!("Global hotkey active ({mode:?} / {backend:?}), key {}.", cfg.hotkey_key);
            Some(handle)
        }
        Err(e) => {
            eprintln!("Global hotkey unavailable ({e:#}) — tray still works.");
            None
        }
    }
}

/// Own the hotkey listener on a background thread and re-bind it when the
/// config's hotkey fields change (e.g. via the settings "Rebind" button).
fn spawn_hotkey_watcher(engine: DictationEngine, cfg: Arc<Mutex<Config>>) {
    std::thread::Builder::new()
        .name("hotkey-watcher".into())
        .spawn(move || {
            let mut current = cfg.lock().unwrap().clone();
            let mut _handle = spawn_hotkey(&engine, &current);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let Ok(latest) = Config::load() else { continue };
                let changed = latest.hotkey_key != current.hotkey_key
                    || latest.hotkey_mode != current.hotkey_mode
                    || latest.hotkey_backend != current.hotkey_backend;
                if changed {
                    println!("Hotkey config changed ({} → {}); re-binding.", current.hotkey_key, latest.hotkey_key);
                    _handle = None; // stop the old listener before starting the new one
                    _handle = spawn_hotkey(&engine, &latest);
                    current = latest;
                }
            }
        })
        .expect("spawn hotkey-watcher thread");
}

fn print_transcript(text: String) {
    if text.is_empty() {
        println!("(no speech detected)");
    } else {
        println!("Transcript: {text}");
    }
}
