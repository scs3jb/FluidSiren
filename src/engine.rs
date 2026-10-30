//! DictationEngine — the core capture→transcribe→enhance→inject pipeline, run on
//! a dedicated thread so it can own the non-`Send` cpal stream and the whisper
//! context. Drivers (CLI, global hotkey, UI) talk to it through a cheap,
//! `Clone`-able, `Send` handle by sending commands; it reports progress through
//! an event callback.
//!
//! While recording, the engine wakes every [`TICK`] and, once the speaker has
//! paused after a few seconds of speech, transcribes the finished phrase in the
//! background ("incremental" mode). On stop only the last phrase is left, so a
//! long dictation is typed almost as soon as the hotkey is released.

use crate::asr::Transcriber;
use crate::audio::{self, Recorder, WHISPER_SAMPLE_RATE};
use crate::config::Config;
use crate::vad::Activity;
use crate::{enhance, inject};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the engine checks a live recording for a finished phrase.
const TICK: Duration = Duration::from_millis(300);
/// Don't split off a phrase shorter than this: the models are more accurate
/// with some context, and short clips gain nothing from early processing.
const MIN_SEGMENT_SECS: f32 = 5.0;
/// A pause at least this long ends a phrase.
const PHRASE_PAUSE_SECS: f32 = 0.6;

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
    /// Transcribe a 16 kHz clip. `live` replays it as if it were being recorded
    /// (incremental segmentation included), for benchmarking the live path.
    Process { audio: Vec<f32>, live: bool, reply: Option<Sender<String>> },
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
    pub fn process_blocking(&self, audio: Vec<f32>, live: bool) -> String {
        let (tx, rx) = mpsc::channel();
        if self
            .cmd_tx
            .send(Command::Process { audio, live, reply: Some(tx) })
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

/// An in-progress recording plus whatever has already been transcribed from it.
struct Session {
    rec: Recorder,
    live: Incremental,
    incremental: bool,
}

/// Incremental transcription state over a growing native-rate buffer.
#[derive(Default)]
struct Incremental {
    /// Samples (native rate) already transcribed into `parts`.
    consumed: usize,
    parts: Vec<String>,
}

impl Incremental {
    /// Given everything recorded after `consumed`, transcribe up to the last
    /// phrase-ending pause if enough audio has built up. Errors are left for the
    /// final pass to retry (the audio stays unconsumed).
    fn advance(&mut self, transcriber: &dyn Transcriber, pending: &[f32], rate: u32) {
        if (pending.len() as f32) < MIN_SEGMENT_SECS * rate as f32 {
            return;
        }
        let Some(cut) = Activity::analyze(pending, rate).last_pause_cut(rate, PHRASE_PAUSE_SECS) else {
            return;
        };
        if (cut as f32) < MIN_SEGMENT_SECS * rate as f32 {
            return;
        }
        match transcribe_clip(transcriber, &pending[..cut], rate) {
            Ok(text) => {
                tracing::debug!("phrase transcribed early: {text:?}");
                self.parts.extend(text);
                self.consumed += cut;
            }
            Err(e) => tracing::warn!("incremental transcription failed (will retry at stop): {e:#}"),
        }
    }

    /// Transcribe the remaining tail and join it with the earlier phrases.
    fn finish(mut self, transcriber: &dyn Transcriber, tail: &[f32], rate: u32) -> anyhow::Result<String> {
        self.parts.extend(transcribe_clip(transcriber, tail, rate)?);
        Ok(self.parts.join(" "))
    }
}

/// Trim silence, resample to 16 kHz and transcribe. `Ok(None)` when the clip
/// holds no speech — skipping the model entirely, which is both faster and
/// avoids Whisper hallucinating text ("Thank you.") from silence.
fn transcribe_clip(transcriber: &dyn Transcriber, raw: &[f32], rate: u32) -> anyhow::Result<Option<String>> {
    let Some(bounds) = Activity::analyze(raw, rate).speech_bounds(rate) else {
        return Ok(None);
    };
    let clip = audio::resample(&raw[bounds], rate, WHISPER_SAMPLE_RATE);
    let text = transcriber.transcribe(&clip)?;
    Ok(Some(text).filter(|t| !t.is_empty()))
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
    let warm = std::time::Instant::now();
    transcriber.warm_up();
    tracing::info!("speech model warmed up in {:.2}s", warm.elapsed().as_secs_f32());
    let mut current: Option<Session> = None;

    loop {
        // While recording incrementally, wake periodically to process phrases.
        let cmd = match current.as_mut().filter(|s| s.incremental) {
            Some(session) => match cmd_rx.recv_timeout(TICK) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    let pending = session.rec.snapshot(session.live.consumed);
                    session.live.advance(&*transcriber, &pending, session.rec.src_rate());
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match cmd_rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            },
        };
        match cmd {
            Command::Start => match Recorder::start() {
                Ok(rec) => {
                    let incremental = Config::load().map(|c| c.incremental).unwrap_or(true);
                    current = Some(Session { rec, live: Incremental::default(), incremental });
                    on_event(EngineEvent::Recording);
                }
                Err(e) => on_event(EngineEvent::Error(format!("{e:#}"))),
            },
            Command::Stop { reply } => {
                let text = match current.take() {
                    Some(Session { rec, live, .. }) => {
                        on_event(EngineEvent::Processing);
                        let rate = rec.src_rate();
                        let raw = rec.stop_raw();
                        let tail = raw.get(live.consumed..).unwrap_or_default();
                        let result = live.finish(&*transcriber, tail, rate);
                        finish(result, &rt, &cfg, inject_ok, &on_event)
                    }
                    None => String::new(),
                };
                if let Some(tx) = reply {
                    let _ = tx.send(text);
                }
            }
            Command::Process { audio, live, reply } => {
                on_event(EngineEvent::Processing);
                let result = if live {
                    replay_live(&*transcriber, &audio)
                } else {
                    transcribe_clip(&*transcriber, &audio, WHISPER_SAMPLE_RATE).map(Option::unwrap_or_default)
                };
                let text = finish(result, &rt, &cfg, inject_ok, &on_event);
                if let Some(tx) = reply {
                    let _ = tx.send(text);
                }
            }
            Command::Shutdown => break,
        }
    }
}

