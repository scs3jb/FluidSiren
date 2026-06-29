#!/usr/bin/env bash
# FluidSiren user-local installer — builds and installs to ~/.local (no sudo).
#
# Usage:
#   scripts/install.sh            # CPU build
#   scripts/install.sh cuda       # Nvidia
#   scripts/install.sh vulkan     # AMD
#   ENABLE_AUTOSTART=1 scripts/install.sh   # also start at login
set -euo pipefail

features="${1:-}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here"

echo "==> Building release${features:+ (features: $features)} …"
if [[ -n "$features" ]]; then
    cargo build --release --features "$features"
else
    cargo build --release
fi

bindir="$HOME/.local/bin"
appsdir="$HOME/.local/share/applications"
mkdir -p "$bindir" "$appsdir"

install -Dm755 target/release/fluidsiren "$bindir/fluidsiren"
install -Dm755 target/release/fluidsiren-overlay "$bindir/fluidsiren-overlay"
echo "==> Installed binaries to $bindir"

# Desktop entry — its basename (dev.altic.FluidSiren) is the app id the KDE
# GlobalShortcuts portal attributes the hotkey to, so it must be installed.
sed "s|^Exec=.*|Exec=$bindir/fluidsiren|" packaging/dev.altic.FluidSiren.desktop \
    > "$appsdir/dev.altic.FluidSiren.desktop"
update-desktop-database "$appsdir" 2>/dev/null || true
echo "==> Installed desktop entry to $appsdir"

if [[ "${ENABLE_AUTOSTART:-}" == "1" ]]; then
    mkdir -p "$HOME/.config/autostart"
    cp "$appsdir/dev.altic.FluidSiren.desktop" "$HOME/.config/autostart/"
    echo "==> Enabled autostart"
fi

# ydotool daemon is needed for typing into apps.
if ! pgrep -x ydotoold >/dev/null 2>&1; then
    echo "!!  ydotoold is not running — typing won't work until it is."
    echo "    Try: systemctl --user enable --now ydotool   (unit name varies by distro)"
fi

case ":$PATH:" in
    *":$bindir:"*) ;;
    *) echo "!!  $bindir is not on your PATH — add it to use 'fluidsiren' directly." ;;
esac

cat <<EOF

Done. Run:  fluidsiren
  • first run downloads the Whisper model (~142 MB)
  • the hotkey self-binds (default F12); rebind it from Settings
  • optional: Ollama for cleanup, 'input' group for a direct evdev hotkey
EOF
