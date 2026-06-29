//! Global-hotkey driver (M2).
//!
//! Lets the user trigger dictation from anywhere on Wayland/Plasma, independent
//! of where focus is. Two trigger *modes* are supported:
//!
//!   * [`HotkeyMode::Toggle`]      — press once to start, press again to stop.
//!   * [`HotkeyMode::PushToTalk`]  — record while held, stop on release.
//!
//! and two *backends*:
//!
//!   1. [`HotkeyBackend::Portal`] — the XDG `GlobalShortcuts` portal via `ashpd`.
//!      This is the correct, sandbox-friendly Wayland mechanism. The compositor
//!      (KWin) owns the key grab; the user binds the *actual* key combo in
//!      **KDE System Settings → Shortcuts** (see module docs / the project README).
//!      We only register a named, abstract shortcut ("toggle_dictation").
//!
//!   2. [`HotkeyBackend::Evdev`] — raw `/dev/input/event*` reading via `evdev`.
//!      This is a true global key listener that works without any portal, and is
//!      the only backend that gives reliable *push-to-talk* key-up events today.
//!      It needs read access to the input devices — typically membership of the
//!      `input` group (`sudo usermod -aG input $USER` + re-login).
//!
//! The default backend is [`HotkeyBackend::Auto`]: try evdev first (a direct key
//! when /dev/input is readable), else the portal. Failures are logged, never fatal —
//! the rest of the app keeps working even if no hotkey could be installed.
//!
//! Entry points:
//!   * [`spawn`]      — convenience: `Auto` backend, default evdev key.
//!   * [`spawn_with`] — full control over backend + evdev key.
//!
//! Both return immediately with a [`HotkeyHandle`]; the listener runs on its own
//! thread (the portal backend drives a private current-thread tokio runtime, so
//! it does not depend on the caller already being inside a runtime).

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};

use crate::engine::DictationEngine;

/// The application-side identifier we register with the GlobalShortcuts portal.
/// KDE shows the `description` (below) to the user when they bind a key to it.
const SHORTCUT_ID: &str = "toggle_dictation";
const SHORTCUT_DESCRIPTION: &str = "Toggle / hold FluidSiren dictation";

/// How the key drives recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyMode {
    /// Press once to start, press again to stop.
    Toggle,
    /// Record while the key is held; stop on release.
    PushToTalk,
}

impl Default for HotkeyMode {
    fn default() -> Self {
        HotkeyMode::Toggle
    }
}

impl FromStr for HotkeyMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "toggle" => Ok(HotkeyMode::Toggle),
            "push_to_talk" | "ptt" | "hold" => Ok(HotkeyMode::PushToTalk),
            other => Err(anyhow!("unknown hotkey mode '{other}' (use 'toggle' or 'push_to_talk')")),
        }
    }
}

/// Which listener implementation to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyBackend {
    /// Try evdev (direct key), fall back to the portal. (Default.)
    Auto,
    /// XDG GlobalShortcuts portal only.
    Portal,
    /// Raw evdev `/dev/input` only.
    Evdev,
}

impl Default for HotkeyBackend {
    fn default() -> Self {
        HotkeyBackend::Auto
    }
}

impl FromStr for HotkeyBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Ok(HotkeyBackend::Auto),
            "portal" | "xdg" | "globalshortcuts" => Ok(HotkeyBackend::Portal),
            "evdev" | "input" => Ok(HotkeyBackend::Evdev),
            other => Err(anyhow!("unknown hotkey backend '{other}' (use 'auto', 'portal' or 'evdev')")),
        }
    }
}

/// Handle to a running hotkey listener. Dropping it (or calling [`stop`]) asks
/// the listener to wind down. Keep it alive for as long as the hotkey should
/// work — usually the whole lifetime of the app.
///
/// [`stop`]: HotkeyHandle::stop
pub struct HotkeyHandle {
    inner: Arc<Shared>,
    #[allow(dead_code)] // read by `stop()`; also keeps the controller thread joinable
    thread: Option<JoinHandle<()>>,
}

