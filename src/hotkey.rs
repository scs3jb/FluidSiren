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
    // Portal trigger = the configured hotkey in canonical form (e.g. "Ctrl+Shift+D").
    let normalized = Shortcut::parse(&evdev_key)
        .map(|sc| sc.display())
        .unwrap_or_else(|| evdev_key.clone());
    let trigger = normalized.as_str();
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

    // evdev is single-key only; modifier combos are handled by the portal backend.
    let sc = Shortcut::parse(key_name).ok_or_else(|| anyhow!("invalid hotkey '{key_name}'"))?;
    if sc.has_modifier() {
        return Err(anyhow!("modifier combos use the portal backend, not evdev"));
    }
    let key: Key = key_from_name(&sc.key)
        .ok_or_else(|| anyhow!("evdev can't grab key '{}' (use the portal backend)", sc.key))?;

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

/// A parsed hotkey: optional modifiers + a base key (uppercased name, e.g. "F12",
/// "D", "SPACE"). Config stores the display form ("Ctrl+Shift+D", "F12").
#[derive(Default, Clone)]
pub struct Shortcut {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    pub key: String,
}

impl Shortcut {
    /// Parse "Ctrl+Shift+D", "F12", "Meta+Space", or a bare evdev name "KEY_F12".
    /// A single control character (Ctrl+letter capture) is decoded back to the letter.
    pub fn parse(s: &str) -> Option<Shortcut> {
        let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
        let (mods, key) = parts.split_at(parts.len().checked_sub(1)?);
        let mut sc = Shortcut::default();
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => sc.ctrl = true,
                "shift" => sc.shift = true,
                "alt" | "option" => sc.alt = true,
                "meta" | "super" | "win" | "logo" | "cmd" => sc.meta = true,
                _ => return None,
            }
        }
        let raw = key.first()?;
        let raw = raw.strip_prefix("KEY_").or_else(|| raw.strip_prefix("key_")).unwrap_or(raw);
        sc.key = normalize_base_key(raw);
        if sc.key.is_empty() {
            return None;
        }
        Some(sc)
    }

    /// Canonical display / config form, e.g. "Ctrl+Shift+D".
    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.meta {
            s.push_str("Meta+");
        }
        s.push_str(&display_key(&self.key));
        s
    }

    pub fn has_modifier(&self) -> bool {
        self.ctrl || self.shift || self.alt || self.meta
    }
}

/// Prettify the (uppercased) base key for display: letters/digits/F-keys as-is,
/// named keys title-cased (SPACE → Space, PAUSE → Pause).
fn display_key(key: &str) -> String {
    if key.chars().count() <= 1 {
        return key.to_string();
    }
    if key.starts_with('F') && key[1..].chars().all(|c| c.is_ascii_digit()) {
        return key.to_string();
    }
    let mut c = key.chars();
    let first = c.next().unwrap();
    format!("{first}{}", c.as_str().to_ascii_lowercase())
}

/// Normalize a base-key token: single printable char → uppercase; single control
/// char (1–26, i.e. Ctrl+letter) → its letter; named key → uppercased name.
fn normalize_base_key(k: &str) -> String {
    if k.chars().count() == 1 {
        let c = k.chars().next().unwrap();
        let code = c as u32;
        if (1..=26).contains(&code) {
            return ((b'A' + code as u8 - 1) as char).to_string();
        }
        return c.to_ascii_uppercase().to_string();
    }
    k.to_ascii_uppercase()
}

