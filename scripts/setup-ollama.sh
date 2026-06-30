#!/usr/bin/env bash
# Install + configure a local Ollama server for FluidSiren transcript cleanup.
#
# Idempotent: installs the `ollama` binary if missing, installs a systemd unit,
# and pulls the configured model. It does NOT leave Ollama running: the server is
# started only long enough to pull the model (and only if it wasn't already up),
# then stopped. FluidSiren starts it on demand when you enable enhancement.
#
# Two modes:
#   scripts/setup-ollama.sh            # SYSTEM: official installer (sudo, system service)
#   scripts/setup-ollama.sh --user     # USER:   no root, a `systemd --user` service
#
# The --user mode is recommended for FluidSiren: no password prompts to start/stop,
# and the server runs as you (so the app's Start/Stop buttons work without polkit).
#
# Env:
#   OLLAMA_MODEL=qwen2.5:3b   # model to pull (default llama3.2:3b)
#   OLLAMA_KEEP_ALIVE=-1      # how long the running server keeps the model loaded
#                             # (default 30m; -1 = forever). Used at runtime, not here.
set -euo pipefail

mode="system"
[[ "${1:-}" == "--user" ]] && mode="user"

model="${OLLAMA_MODEL:-llama3.2:3b}"
keep_alive="${OLLAMA_KEEP_ALIVE:-30m}"

ollama_up() { curl -fsS --max-time 2 http://localhost:11434/api/tags >/dev/null 2>&1; }

wait_up() {
    for _ in $(seq 1 30); do ollama_up && return 0; sleep 0.5; done
    return 1
}

pull_model() {
    echo "==> Pulling model '$model' (first run can be a few GB)…"
    if ! ollama pull "$model"; then
        echo "!!  Failed to pull '$model'. Retry with: ollama pull $model" >&2
        exit 1
    fi
    echo "==> Model '$model' ready."
}

# Was a server already running before we touched anything? If so, we leave it as
# we found it; if not, install must not leave one running.
was_up_before=0
ollama_up && was_up_before=1

# ── USER mode: ~/.local binary + a systemd --user service ────────────────────
if [[ "$mode" == "user" ]]; then
    echo "==> FluidSiren: setting up a USER-level Ollama (model: $model)"

    # 1. Ensure the binary exists. Reuse any ollama on PATH; otherwise do a rootless
    #    install into ~/.local (the documented manual install — no sudo).
    if command -v ollama >/dev/null 2>&1; then
        bin="$(command -v ollama)"
        echo "==> Reusing existing ollama: $bin"
    else
        case "$(uname -m)" in
            x86_64)  arch="amd64" ;;
            aarch64) arch="arm64" ;;
            *) echo "!!  Unsupported arch $(uname -m) for rootless install; install ollama manually." >&2; exit 1 ;;
        esac
        echo "==> Installing ollama into ~/.local (rootless)…"
        mkdir -p "$HOME/.local"
        tmp="$(mktemp)"
        curl -fsSL "https://ollama.com/download/ollama-linux-${arch}.tgz" -o "$tmp"
        tar -C "$HOME/.local" -xzf "$tmp"
        rm -f "$tmp"
        bin="$HOME/.local/bin/ollama"
        echo "==> Installed $bin"
        case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *)
            echo "!!  $HOME/.local/bin is not on PATH — add it so 'ollama' is found." ;;
        esac
    fi

    # 2. If a *system* ollama service holds the port, the user service can't bind it.
    if systemctl list-unit-files ollama.service >/dev/null 2>&1 \
        && systemctl is-active --quiet ollama 2>/dev/null; then
        echo "!!  The system ollama.service is running and owns port 11434."
        echo "    Disable it first so the user service can take over:"
        echo "        sudo systemctl disable --now ollama"
    fi

    # 3. Write the user unit (carries OLLAMA_KEEP_ALIVE for when it does run).
    unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    mkdir -p "$unit_dir"
    cat > "$unit_dir/ollama.service" <<UNIT
[Unit]
Description=Ollama (user) for FluidSiren
After=network-online.target

[Service]
ExecStart=$bin serve
Environment=OLLAMA_KEEP_ALIVE=$keep_alive
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
UNIT
    echo "==> Wrote $unit_dir/ollama.service"
    # Not enabled and not left running — FluidSiren controls start/stop on demand.
    systemctl --user daemon-reload

    # 4. Start the server only if needed, just to pull the model, then put it back.
    we_started=0
    if ! ollama_up; then
        systemctl --user start ollama
        we_started=1
        wait_up || { echo "!!  Server didn't come up to pull the model. Check: systemctl --user status ollama" >&2; exit 1; }
        echo "==> Started Ollama temporarily to pull the model."
    fi
    pull_model
    if (( we_started )) && (( ! was_up_before )); then
        systemctl --user stop ollama
        echo "==> Stopped Ollama — install leaves it not running."
    fi

    cat <<EOF

Done. A user-level Ollama is set up for FluidSiren (and left stopped).
  • Managed by: systemctl --user {start,stop,status} ollama   (no password prompts)
  • Not enabled at login and not running now. It starts only when you enable
    "Enhance transcript with Ollama" in FluidSiren → Settings (or hit Start there),
    or manually: systemctl --user start ollama
EOF
    exit 0
fi

# ── SYSTEM mode: official installer + system service ─────────────────────────
echo "==> FluidSiren: setting up a SYSTEM-level Ollama (model: $model)"

if ! command -v ollama >/dev/null 2>&1; then
    echo "==> Installing Ollama via the official install script…"
    curl -fsSL https://ollama.com/install.sh | sh
else
    echo "==> Ollama already installed ($(ollama --version 2>/dev/null | head -n1))."
fi

# Ensure a server is up just long enough to pull the model.
we_started=0
if ! ollama_up; then
    if systemctl list-unit-files ollama.service >/dev/null 2>&1 \
        && sudo systemctl start ollama 2>/dev/null; then
        we_started=1
        echo "==> Started Ollama (system service) temporarily to pull the model."
    else
        echo "!!  Could not start a server to pull the model. Start it and re-run." >&2
        exit 1
    fi
    wait_up || { echo "!!  Ollama server did not come up in time." >&2; exit 1; }
fi

pull_model

# The official installer enables + starts the service. If it wasn't running before
# we began, leave the machine as we found it: stop and disable so install doesn't
# silently leave Ollama running or autostarting at boot.
if (( ! was_up_before )); then
    echo "==> Stopping + disabling the system service so install doesn't leave it running…"
    sudo systemctl disable --now ollama 2>/dev/null \
        || echo "!!  Could not stop/disable it; do so with: sudo systemctl disable --now ollama" >&2
fi

cat <<EOF

Done. Ollama is set up for FluidSiren (and left stopped).
  • Tip: 'scripts/setup-ollama.sh --user' installs a no-sudo user service instead.
  • It starts only when you enable "Enhance transcript with Ollama" in
    FluidSiren → Settings (or hit Start there). A system service start prompts
    for authentication (polkit); --user mode does not.
EOF
