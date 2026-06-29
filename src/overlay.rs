//! Controls the external `fluidsiren-overlay` process — the bottom-center
//! recording overlay (GTK4 + wlr-layer-shell, which Slint/winit can't do here).
//!
//! It's spawned while recording and killed when idle, so it "disappears when not
//! recording". Status updates are sent over the child's stdin.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

#[derive(Default)]
pub struct OverlayController {
    running: Mutex<Option<Running>>,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
}

impl OverlayController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Path to the overlay binary, installed alongside the main binary.
    fn bin() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("fluidsiren-overlay"))
    }

    /// Show the overlay (spawn if needed) in the "recording" state.
    pub fn show_recording(&self) {
        let mut guard = self.running.lock().unwrap();
        if let Some(r) = guard.as_mut() {
            let _ = writeln!(r.stdin, "recording");
            let _ = r.stdin.flush();
            return;
        }
        let Some(bin) = Self::bin() else { return };
        match Command::new(&bin).stdin(Stdio::piped()).spawn() {
            Ok(mut child) => match child.stdin.take() {
                Some(stdin) => *guard = Some(Running { child, stdin }),
                None => {
                    let _ = child.kill();
                }
            },
            Err(e) => tracing::warn!("overlay spawn failed ({}): {e}", bin.display()),
        }
    }

    /// Switch the (already shown) overlay to the "transcribing" state.
    pub fn set_transcribing(&self) {
        let mut guard = self.running.lock().unwrap();
        if let Some(r) = guard.as_mut() {
            let _ = writeln!(r.stdin, "transcribing");
            let _ = r.stdin.flush();
        }
    }

    /// Hide the overlay (it exits and disappears).
    pub fn hide(&self) {
        let mut guard = self.running.lock().unwrap();
        if let Some(mut r) = guard.take() {
            let _ = writeln!(r.stdin, "quit");
            let _ = r.stdin.flush();
            let _ = r.child.kill(); // ensure it's gone even if it ignored stdin
            let _ = r.child.wait(); // reap
        }
    }
}
