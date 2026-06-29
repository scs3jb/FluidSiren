//! Text injection into the focused application.
//!
//! Wayland blocks synthetic input, so we use `ydotool` (uinput-based), which works
//! on KWin/Plasma. Requires `ydotoold` running and access to /dev/uinput.

use anyhow::{anyhow, Context, Result};
use std::process::Command;

/// Type `text` into the currently focused window via ydotool.
pub fn type_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    // Safety kill-switch: when set, never emit synthetic input. Used by test modes
    // and automation so transcripts can't leak into whatever window is focused.
    if std::env::var_os("FLUIDSIREN_NO_INJECT").is_some() {
        tracing::info!("FLUIDSIREN_NO_INJECT set — not typing {} chars", text.len());
        return Ok(());
    }
    let output = Command::new("ydotool")
        .arg("type")
        .arg("--")
        .arg(text)
        .output()
        .context("running ydotool (is it installed and is ydotoold running?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "ydotool failed: {stderr}\n\
             Hint: ydotoold must be running and you need access to /dev/uinput \
             (add yourself to the 'input' group or run the ydotoold service)."
        ));
    }
    Ok(())
}

/// Whether ydotool appears usable (best-effort check).
pub fn available() -> bool {
    Command::new("ydotool")
        .arg("--help")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false)
}
