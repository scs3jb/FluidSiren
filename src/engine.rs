//! DictationEngine — the core capture→transcribe→enhance→inject pipeline, run on
//! a dedicated thread so it can own the non-`Send` cpal stream and the whisper
//! context. Drivers (CLI, global hotkey, UI) talk to it through a cheap,
//! `Clone`-able, `Send` handle by sending commands; it reports progress through
//! an event callback.

use crate::asr::Transcriber;
use crate::audio::{Recorder, WHISPER_SAMPLE_RATE};
use crate::config::Config;
use crate::{enhance, inject};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

/// State changes emitted by the engine (drive an overlay/tray in the UI milestone).
#[derive(Clone, Debug)]
pub enum EngineEvent {
    Recording,
    Processing,
    #[allow(dead_code)] // carried for the future live overlay; UI ignores it now
    Transcript(String),
    Idle,
    Error(String),
}

pub type EventCallback = Box<dyn Fn(EngineEvent) + Send + 'static>;

enum Command {
    Start,
    Stop { reply: Option<Sender<String>> },
    Process { audio: Vec<f32>, reply: Option<Sender<String>> },
    Shutdown,
}

/// Cheap, clone-able handle to the engine. Safe to share across threads.
#[derive(Clone)]
pub struct DictationEngine {
    cmd_tx: Sender<Command>,
}

impl DictationEngine {
    /// Spawn the engine thread. `cfg` is shared so the UI can change settings
    /// (e.g. toggle enhancement) live. Changing the whisper model still needs a
    /// reload (not yet wired).
    pub fn spawn(cfg: Arc<Mutex<Config>>, transcriber: Box<dyn Transcriber>, on_event: EventCallback) -> Self {
        let inject_ok = inject::available();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        std::thread::Builder::new()
            .name("dictation-engine".into())
            .spawn(move || run(cfg, transcriber, inject_ok, cmd_rx, on_event))
            .expect("spawn engine thread");
        Self { cmd_tx }
    }

    /// Begin capturing from the default microphone.
    pub fn start(&self) {
        let _ = self.cmd_tx.send(Command::Start);
    }

    /// Stop capturing and process in the background (fire-and-forget; watch events).
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(Command::Stop { reply: None });
    }

    /// Stop capturing and block until the final transcript is ready.
    /// (API counterpart to `process_blocking`; kept for CLI/test drivers.)
    #[allow(dead_code)]
    pub fn stop_blocking(&self) -> String {
        let (tx, rx) = mpsc::channel();
        if self.cmd_tx.send(Command::Stop { reply: Some(tx) }).is_err() {
            return String::new();
        }
        rx.recv().unwrap_or_default()
    }

    /// Process a pre-recorded 16 kHz mono clip and block for the transcript.
    pub fn process_blocking(&self, audio: Vec<f32>) -> String {
        let (tx, rx) = mpsc::channel();
        if self
            .cmd_tx
            .send(Command::Process { audio, reply: Some(tx) })
            .is_err()
        {
            return String::new();
        }
        rx.recv().unwrap_or_default()
    }

    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }
}

fn run(
    cfg: Arc<Mutex<Config>>,
    transcriber: Box<dyn Transcriber>,
    inject_ok: bool,
    cmd_rx: mpsc::Receiver<Command>,
    on_event: EventCallback,
) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("engine tokio runtime");
    let mut current: Option<Recorder> = None;

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Command::Start => match Recorder::start() {
                Ok(r) => {
                    current = Some(r);
                    on_event(EngineEvent::Recording);
                }
                Err(e) => on_event(EngineEvent::Error(format!("{e:#}"))),
            },
            Command::Stop { reply } => {
                let text = match current.take() {
                    Some(r) => process(&*transcriber, &rt, &cfg, inject_ok, &on_event, r.stop()),
                    None => String::new(),
                };
                if let Some(tx) = reply {
                    let _ = tx.send(text);
                }
            }
            Command::Process { audio, reply } => {
                let text = process(&*transcriber, &rt, &cfg, inject_ok, &on_event, audio);
                if let Some(tx) = reply {
                    let _ = tx.send(text);
                }
            }
            Command::Shutdown => break,
        }
    }
}

fn process(
    transcriber: &dyn Transcriber,
    rt: &tokio::runtime::Runtime,
    cfg: &Arc<Mutex<Config>>,
    inject_ok: bool,
    on_event: &EventCallback,
    audio: Vec<f32>,
) -> String {
    let secs = audio.len() as f32 / WHISPER_SAMPLE_RATE as f32;
    tracing::info!("processing {secs:.1}s clip");
    on_event(EngineEvent::Processing);

    let raw = match transcriber.transcribe(&audio) {
        Ok(t) => t,
        Err(e) => {
            on_event(EngineEvent::Error(format!("transcription failed: {e:#}")));
            on_event(EngineEvent::Idle);
            return String::new();
        }
    };

    // Reload from disk so edits made by the `--settings` process take effect
    // (enhancement toggle, Ollama settings). Falls back to the in-memory copy.
    let snapshot = Config::load().unwrap_or_else(|_| cfg.lock().unwrap().clone());
    let text = rt.block_on(enhance::maybe_enhance(&snapshot, raw));

    if inject_ok && !text.is_empty() {
        if let Err(e) = inject::type_text(&text) {
            on_event(EngineEvent::Error(format!("injection failed: {e:#}")));
        }
    }
    on_event(EngineEvent::Transcript(text.clone()));
    on_event(EngineEvent::Idle);
    text
}