/// Shared state between the public handle and the listener thread(s).
struct Shared {
    /// Cleared to `false` to request shutdown. Listener loops observe this.
    running: AtomicBool,
    /// Wakes the async portal loop so it can shut down promptly.
    notify: tokio::sync::Notify,
}

impl HotkeyHandle {
    /// Request shutdown and join the controller thread.
    /// (`main` keeps the handle for the process lifetime; Drop also signals stop.)
    #[allow(dead_code)]
    pub fn stop(mut self) {
        self.signal_stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn signal_stop(&self) {
        self.inner.running.store(false, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        // Best-effort: detach rather than block on drop. The controller thread
        // observes `running == false` and exits; evdev reader threads exit after
        // their next event (blocking reads can't be interrupted portably).
        self.signal_stop();
    }
}

/// Convenience entry point: `Auto` backend, default evdev key (`KEY_F12`).
#[allow(dead_code)] // `main` uses `spawn_with`; this is the simple public entry
pub fn spawn(engine: DictationEngine, mode: HotkeyMode) -> Result<HotkeyHandle> {
    spawn_with(engine, mode, HotkeyBackend::Auto, "KEY_F12")
}

/// Spawn the hotkey listener with an explicit backend and evdev key name.
///
/// `evdev_key` is only used by the evdev backend (e.g. `"KEY_F12"`,
/// `"KEY_RIGHTCTRL"`); it is ignored by the portal backend, where the key is
/// chosen by the user in KDE System Settings.
///
/// Returns immediately; the listener runs on a background thread. An `Err` is
/// only returned for set-up problems we can detect synchronously (e.g. spawning
/// the thread). Backend-selection failures (portal missing, no input access) are
/// logged and — for `Auto` — fall through to the next backend.
pub fn spawn_with(
    engine: DictationEngine,
    mode: HotkeyMode,
    backend: HotkeyBackend,
    evdev_key: &str,
) -> Result<HotkeyHandle> {
    let inner = Arc::new(Shared {
        running: AtomicBool::new(true),
        notify: tokio::sync::Notify::new(),
    });
    let evdev_key = evdev_key.to_string();
    let thread_inner = inner.clone();

    let thread = std::thread::Builder::new()
        .name("hotkey-controller".into())
        .spawn(move || run_controller(thread_inner, engine, mode, backend, evdev_key))
        .map_err(|e| anyhow!("failed to spawn hotkey thread: {e}"))?;

    Ok(HotkeyHandle {
        inner,
        thread: Some(thread),
    })
}

/// Controller thread body: selects a backend and runs its listener.
fn run_controller(
    shared: Arc<Shared>,
    engine: DictationEngine,
    mode: HotkeyMode,
    backend: HotkeyBackend,
    evdev_key: String,
) {
    // Portal trigger derived from the configured key (e.g. "KEY_F12" → "F12").
    let trigger = evdev_key.strip_prefix("KEY_").unwrap_or(evdev_key.as_str());
    match backend {
        HotkeyBackend::Portal => {
            if let Err(e) = run_portal(&shared, &engine, mode, Some(trigger)) {
                tracing::error!("global-shortcuts portal hotkey unavailable: {e:#}");
            }
        }
        HotkeyBackend::Evdev => {
            if let Err(e) = run_evdev(&shared, &engine, mode, &evdev_key) {
                tracing::error!("evdev hotkey unavailable: {e:#}");
            }
        }
        HotkeyBackend::Auto => {
            // Try evdev first: when /dev/input is readable (you're in the `input`
            // group) it grabs the key directly and "just works" with no KDE setup.
            // Otherwise fall back to the GlobalShortcuts portal, which on KDE needs
            // a one-time binding in System Settings (the preferred trigger pre-fills
            // it) — the portal will not relay a key bound any other way.
            match run_evdev(&shared, &engine, mode, &evdev_key) {
                Ok(()) => {}
                Err(e_evdev) => {
                    if !shared.running.load(Ordering::SeqCst) {
                        return; // shutdown requested while probing evdev
                    }
                    tracing::info!(
                        "evdev hotkey unavailable ({e_evdev}); using the GlobalShortcuts portal. \
                         If the key does nothing, bind it in System Settings → Shortcuts → \
                         FluidSiren, or add yourself to the 'input' group for a direct hotkey."
                    );
                    if let Err(e_portal) = run_portal(&shared, &engine, mode, Some(trigger)) {
                        tracing::error!(
                            "no global hotkey available (evdev: {e_evdev}; portal: {e_portal:#})"
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Backend 1: XDG GlobalShortcuts portal (ashpd, async).
// ---------------------------------------------------------------------------

/// Run the portal listener to completion (blocks the controller thread).
///
/// Builds a private current-thread tokio runtime so it works whether or not the
/// caller is already inside a runtime. Returns `Err` if the portal can't be
/// reached or the shortcut can't be registered.
fn run_portal(
    shared: &Arc<Shared>,
    engine: &DictationEngine,
    mode: HotkeyMode,
    trigger: Option<&str>,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("portal tokio runtime: {e}"))?;
    rt.block_on(portal_loop(shared, engine, mode, trigger))
}

async fn portal_loop(
    shared: &Arc<Shared>,
    engine: &DictationEngine,
    mode: HotkeyMode,
    trigger: Option<&str>,
) -> Result<()> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;

    let shortcuts = GlobalShortcuts::new()
        .await
        .map_err(|e| anyhow!("connect to GlobalShortcuts portal: {e}"))?;

    // A session scopes our shortcut registrations and the Activated/Deactivated
    // signals we receive.
    let session = shortcuts
        .create_session()
        .await
        .map_err(|e| anyhow!("create portal session: {e}"))?;

    // Subscribe to the signal streams *before* binding, so we can't miss an
    // early activation.
    let mut activated = shortcuts
        .receive_activated()
        .await
        .map_err(|e| anyhow!("subscribe Activated: {e}"))?;
    let mut deactivated = shortcuts
        .receive_deactivated()
        .await
        .map_err(|e| anyhow!("subscribe Deactivated: {e}"))?;

    // Register the abstract shortcut with a preferred trigger (e.g. "F12") so KDE
    // binds it automatically on first registration. The user can still rebind it
    // in System Settings. Note: KDE only applies this when the shortcut has no
    // stored binding yet, so a previously-registered "none" must be cleared first.
    let new = NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION).preferred_trigger(trigger);
    let request = shortcuts
        .bind_shortcuts(&session, &[new], None)
        .await
        .map_err(|e| anyhow!("bind_shortcuts call: {e}"))?;
    // `.response()` errors if the user cancelled the (KDE: automatic) dialog.
    let bound = request
        .response()
        .map_err(|e| anyhow!("bind_shortcuts response: {e}"))?;
    tracing::info!(
        "registered GlobalShortcuts: {} shortcut(s)",
        bound.shortcuts().len(),
    );
    // Self-bind the configured key so the hotkey works out of the box. KDE's
    // portal otherwise leaves a registered shortcut unassigned (the user would
    // have to bind it in System Settings); `setForeignShortcut` is the same call
    // that UI makes, and a real keypress on the bound key then fires dictation.
    if let Some(t) = trigger {
        set_portal_shortcut(t);
    }

    // For toggle mode we track recording state ourselves; the portal only tells
    // us "the shortcut fired".
    let mut recording = false;

    loop {
        tokio::select! {
            // Shutdown requested via the handle.
            _ = shared.notify.notified() => {
                break;
            }
            ev = activated.next() => {
                match ev {
                    Some(a) if a.shortcut_id() == SHORTCUT_ID => {
                        match mode {
                            HotkeyMode::Toggle => {
                                recording = !recording;
                                if recording { engine.start(); } else { engine.stop(); }
                            }
                            HotkeyMode::PushToTalk => {
                                engine.start();
                            }
                        }
                    }
                    Some(_) => {} // some other shortcut id — ignore
                    None => break, // stream ended (portal went away)
                }
            }
            ev = deactivated.next() => {
                match ev {
                    Some(d) if d.shortcut_id() == SHORTCUT_ID => {
                        // Deactivated == key released. Only meaningful for
                        // push-to-talk; in toggle mode we drive everything from
                        // Activated and ignore the release.
                        if mode == HotkeyMode::PushToTalk {
                            engine.stop();
                        }
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        }

        if !shared.running.load(Ordering::SeqCst) {
            break;
        }
    }

    // Tidy up; ignore errors on the way out.
    let _ = session.close().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Backend 2: raw evdev /dev/input (true global key grab, real push-to-talk).
// ---------------------------------------------------------------------------

/// Run the evdev listener. Spawns one reader thread per keyboard device that
/// reports the target key, then blocks the controller thread until shutdown.
fn run_evdev(
    shared: &Arc<Shared>,
    engine: &DictationEngine,
    mode: HotkeyMode,
    key_name: &str,
) -> Result<()> {
    use evdev::Key;

    let key: Key = key_from_name(key_name)
        .ok_or_else(|| anyhow!("unknown evdev key name '{key_name}'"))?;

    // Find devices that can emit our key. Reading /dev/input needs `input` group
    // membership (or root); enumerate() silently skips devices we can't open.
    let mut readers = 0usize;
    // Shared recording flag for toggle mode, shared across all device threads so
    // pressing the key on any keyboard toggles the same state.
    let recording = Arc::new(AtomicBool::new(false));

    for (path, device) in evdev::enumerate() {
        let supports = device
            .supported_keys()
            .map(|keys| keys.contains(key))
            .unwrap_or(false);
        if !supports {
            continue;
        }

        let shared = shared.clone();
        let engine = engine.clone();
        let recording = recording.clone();
        let path_dbg = path.clone();
        let spawn_res = std::thread::Builder::new()
            .name("hotkey-evdev".into())
            .spawn(move || evdev_reader(shared, engine, mode, key, recording, device));
        match spawn_res {
            Ok(_) => {
                tracing::info!("evdev hotkey: watching {} for {:?}", path_dbg.display(), key);
                readers += 1;
            }
            Err(e) => tracing::warn!("evdev: could not spawn reader for {}: {e}", path_dbg.display()),
        }
    }

    if readers == 0 {
        return Err(anyhow!(
            "no readable input device exposes {key_name}. Add yourself to the 'input' group \
             (sudo usermod -aG input $USER) and re-login, or pick a key your keyboard has."
        ));
    }

    // Park the controller thread until shutdown is requested. The detached
    // reader threads do the real work; they exit after their next event once
    // `running` is false.
    while shared.running.load(Ordering::SeqCst) {
        std::thread::park_timeout(std::time::Duration::from_millis(250));
    }
    Ok(())
}

/// Bind the KDE GlobalShortcuts (portal) `toggle_dictation` shortcut to the given
/// evdev key, via kglobalaccel's `setForeignShortcut` — the same call System
/// Settings makes. This makes a settings "Rebind" take effect for portal users
/// (the captured key then triggers dictation on a real keypress). Best-effort and
/// non-fatal; evdev users get the same key from the config + live re-bind.
pub fn set_portal_shortcut(evdev_name: &str) {
    let Some(qt) = qt_keycode_for(evdev_name) else {
        tracing::warn!("no Qt keycode for {evdev_name}; portal binding not updated");
        return;
    };
    let action = format!(
        "['dev.altic.FluidSiren','{SHORTCUT_ID}','FluidSiren','{SHORTCUT_DESCRIPTION}']"
    );
    let result = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.kde.kglobalaccel",
            "--object-path",
            "/kglobalaccel",
            "--method",
            "org.kde.KGlobalAccel.setForeignShortcut",
            &action,
            &format!("[{qt}]"),
        ])
        .output();
    match result {
        Ok(o) if o.status.success() => tracing::info!("portal shortcut set to {evdev_name}"),
        Ok(o) => tracing::warn!(
            "setForeignShortcut failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => tracing::warn!("could not run gdbus to set portal shortcut: {e}"),
    }
}

/// Qt key code (`Qt::Key`) for an evdev key name, for `setForeignShortcut`.
/// Function keys only (the settings capture box offers F1–F12).
fn qt_keycode_for(evdev_name: &str) -> Option<i32> {
    let up = evdev_name.strip_prefix("KEY_").unwrap_or(evdev_name);
    let n: i32 = up.strip_prefix('F')?.parse().ok()?;
    if (1..=24).contains(&n) {
        Some(0x0100_002F + n) // Qt::Key_F1 = 0x01000030
    } else {
        None
    }
}

/// Per-device blocking read loop. `fetch_events` blocks until input arrives, so
/// each device gets its own thread.
fn evdev_reader(
    shared: Arc<Shared>,
    engine: DictationEngine,
    mode: HotkeyMode,
    key: evdev::Key,
    recording: Arc<AtomicBool>,
    mut device: evdev::Device,
) {
    use evdev::InputEventKind;

    while shared.running.load(Ordering::SeqCst) {
        let events = match device.fetch_events() {
            Ok(ev) => ev,
            Err(e) => {
                // EAGAIN shouldn't happen on a blocking fd, but a device can be
                // unplugged. Log once and stop reading this device.
                tracing::warn!("evdev: device read error, dropping reader: {e}");
                return;
            }
        };

        for event in events {
            if !matches!(event.kind(), InputEventKind::Key(k) if k == key) {
                continue;
            }
            // value: 1 = press (key-down), 0 = release (key-up), 2 = autorepeat.
            match event.value() {
                1 => match mode {
                    HotkeyMode::Toggle => {
                        // Flip shared state: true->recording started.
                        let was = recording.fetch_xor(true, Ordering::SeqCst);
                        if was {
                            engine.stop();
                        } else {
                            engine.start();
                        }
                    }
                    HotkeyMode::PushToTalk => engine.start(),
                },
                0 => {
                    if mode == HotkeyMode::PushToTalk {
                        engine.stop();
                    }
                }
                _ => {} // ignore autorepeat
            }
        }
    }
}

/// Map a small set of human key names to `evdev::Key`. Covers the keys people
/// actually pick for a dictation hotkey (function keys, modifiers, and a few
/// odds). Returns `None` for anything unrecognised.
fn key_from_name(name: &str) -> Option<evdev::Key> {
    use evdev::Key;
    // Accept "F12", "KEY_F12", "key_f12" etc.
    let up = name.trim().to_ascii_uppercase();
    let up = up.strip_prefix("KEY_").unwrap_or(&up);
    Some(match up {
        "F1" => Key::KEY_F1,
        "F2" => Key::KEY_F2,
        "F3" => Key::KEY_F3,
        "F4" => Key::KEY_F4,
        "F5" => Key::KEY_F5,
        "F6" => Key::KEY_F6,
        "F7" => Key::KEY_F7,
        "F8" => Key::KEY_F8,
        "F9" => Key::KEY_F9,
        "F10" => Key::KEY_F10,
        "F11" => Key::KEY_F11,
        "F12" => Key::KEY_F12,
        "RIGHTCTRL" | "RCTRL" => Key::KEY_RIGHTCTRL,
        "LEFTCTRL" | "LCTRL" => Key::KEY_LEFTCTRL,
        "RIGHTALT" | "RALT" | "ALTGR" => Key::KEY_RIGHTALT,
        "LEFTALT" | "LALT" => Key::KEY_LEFTALT,
        "RIGHTSHIFT" | "RSHIFT" => Key::KEY_RIGHTSHIFT,
        "LEFTSHIFT" | "LSHIFT" => Key::KEY_LEFTSHIFT,
        "RIGHTMETA" | "RMETA" | "RSUPER" | "RWIN" => Key::KEY_RIGHTMETA,
        "LEFTMETA" | "LMETA" | "LSUPER" | "LWIN" | "SUPER" => Key::KEY_LEFTMETA,
        "CAPSLOCK" | "CAPS" => Key::KEY_CAPSLOCK,
        "SCROLLLOCK" => Key::KEY_SCROLLLOCK,
        "PAUSE" => Key::KEY_PAUSE,
        "INSERT" | "INS" => Key::KEY_INSERT,
        "MENU" | "COMPOSE" => Key::KEY_COMPOSE,
        _ => return None,
    })
}