/// Bind the KDE GlobalShortcuts (portal) `toggle_dictation` shortcut to the given
/// hotkey string (e.g. "Ctrl+Shift+D"), via kglobalaccel's `setForeignShortcut`
/// — the same call System Settings makes. Makes a settings rebind / startup
/// self-bind take effect on a real keypress. Best-effort and non-fatal.
pub fn set_portal_shortcut(shortcut: &str) {
    let Some(qt) = Shortcut::parse(shortcut).and_then(|sc| qt_keycode(&sc)) else {
        tracing::warn!("no Qt keycode for hotkey '{shortcut}'; portal binding not updated");
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
        Ok(o) if o.status.success() => tracing::info!("portal shortcut set to {shortcut}"),
        Ok(o) => tracing::warn!(
            "setForeignShortcut failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => tracing::warn!("could not run gdbus to set portal shortcut: {e}"),
    }
}

/// `Qt::Key` value for a shortcut: base key OR'd with modifier flags.
fn qt_keycode(sc: &Shortcut) -> Option<i32> {
    let mut code = qt_base_key(&sc.key)?;
    if sc.shift {
        code |= 0x0200_0000;
    }
    if sc.ctrl {
        code |= 0x0400_0000;
    }
    if sc.alt {
        code |= 0x0800_0000;
    }
    if sc.meta {
        code |= 0x1000_0000;
    }
    Some(code)
}

/// `Qt::Key` for a base key name (no modifiers).
fn qt_base_key(key: &str) -> Option<i32> {
    // Function keys: Qt::Key_F1 = 0x01000030.
    if let Some(n) = key.strip_prefix('F').and_then(|s| s.parse::<i32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x0100_0030 + (n - 1));
        }
    }
    // Single ASCII letter / digit: Qt key == uppercase ASCII code.
    if key.chars().count() == 1 {
        let c = key.chars().next().unwrap();
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase() as i32);
        }
    }
    Some(match key {
        "SPACE" => 0x20,
        "TAB" => 0x0100_0001,
        "RETURN" | "ENTER" => 0x0100_0004,
        "ESC" | "ESCAPE" => 0x0100_0000,
        "BACKSPACE" => 0x0100_0003,
        "INSERT" => 0x0100_0006,
        "DELETE" => 0x0100_0007,
        "HOME" => 0x0100_0010,
        "END" => 0x0100_0011,
        "PAGEUP" => 0x0100_0016,
        "PAGEDOWN" => 0x0100_0017,
        "UP" => 0x0100_0013,
        "DOWN" => 0x0100_0015,
        "LEFT" => 0x0100_0012,
        "RIGHT" => 0x0100_0014,
        "PAUSE" => 0x0100_0008,
        "SCROLLLOCK" => 0x0100_0026,
        "PRINT" => 0x0100_0009,
        "MENU" => 0x0100_0055,
        _ => return None,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn disp(s: &str) -> String {
        Shortcut::parse(s).unwrap().display()
    }
    fn qt(s: &str) -> i32 {
        qt_keycode(&Shortcut::parse(s).unwrap()).unwrap()
    }

    #[test]
    fn parse_and_display() {
        assert_eq!(disp("F12"), "F12");
        assert_eq!(disp("KEY_F12"), "F12"); // legacy evdev name
        assert_eq!(disp("ctrl+shift+d"), "Ctrl+Shift+D");
        assert_eq!(disp("Meta+space"), "Meta+Space");
        assert_eq!(disp("super+win+d"), "Meta+D"); // super/win → meta, dedup
    }

    #[test]
    fn control_char_decodes_to_letter() {
        // Ctrl+D captured in-window arrives as the control char \u{4}.
        assert_eq!(disp("ctrl+\u{4}"), "Ctrl+D");
        assert_eq!(disp("ctrl+\u{1}"), "Ctrl+A");
    }

    #[test]
    fn qt_keycodes() {
        assert_eq!(qt("F1"), 0x0100_0030);
        assert_eq!(qt("F12"), 0x0100_003B); // 16777275
        assert_eq!(qt("D"), 0x44); // Qt::Key_D
        assert_eq!(qt("Ctrl+Shift+D"), 0x44 | 0x0400_0000 | 0x0200_0000);
        assert_eq!(qt("Meta+Space"), 0x20 | 0x1000_0000);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Shortcut::parse("Hyper+D").is_none()); // unknown modifier
        assert!(Shortcut::parse("").is_none());
        assert!(qt_keycode(&Shortcut::parse("ScrollLock").unwrap()).is_some());
    }

    #[test]
    fn evdev_only_single_keys() {
        let combo = Shortcut::parse("Ctrl+D").unwrap();
        assert!(combo.has_modifier());
        let single = Shortcut::parse("F12").unwrap();
        assert!(!single.has_modifier());
    }
}
