#!/usr/bin/env bash
# FluidSiren user-local uninstaller — reverses scripts/install.sh (no sudo).
#
# Usage:
#   scripts/uninstall.sh                     # remove the app; keep config + downloaded models
#   scripts/uninstall.sh --purge             # also remove config, models, and Ollama
#   scripts/uninstall.sh --purge --keep-config  # purge, but keep ~/.config/fluidsiren
#   scripts/uninstall.sh --purge -y          # purge without the confirmation prompt
set -euo pipefail

purge=0
assume_yes=0
keep_config=0
for a in "$@"; do
    case "$a" in
        --purge)       purge=1 ;;
        --keep-config) keep_config=1 ;;
        -y|--yes)      assume_yes=1 ;;
        -h|--help)     sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

# 4. --purge: user data (config + models) and Ollama.
if (( purge )); then
    if (( ! assume_yes )); then
        echo
        echo "--purge will also delete:"
        if (( keep_config )); then
            echo "  • config:  KEPT ($cfgdir)"
        else
            echo "  • config:  $cfgdir"
        fi
        echo "  • models:  $datadir  (downloaded speech models — can be several GB)"
        echo "  • Ollama:  the FluidSiren user service, the rootless install in ~/.local,"
        echo "             and all Ollama models/data in ~/.ollama (can be many GB)."
        echo "             A system-wide Ollama (in /usr) is left for you to remove with sudo."
        read -r -p "Proceed? [y/N] " ans
        [[ "$ans" == [yY]* ]] || { echo "Purge aborted — the app is still uninstalled."; exit 0; }
    fi

    # --- Ollama (what setup-ollama.sh may have set up) ---
    # 1. The user service, only if it's the one setup-ollama.sh wrote for us.
    ollama_unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ollama.service"
    if [[ -f "$ollama_unit" ]] && grep -q FluidSiren "$ollama_unit"; then
        systemctl --user disable --now ollama >/dev/null 2>&1 || true
        rm -f "$ollama_unit"
        systemctl --user daemon-reload >/dev/null 2>&1 || true
        echo "==> Removed the FluidSiren Ollama user service"
    fi
    # 2. The rootless install setup-ollama.sh --user unpacks into ~/.local. Stop a
    #    stray server started from there first (targeted so a system ollama is safe).
    pkill -f "$HOME/.local/bin/ollama" >/dev/null 2>&1 || true
    if [[ -e "$HOME/.local/bin/ollama" || -d "$HOME/.local/lib/ollama" ]]; then
        rm -f "$HOME/.local/bin/ollama"
        rm -rf "$HOME/.local/lib/ollama"
        echo "==> Removed the rootless Ollama install (~/.local/bin/ollama, ~/.local/lib/ollama)"
    fi
    # 3. Ollama models + data (respects a custom $OLLAMA_MODELS location).
    ollama_data="${OLLAMA_MODELS:-$HOME/.ollama}"
    if [[ -d "$ollama_data" ]]; then
        rm -rf "$ollama_data"
        echo "==> Removed Ollama models and data ($ollama_data)"
    fi
    # 4. A system-wide Ollama needs root — we can't remove it here, so point the way.
    sys_ollama="$(command -v ollama 2>/dev/null || true)"
    if [[ -n "$sys_ollama" ]]; then
        echo "!!  A system Ollama is still installed at $sys_ollama. To remove it:"
        echo "      sudo systemctl disable --now ollama 2>/dev/null; sudo rm -f \"$sys_ollama\""
        echo "    (and 'sudo userdel ollama' if the official installer created that user)."
    fi

    rm -rf "$datadir"
    if (( keep_config )); then
        echo "==> Purged downloaded speech models (kept config: $cfgdir)"
    else
        rm -rf "$cfgdir"
        echo "==> Purged config and downloaded speech models"
    fi
fi

cat <<EOF

FluidSiren uninstalled.
EOF
if (( ! purge )); then
    cat <<EOF
  • Kept your config ($cfgdir), downloaded models ($datadir), and Ollama.
    Re-run with --purge to remove those too.
EOF
fi
cat <<EOF
  • If you added yourself to the 'input' group for the evdev hotkey, that's a
    system change this script doesn't touch: sudo gpasswd -d \$USER input to undo.
EOF
