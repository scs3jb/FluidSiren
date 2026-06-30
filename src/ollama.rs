//! Local Ollama server lifecycle.
//!
//! FluidSiren can optionally clean transcripts with a local Ollama model (see
//! [`crate::enhance`]). This module owns *running* that server: detecting how it
//! is managed, starting it, and stopping it. Every operation is best-effort and
//! never panics — enhancement degrades gracefully when Ollama is absent, so a
//! failure here only means transcripts stay un-enhanced.
//!
//! The official Linux installer registers a **system** `ollama.service` running
//! as the `ollama` user; some setups use a **systemd --user** unit; others run a
//! bare `ollama serve`. We detect which and route start/stop through the matching
//! mechanism — notably, a non-root user cannot `kill` the system service's
//! process, so stopping it must go through `systemctl` (which prompts via polkit).
//!
//! Reachability (is the server actually answering?) is a separate question
//! handled by [`crate::enhance::is_available`]; this module only manages the
//! process.

use anyhow::Context;
use std::process::{Command, Stdio};

/// How Ollama is managed on this machine.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Management {
    /// A `systemctl --user` unit owns it.
    UserUnit,
    /// A system `systemctl` unit owns it (runs as the `ollama` user; stopping it
    /// needs privilege and goes through polkit).
    SystemUnit,
    /// No unit — a bare `ollama serve` process we start/stop directly.
    Bare,
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

/// Decide how Ollama is managed, preferring a user unit, then a system unit, then
/// a bare process. Both unit checks are read-only and need no privilege.
fn management() -> Management {
    if unit_exists(&["--user"]) {
        Management::UserUnit
    } else if unit_exists(&[]) {
        Management::SystemUnit
    } else {
        Management::Bare
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

/// Start the local Ollama server if it isn't already running. Routes through the
/// managing unit when there is one; otherwise spawns a detached `ollama serve`.
/// Starting an already-running server is harmless, so callers may call this
/// unconditionally.
pub fn start() -> anyhow::Result<()> {
    if !installed() {
        anyhow::bail!("ollama is not installed (run scripts/setup-ollama.sh)");
    }
    match management() {
        Management::UserUnit => systemctl(&["--user"], "start"),
        Management::SystemUnit => systemctl(&[], "start"),
        Management::Bare => spawn_detached(),
    }
}

/// Stop the local Ollama server via whatever manages it.
///
/// For a system unit this runs `systemctl stop ollama`, which a desktop polkit
/// agent (KDE/Plasma ships one) turns into an authentication prompt — a non-root
/// process cannot kill the `ollama`-user service directly, so this is the only
/// way that works. Errors (e.g. polkit denied / no agent) are propagated so the
/// caller can tell the user instead of silently doing nothing.
pub fn stop() -> anyhow::Result<()> {
    match management() {
        Management::UserUnit => systemctl(&["--user"], "stop"),
        Management::SystemUnit => systemctl(&[], "stop"),
        Management::Bare => {
            // We started it as this user, so we can terminate it directly.
            let status = Command::new("pkill")
                .args(["-x", "ollama"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("running pkill")?;
            // pkill exits 1 when nothing matched — already stopped, not an error.
            if status.success() || status.code() == Some(1) {
                Ok(())
            } else {
                anyhow::bail!("pkill exited with {status}")
            }
        }
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
            "(stopping the system ollama.service needs privileges — try: sudo systemctl stop ollama)".to_string()
        } else {
            msg.to_string()
        }
    );
}

/// Spawn a detached `ollama serve` that survives this process exiting. `setsid -f`
/// double-forks so the server is reparented to init (which reaps it) — no zombie
/// is left behind even though we don't hold the child. Falls back to a plain
/// spawn if `setsid` is unavailable.
fn spawn_detached() -> anyhow::Result<()> {
    let via_setsid = Command::new("setsid")
        .args(["-f", "ollama", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status(); // setsid -f returns immediately after forking; reap it here.
    match via_setsid {
        Ok(s) if s.success() => Ok(()),
        _ => Command::new("ollama")
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("spawning `ollama serve`: {e}")),
    }
}
