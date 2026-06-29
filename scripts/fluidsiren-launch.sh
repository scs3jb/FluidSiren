#!/usr/bin/env bash
# Launch FluidSiren in its OWN transient systemd scope.
#
# Why: xdg-desktop-portal identifies non-sandboxed apps by their systemd unit
# (cgroup). A process started from a terminal inherits that terminal's scope — so
# if you launch FluidSiren from, say, a terminal running inside another app, the
# GlobalShortcuts portal attributes FluidSiren's global shortcut to THAT app and
# hijacks its shortcuts. Running in a dedicated `app-dev.altic.FluidSiren-*.scope`
# makes the portal see the app id `dev.altic.FluidSiren` instead.
#
# Installed builds launched from the KDE menu (via the .desktop file) already get
# their own scope automatically; this wrapper is for running from a terminal.
set -euo pipefail

APP_ID="dev.altic.FluidSiren"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${FLUIDSIREN_BIN:-$here/../target/release/fluidsiren}"

if [[ ! -x "$BIN" ]]; then
    echo "fluidsiren binary not found at: $BIN" >&2
    echo "Build it first: (cd \"$here/..\" && cargo build --release)" >&2
    exit 1
fi

# A unique app-prefixed scope name → portal extracts the app id `dev.altic.FluidSiren`.
exec systemd-run --user --scope --quiet --collect \
    --unit="app-${APP_ID}-$$" \
    -- "$BIN" "$@"