/// Feed a clip through the incremental path one tick of audio at a time, as a
/// live recording would arrive (without the real-time waits). Logs how long the
/// final pass took — the latency a user would see after releasing the hotkey.
fn replay_live(transcriber: &dyn Transcriber, audio: &[f32]) -> anyhow::Result<String> {
    let step = (TICK.as_secs_f32() * WHISPER_SAMPLE_RATE as f32) as usize;
    let mut live = Incremental::default();
    let mut recorded = 0;
    while recorded < audio.len() {
        recorded = (recorded + step).min(audio.len());
        live.advance(transcriber, &audio[live.consumed..recorded], WHISPER_SAMPLE_RATE);
    }
    let started = std::time::Instant::now();
    let tail = &audio[live.consumed..];
    let text = live.finish(transcriber, tail, WHISPER_SAMPLE_RATE);
    tracing::info!("live replay: final pass took {:.3}s", started.elapsed().as_secs_f32());
    text
}

/// Enhance and inject a finished transcription, emitting the closing events.
fn finish(
    result: anyhow::Result<String>,
    rt: &tokio::runtime::Runtime,
    cfg: &Arc<Mutex<Config>>,
    inject_ok: bool,
    on_event: &EventCallback,
) -> String {
    let raw = match result {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Reports the length of each clip it is given, in tenths of a second.
    struct LengthTranscriber;
    impl Transcriber for LengthTranscriber {
        fn transcribe(&self, audio: &[f32]) -> anyhow::Result<String> {
            Ok(format!("{}", audio.len() / (WHISPER_SAMPLE_RATE as usize / 10)))
        }
    }

    fn phrase(secs: f32) -> Vec<f32> {
        (0..(secs * WHISPER_SAMPLE_RATE as f32) as usize)
            .map(|i| 0.3 * (i as f32 * 0.09).sin())
            .collect()
    }

    fn pause(secs: f32) -> Vec<f32> {
        vec![0.0005; (secs * WHISPER_SAMPLE_RATE as f32) as usize]
    }

    #[test]
    fn silence_is_never_transcribed() {
        let out = transcribe_clip(&LengthTranscriber, &pause(3.0), WHISPER_SAMPLE_RATE).unwrap();
        assert_eq!(out, None);
    }

    #[test]
    fn live_replay_splits_long_dictation_at_pauses() {
        let mut audio = pause(0.5);
        for _ in 0..3 {
            audio.extend(phrase(4.0));
            audio.extend(pause(1.0));
        }
        let text = replay_live(&LengthTranscriber, &audio).unwrap();
        let parts: Vec<&str> = text.split(' ').collect();
        assert!(parts.len() >= 2, "expected early phrases, got {text:?}");
        // Every phrase is covered: total transcribed audio ≥ the 12s of speech.
        let total: usize = parts.iter().map(|p| p.parse::<usize>().unwrap()).sum();
        assert!(total >= 120, "{text:?}");
    }

    #[test]
    fn short_dictation_is_one_clip() {
        let mut audio = phrase(3.0);
        audio.extend(pause(1.0));
        let text = replay_live(&LengthTranscriber, &audio).unwrap();
        assert!(!text.contains(' '), "{text:?}");
    }
}
