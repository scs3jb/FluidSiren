#!/usr/bin/env bash
# FluidSiren user-local uninstaller — reverses scripts/install.sh (no sudo).
#
# Usage:
#   scripts/uninstall.sh            # remove the app; keep your config + downloaded models
#   scripts/uninstall.sh --purge    # also remove config, downloaded models, and the
#                                    # FluidSiren Ollama user service (ollama itself is kept)
#   scripts/uninstall.sh --purge -y # purge without the confirmation prompt
set -euo pipefail

purge=0
assume_yes=0
for a in "$@"; do
    case "$a" in
        --purge)   purge=1 ;;
        -y|--yes)  assume_yes=1 ;;
        -h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $a (try --help)" >&2; exit 2 ;;
    esac
done

bindir="$HOME/.local/bin"
appsdir="$HOME/.local/share/applications"
libdir="$HOME/.local/lib/fluidsiren"
autostart="$HOME/.config/autostart/dev.altic.FluidSiren.desktop"
cfgdir="${XDG_CONFIG_HOME:-$HOME/.config}/fluidsiren"
datadir="${XDG_DATA_HOME:-$HOME/.local/share}/fluidsiren"

# 1. Stop anything currently running so we don't remove a busy binary.
pkill -x fluidsiren-overlay 2>/dev/null || true
pkill -x fluidsiren 2>/dev/null || true

# 2. KWin overlay keep-on-top script (KDE-only, best-effort — mirrors install.sh).
kwin_dir="$HOME/.local/share/kwin/scripts/fluidsiren-overlay-ontop"
qbin="$(command -v qdbus6 || command -v qdbus || true)"
if command -v kpackagetool6 >/dev/null 2>&1 || [[ -d "$kwin_dir" ]]; then
    # Unload from the running session first.
    [[ -n "$qbin" ]] && "$qbin" org.kde.KWin /Scripting \
        org.kde.kwin.Scripting.unloadScript fluidsiren-overlay-ontop >/dev/null 2>&1 || true
    # Disable for future logins.
    kwriteconfig6 --file kwinrc --group Plugins \
        --key fluidsiren-overlay-ontopEnabled --delete >/dev/null 2>&1 \
        || kwriteconfig6 --file kwinrc --group Plugins \
            --key fluidsiren-overlay-ontopEnabled false >/dev/null 2>&1 || true
    # Remove the installed package.
    command -v kpackagetool6 >/dev/null 2>&1 \
        && kpackagetool6 --type KWin/Script --remove fluidsiren-overlay-ontop >/dev/null 2>&1 || true
    rm -rf "$kwin_dir"
    [[ -n "$qbin" ]] && "$qbin" org.kde.KWin /KWin org.kde.KWin.reconfigure >/dev/null 2>&1 || true
    echo "==> Removed KWin overlay keep-on-top script"
fi

# 3. Binaries, bundled libs, desktop entry, autostart.
rm -f "$bindir/fluidsiren" "$bindir/fluidsiren-overlay"
rm -rf "$libdir"
rm -f "$appsdir/dev.altic.FluidSiren.desktop"
update-desktop-database "$appsdir" 2>/dev/null || true
rm -f "$autostart"
echo "==> Removed binaries, bundled libs, desktop entry, and autostart"

# 4. --purge: user data + the FluidSiren Ollama user service.
if (( purge )); then
    if (( ! assume_yes )); then
        echo
        echo "--purge will also delete:"
        echo "  • config:  $cfgdir"
        echo "  • models:  $datadir  (downloaded models — can be several GB)"
        echo "  • the FluidSiren Ollama user service (the ollama binary + its models are kept)"
        read -r -p "Proceed? [y/N] " ans
        [[ "$ans" == [yY]* ]] || { echo "Purge aborted — the app is still uninstalled."; exit 0; }
    fi

    # Only touch the Ollama user unit if it's the one setup-ollama.sh wrote for us.
    ollama_unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ollama.service"
    if [[ -f "$ollama_unit" ]] && grep -q FluidSiren "$ollama_unit"; then
        systemctl --user disable --now ollama >/dev/null 2>&1 || true
        rm -f "$ollama_unit"
        systemctl --user daemon-reload >/dev/null 2>&1 || true
        echo "==> Removed the FluidSiren Ollama user service (ollama binary + models kept)"
    fi

    rm -rf "$cfgdir" "$datadir"
    echo "==> Purged config and downloaded models"
fi

cat <<EOF

FluidSiren uninstalled.
EOF
if (( ! purge )); then
    cat <<EOF
  • Kept your config ($cfgdir) and downloaded models ($datadir).
    Re-run with --purge to remove those too.
EOF
fi
cat <<EOF
  • Ollama (if you set it up) was left installed; remove it separately if you like.
  • If you added yourself to the 'input' group for the evdev hotkey, that's a
    system change this script doesn't touch: sudo gpasswd -d \$USER input to undo.
EOF
