//! Local Ollama server lifecycle — managed strictly through systemd.
//!
//! FluidSiren never spawns a bare `ollama serve` of its own. Starting and
//! stopping the server always go through the managing systemd unit (a
//! `systemctl --user` unit, or the system `ollama.service`). That keeps control
//! in one place — the unit can be driven by `systemctl` directly or by
//! FluidSiren's settings — and never leaves a stray process behind. If no unit is
//! installed, `scripts/setup-ollama.sh --user` creates one (started, not enabled,
//! so it does not autostart at login).
//!
//! Reachability (is the server actually answering?) is a separate question
//! handled by [`crate::enhance::is_available`]; this module only manages the unit.

use anyhow::Context;
use std::process::{Command, Stdio};

/// How Ollama is managed on this machine.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Management {
    /// A `systemctl --user` unit owns it (runs as us; no privilege needed).
    UserUnit,
    /// A system `systemctl` unit owns it (runs as the `ollama` user; start/stop
    /// goes through polkit).
    SystemUnit,
    /// No systemd unit is installed — we will not start a bare process.
    None,
}

/// True if the `ollama` binary is on `PATH`.
pub fn installed() -> bool {
    Command::new("ollama")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decide how Ollama is managed, preferring a user unit, then a system unit.
/// Both checks are read-only and need no privilege.
fn management() -> Management {
    if unit_exists(&["--user"]) {
        Management::UserUnit
    } else if unit_exists(&[]) {
        Management::SystemUnit
    } else {
        Management::None
    }
}

/// Whether `ollama.service` exists in the given systemctl scope (`&["--user"]`
/// for the user manager, `&[]` for the system manager).
fn unit_exists(scope: &[&str]) -> bool {
    Command::new("systemctl")
        .args(scope)
        .args(["list-unit-files", "ollama.service"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("ollama.service"))
        .unwrap_or(false)
}

/// Start the local Ollama server via its systemd unit. Errors (rather than
/// spawning a bare process) when no unit is installed, pointing at the setup
/// script. Starting an already-running unit is a harmless no-op.
pub fn start() -> anyhow::Result<()> {
    match management() {
        Management::UserUnit => systemctl(&["--user"], "start"),
        Management::SystemUnit => systemctl(&[], "start"),
        Management::None => anyhow::bail!(
            "no ollama systemd unit found — run `scripts/setup-ollama.sh --user` to create one"
        ),
    }
}

/// Stop the local Ollama server via its systemd unit.
///
/// For a system unit this runs `systemctl stop ollama`, which a desktop polkit
/// agent (KDE/Plasma ships one) turns into an authentication prompt. With no unit
/// there is nothing we manage, so this is a no-op.
pub fn stop() -> anyhow::Result<()> {
    match management() {
        Management::UserUnit => systemctl(&["--user"], "stop"),
        Management::SystemUnit => systemctl(&[], "stop"),
        Management::None => Ok(()),
    }
}

/// Run `systemctl [scope] <action> ollama`, capturing output so a failure carries
/// systemd's own message (e.g. "Access denied", "Interactive authentication
/// required") instead of vanishing.
fn systemctl(scope: &[&str], action: &str) -> anyhow::Result<()> {
    let out = Command::new("systemctl")
        .args(scope)
        .arg(action)
        .arg("ollama")
        .stdin(Stdio::null())
        .output()
        .context("running systemctl")?;
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stderr);
    let msg = msg.trim();
    let scope_label = if scope.is_empty() { "system" } else { "--user" };
    anyhow::bail!(
        "systemctl {scope_label} {action} ollama failed{}{}",
        if msg.is_empty() { "" } else { ": " },
        if msg.is_empty() {
            "(the system ollama.service may need privileges — try: sudo systemctl stop ollama)".to_string()
        } else {
            msg.to_string()
        }
    );
}
