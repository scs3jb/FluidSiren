//! Slint UI layer for FluidSiren.
//!
//! Three pieces, all driven by the same `DictationEngine`:
//!   * a KDE StatusNotifierItem **tray** (via `ksni`, blocking API) whose icon /
//!     tooltip reflect state and whose menu starts/stops dictation, opens
//!     settings and quits;
//!   * a **settings window** that edits the shared `Config` and persists it;
//!   * a frameless, on-top **overlay** that shows live state + transcript.
//!
//! Threading model: Slint owns the **main thread** event loop. The engine emits
//! events on its own thread; we forward them to the UI thread with
//! `slint::invoke_from_event_loop` (overlay) and to the tray's background thread
//! with the `ksni` handle (`handle.update`). The tray's own menu callbacks run on
//! the tray thread and reach the UI with `Weak::upgrade_in_event_loop`.

use crate::config::Config;
use crate::engine::{DictationEngine, EngineEvent, EventCallback};
use std::sync::{Arc, Mutex};

use ksni::blocking::TrayMethods; // brings `.spawn()` into scope
use slint::ComponentHandle;

// Pull in the Rust generated from `ui/main.slint` (Overlay + Settings).
slint::include_modules!();

/// Coarse state used to drive the tray icon / tooltip and overlay dot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrayState {
    Idle,
    Recording,
    Processing,
}

impl TrayState {
    fn label(self) -> &'static str {
        match self {
            TrayState::Idle => "Idle",
            TrayState::Recording => "Recording",
            TrayState::Processing => "Transcribing",
        }
    }

    /// Freedesktop icon names present in standard KDE/Plasma icon themes.
    fn icon(self) -> &'static str {
        match self {
            TrayState::Idle => "audio-input-microphone",
            TrayState::Recording => "media-record",
            TrayState::Processing => "view-refresh",
        }
    }
}

/// The StatusNotifierItem. Lives on the `ksni` background thread; its menu
/// `activate` closures receive `&mut Self` and bridge back to the Slint loop.
struct FluidTray {
    state: TrayState,
    engine: DictationEngine,
}


impl ksni::Tray for FluidTray {
    fn id(&self) -> String {
        "fluidsiren".into()
    }

    fn title(&self) -> String {
        "FluidSiren".into()
    }

