#!/usr/bin/env bash
# Install + configure a local Ollama server for FluidSiren transcript cleanup.
#
# Idempotent: installs the `ollama` binary if missing, ensures the server is
# running, pulls the configured model, and warms it up. Safe to re-run.
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
#   OLLAMA_KEEP_ALIVE=-1      # how long to keep the model loaded (default 30m; -1 = forever)
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

pull_and_warm() {
    echo "==> Pulling model '$model' (first run can be a few GB)…"
    if ! ollama pull "$model"; then
        echo "!!  Failed to pull '$model'. Retry with: ollama pull $model" >&2
        exit 1
    fi
    echo "==> Warming up '$model' (loads it into memory; keep_alive=$keep_alive)…"
    curl -fsS --max-time 120 http://localhost:11434/api/generate \
        -d "{\"model\":\"$model\",\"prompt\":\"\",\"keep_alive\":\"$keep_alive\"}" \
        >/dev/null 2>&1 || echo "!!  Warm-up request failed (non-fatal)." >&2
    echo "==> Model '$model' ready."
}

# ── USER mode: ~/.local binary + a systemd --user service ────────────────────
if [[ "$mode" == "user" ]]; then
    echo "==> FluidSiren: setting up a USER-level Ollama (model: $model, keep_alive: $keep_alive)"

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

    # 3. Write + start the user unit (carries OLLAMA_KEEP_ALIVE so the model stays warm).
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
    systemctl --user daemon-reload
    systemctl --user enable --now ollama

    if wait_up; then echo "==> User Ollama server is up."; else
        echo "!!  Server didn't come up. Check: systemctl --user status ollama" >&2
        exit 1
    fi
    pull_and_warm

    cat <<EOF

Done. A user-level Ollama is running for FluidSiren.
  • Managed by: systemctl --user {start,stop,status} ollama   (no password prompts)
  • Enable "Enhance transcript with Ollama" in FluidSiren → Settings.
  • The Settings window shows live status + Start/Stop/Warm-up controls.
EOF
    exit 0
fi

# ── SYSTEM mode: official installer + system service ─────────────────────────
echo "==> FluidSiren: setting up a SYSTEM-level Ollama (model: $model, keep_alive: $keep_alive)"

if ! command -v ollama >/dev/null 2>&1; then
    echo "==> Installing Ollama via the official install script…"
    curl -fsSL https://ollama.com/install.sh | sh
else
    echo "==> Ollama already installed ($(ollama --version 2>/dev/null | head -n1))."
fi

if ollama_up; then
    echo "==> Ollama server already running."
else
    if systemctl --user list-unit-files ollama.service >/dev/null 2>&1 \
        && systemctl --user start ollama 2>/dev/null; then
        echo "==> Started Ollama via 'systemctl --user'."
    elif systemctl list-unit-files ollama.service >/dev/null 2>&1 \
        && sudo systemctl start ollama 2>/dev/null; then
        echo "==> Started Ollama via system 'systemctl'."
    else
        echo "==> Starting detached 'ollama serve'…"
        setsid ollama serve >/dev/null 2>&1 </dev/null &
    fi
    if ! wait_up; then
        echo "!!  Ollama server did not come up in time; try 'ollama serve' manually." >&2
        exit 1
    fi
    echo "==> Ollama server is up."
fi

pull_and_warm

cat <<EOF

Done. Ollama is set up for FluidSiren.
  • Tip: 'scripts/setup-ollama.sh --user' installs a no-sudo user service instead.
  • Enable "Enhance transcript with Ollama" in FluidSiren → Settings.
  • With that option on, the app starts + warms the server automatically on launch.
EOF
