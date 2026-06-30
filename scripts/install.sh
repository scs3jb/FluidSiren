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

# Bundle the sherpa-onnx / onnxruntime libs (Parakeet provider) into
# ~/.local/lib/fluidsiren; the binary's $ORIGIN/../lib/fluidsiren rpath finds them.
shopt -s nullglob
libs=(target/release/libsherpa-onnx-*.so* target/release/libonnxruntime.so*)
if (( ${#libs[@]} )); then
    libdir="$HOME/.local/lib/fluidsiren"
    mkdir -p "$libdir"
    cp -f "${libs[@]}" "$libdir/"
    echo "==> Installed Parakeet libs to $libdir"
fi

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

# Optional value-add: set up local Ollama for transcript enhancement. This is
# strictly best-effort — FluidSiren works fully without it (enhancement just stays
# off), so any failure here is a warning, never fatal to the install. Attempted by
# default; skip with SKIP_OLLAMA=1 (it pulls a model, a few GB). Defaults to the
# rootless user-service mode (no sudo/polkit prompts); OLLAMA_SYSTEM=1 uses the
# official system-service install. Override with OLLAMA_MODEL=… / OLLAMA_KEEP_ALIVE=…
if [[ "${SKIP_OLLAMA:-}" == "1" ]]; then
    echo "==> Skipping optional Ollama setup (SKIP_OLLAMA=1)."
else
    ollama_args=(--user)
    [[ "${OLLAMA_SYSTEM:-}" == "1" ]] && ollama_args=()
    echo "==> Setting up optional Ollama enhancement (best-effort; SKIP_OLLAMA=1 to skip)…"
    # Non-fatal by construction: a command in an `if` condition is exempt from
    # errexit, so whatever the setup script does, the install continues.
    if bash "$here/scripts/setup-ollama.sh" "${ollama_args[@]}"; then
        : # set up successfully
    else
        echo "!!  Ollama setup didn't finish — that's fine; FluidSiren works without it." >&2
        echo "    Re-run later with: scripts/setup-ollama.sh --user" >&2
    fi
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
  • optional: Ollama cleans up transcripts when enabled in Settings
    (set up above if it succeeded; otherwise run scripts/setup-ollama.sh --user)
  • optional: 'input' group for a direct evdev hotkey
EOF
