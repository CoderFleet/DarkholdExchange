#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

HOST="${DX_LOAD_HOST:-127.0.0.1}"
PORT="${DX_LOAD_PORT:-18080}"
DURATION="${DX_LOAD_DURATION_SECS:-10}"
RATE="${DX_LOAD_RATE:-1000}"
USERS="${DX_LOAD_USERS:-32}"
WS_CONNECTIONS="${DX_LOAD_WS_CONNECTIONS:-4}"
PROBE_RATE="${DX_LOAD_PROBE_RATE:-10}"
QUEUE_CAPACITY="${DX_LOAD_QUEUE_CAPACITY:-1024}"

result_dir="bench/results"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
json="$result_dir/${stamp}-load.json"
markdown="$result_dir/${stamp}-load.md"
log="$(mktemp "${TMPDIR:-/tmp}/dx-load-gateway.XXXXXX")"

export DX_COMMAND_QUEUE_CAPACITY="$QUEUE_CAPACITY"

cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "${gateway_pid:-}" ]] && kill -0 "$gateway_pid" 2>/dev/null; then
        kill "$gateway_pid" 2>/dev/null || true
        wait "$gateway_pid" 2>/dev/null || true
    fi
    if [[ $status -ne 0 ]]; then
        echo "gateway log: $log" >&2
        tail -n 50 "$log" >&2 || true
    else
        rm -f "$log"
    fi
    exit "$status"
}
trap cleanup EXIT

mkdir -p "$result_dir"
cargo build --release -p dx-gateway -p dx-simulator

GATEWAY_ADDR="$HOST:$PORT" \
    DX_COMMAND_QUEUE_CAPACITY="$QUEUE_CAPACITY" \
    target/release/dx-gateway >"$log" 2>&1 &
gateway_pid=$!

for _ in $(seq 1 100); do
    if curl -fsS "http://$HOST:$PORT/health" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$gateway_pid" 2>/dev/null; then
        echo "gateway exited during startup" >&2
        exit 1
    fi
    sleep 0.1
done
if ! curl -fsS "http://$HOST:$PORT/health" >/dev/null 2>&1; then
    echo "gateway did not become healthy" >&2
    exit 1
fi

{
    printf '# Darkhold Exchange load baseline\n\n'
    printf -- '- Generated: `%s`\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- Host: `%s`\n' "$(uname -a)"
    if [[ "$(uname -s)" == "Darwin" ]]; then
        printf -- '- CPU: `%s`\n' "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
    fi
    printf -- '- Gateway: release build, `DX_COMMAND_QUEUE_CAPACITY=%s`\n' "$QUEUE_CAPACITY"
    printf -- '- Command: `target/release/dx-load --base-url http://%s:%s --duration-secs %s --placements-per-second %s --users %s --ws-connections %s --probe-rate %s`\n\n' \
        "$HOST" "$PORT" "$DURATION" "$RATE" "$USERS" "$WS_CONNECTIONS" "$PROBE_RATE"
    target/release/dx-load \
        --base-url "http://$HOST:$PORT" \
        --duration-secs "$DURATION" \
        --placements-per-second "$RATE" \
        --users "$USERS" \
        --ws-connections "$WS_CONNECTIONS" \
        --probe-rate "$PROBE_RATE" \
        --json-out "$json"
} | tee "$markdown"

echo
echo "Wrote $markdown"
echo "Wrote $json"