    fn icon_name(&self) -> String {
        self.state.icon().into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "FluidSiren".into(),
            description: format!("Status: {}", self.state.label()),
            icon_name: self.state.icon().into(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let recording = self.state != TrayState::Idle;
        vec![
            StandardItem {
                label: if recording {
                    "Stop dictation".into()
                } else {
                    "Start dictation".into()
                },
                icon_name: if recording {
                    "media-playback-stop".into()
                } else {
                    "media-record".into()
                },
                activate: Box::new(|this: &mut Self| {
                    if this.state == TrayState::Idle {
                        this.engine.start();
                    } else {
                        this.engine.stop();
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Settings…".into(),
                icon_name: "configure".into(),
                activate: Box::new(|_this: &mut Self| {
                    // Launch the settings UI as a separate process: winit/Wayland
                    // can't map a window shown after this loop started, but a fresh
                    // process shows its window before its own loop, so it maps.
                    match std::env::current_exe() {
                        Ok(exe) => {
                            if let Err(e) = std::process::Command::new(exe).arg("--settings").spawn() {
                                tracing::error!("[tray] could not launch settings: {e}");
                            }
                        }
                        Err(e) => tracing::error!("[tray] current_exe failed: {e}"),
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| {
                    this.engine.shutdown();
                    let _ = slint::quit_event_loop();
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Build the whole UI, wire it to a freshly-built engine, and run the Slint
/// event loop until the user quits. Blocks the calling (main) thread.
///
/// `build_engine` is handed the event callback and must return the engine; this
/// lets `main` own model loading while the UI owns the callback wiring (the
/// overlay handle must exist *before* the engine, so the callback can capture
/// it).
pub fn run(
    cfg: Arc<Mutex<Config>>,
    build_engine: impl FnOnce(EventCallback) -> DictationEngine,
) -> anyhow::Result<()> {
    // Settings runs as a separate `--settings` process (see `run_settings`)
    // because winit's Wayland backend can't map a window shown after the event
    // loop starts. `cfg` is unused here now; the engine reads config from disk per
    // transcription so it sees the settings process's edits.
    let _ = &cfg;

    // 2. A slot for the tray handle. Breaks the cycle: the engine callback needs
    //    the handle to refresh the tray, but the tray needs the engine.
    let tray_slot: Arc<Mutex<Option<ksni::blocking::Handle<FluidTray>>>> =
        Arc::new(Mutex::new(None));

    // 3. Engine, built with a callback that updates the tray icon/tooltip and the
    //    bottom-center recording overlay (a separate GTK4 layer-shell process).
    let cb_slot = tray_slot.clone();
    let overlay = std::sync::Arc::new(crate::overlay::OverlayController::new());
    let callback: EventCallback = Box::new(move |ev: EngineEvent| {
        let tray_state = match &ev {
            EngineEvent::Recording => TrayState::Recording,
            EngineEvent::Processing | EngineEvent::Transcript(_) => TrayState::Processing,
            EngineEvent::Idle | EngineEvent::Error(_) => TrayState::Idle,
        };
        if let Some(h) = cb_slot.lock().unwrap().as_ref() {
            h.update(|t| t.state = tray_state);
        }
        match &ev {
            EngineEvent::Recording => overlay.show_recording(),
            EngineEvent::Processing => overlay.set_transcribing(),
            EngineEvent::Idle | EngineEvent::Error(_) => overlay.hide(),
            EngineEvent::Transcript(_) => {}
        }
    });

    let engine = build_engine(callback);

    // 4. Spawn the tray (its own background thread; non-fatal if SNI is absent).
    let tray = FluidTray {
        state: TrayState::Idle,
        engine: engine.clone(),
    };
    match tray.spawn() {
        Ok(h) => *tray_slot.lock().unwrap() = Some(h),
        Err(e) => eprintln!(
            "FluidSiren: system tray unavailable ({e}). \
             Dictation still works via the global hotkey."
        ),
    }

    // 5. Run until the tray's Quit calls quit_event_loop(). No window is shown:
    //    the app lives in the tray. (A visible live-preview overlay isn't possible
    //    here yet — winit/Wayland can't map a window shown after the loop starts,
    //    nor resize/hide a mapped one — future work: layer-shell or a subprocess.)
    slint::run_event_loop()?;

    engine.shutdown();
    Ok(())
}

/// Read the settings form into the shared config and persist it to disk.
fn persist_settings(s: &Settings, cfg: &Arc<Mutex<Config>>) {
    let mut c = cfg.lock().unwrap();
    c.provider = s.get_provider().to_string();
    c.whisper_model = s.get_whisper_model().to_string();
    c.language = s.get_language().to_string();
    c.enhance = s.get_enhance();
    c.ollama_model = s.get_ollama_model().to_string();
    c.ollama_url = s.get_ollama_url().to_string();
    c.hotkey_key = s.get_hotkey_key().to_string();
    if let Err(e) = c.save() {
        eprintln!("FluidSiren: failed to save config: {e:#}");
    }
}

/// Run ONLY the settings window — the `fluidsiren --settings` subprocess. Its
/// window is shown *before* this process's event loop, so it maps correctly even
/// though the main app cannot show a window on demand. Edits are written to the
/// shared config file; the main app reloads config from disk per transcription.
pub fn run_settings(cfg: Arc<Mutex<Config>>) -> anyhow::Result<()> {
    let settings = Settings::new()?;
    {
        let c = cfg.lock().unwrap();
        settings.set_provider(c.provider.clone().into());
        settings.set_whisper_model(c.whisper_model.clone().into());
        settings.set_language(c.language.clone().into());
        settings.set_enhance(c.enhance);
        settings.set_ollama_model(c.ollama_model.clone().into());
        settings.set_ollama_url(c.ollama_url.clone().into());
        settings.set_hotkey_key(c.hotkey_key.clone().into());
    }
    // Save → write the form into the config file.
    {
        let weak = settings.as_weak();
        let cfg = cfg.clone();
        settings.on_save(move || {
            if let Some(s) = weak.upgrade() {
                persist_settings(&s, &cfg);
            }
            let _ = slint::quit_event_loop(); // Save closes the window
        });
    }
    // Captured (in-window keypress) → store the new key, persist it (the running
    // app re-binds evdev live), and update the KDE portal binding so it also takes
    // effect for portal users.
    {
        let weak = settings.as_weak();
        let cfg = cfg.clone();
        settings.on_captured(move |raw| {
            // Canonicalize ("ctrl+d" / control chars → "Ctrl+D").
            let shortcut = crate::hotkey::Shortcut::parse(raw.as_str())
                .map(|sc| sc.display())
                .unwrap_or_else(|| raw.to_string());
            if let Some(s) = weak.upgrade() {
                s.set_hotkey_key(shortcut.clone().into());
                persist_settings(&s, &cfg);
            }
            crate::hotkey::set_portal_shortcut(&shortcut);
        });
    }
    // Close button → quit this process.
    settings.on_close_requested(|| {
        let _ = slint::quit_event_loop();
    });
    // Titlebar close (X) → also quit.
    settings.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });

    settings.show()?; // shown before run_event_loop → maps correctly
    slint::run_event_loop()?;
    Ok(())
}
