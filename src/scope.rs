//! Self-heal the app's systemd scope so the KDE GlobalShortcuts portal identifies
//! us correctly.
//!
//! xdg-desktop-portal identifies a non-sandboxed app by its systemd unit
//! (cgroup). A process launched from a terminal inherits that terminal's scope —
//! so if FluidSiren is started from a terminal living inside another app's scope
//! (e.g. a terminal multiplexer), the GlobalShortcuts portal would register our
//! shortcut under THAT app and hijack its shortcuts.
//!
//! To avoid this we re-exec ourselves inside a dedicated transient scope named
//! `app-dev.altic.FluidSiren-<pid>.scope`, from which the portal derives the app
//! id `dev.altic.FluidSiren`. The matching `.desktop` file must be installed for
//! the portal to accept that id (otherwise it returns NotAllowed and we fall back
//! to evdev). Best-effort: any failure logs and continues.

pub const APP_ID: &str = "dev.altic.FluidSiren";

/// Re-exec into our own systemd scope unless we're already in one. No-op without
/// systemd / on the re-exec'd child (loop-guarded) / when already correct.
pub fn reexec_in_own_scope_if_needed(forwarded_args: &[String]) {
    use std::os::unix::process::CommandExt;

    // Loop guard: the re-exec'd child carries this env var.
    if std::env::var_os("FLUIDSIREN_REEXEC").is_some() {
        return;
    }
    // Already in our own scope? (also matches a KDE-launched app-<id>@<n>.scope)
    match std::fs::read_to_string("/proc/self/cgroup") {
        Ok(c) if c.contains(APP_ID) => return,
        Ok(_) => {}
        Err(_) => return, // no cgroup info (not Linux/systemd) — nothing to do
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let unit = format!("app-{APP_ID}-{}", std::process::id());
    tracing::info!("re-executing in own systemd scope '{unit}' for a correct portal app id");

    // `exec` replaces this process image; it only returns on failure.
    let err = std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--collect", "--unit", &unit, "--"])
        .arg(&exe)
        .args(forwarded_args)
        .env("FLUIDSIREN_REEXEC", "1")
        .exec();
    tracing::warn!(
        "could not re-exec into own scope ({err}); continuing. The global shortcut \
         may attach to the launching app's id — install systemd-run, or launch via \
         the installed FluidSiren .desktop entry."
    );
}
