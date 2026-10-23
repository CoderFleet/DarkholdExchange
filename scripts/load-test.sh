#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

HOST="${DX_LOAD_HOST:-127.0.0.1}"
PORT="${DX_LOAD_PORT:-18080}"
WORKLOAD="${DX_LOAD_WORKLOAD:-place-cancel}"
DURATION="${DX_LOAD_DURATION_SECS:-10}"
RATE="${DX_LOAD_RATE:-1000}"
FILLS_PER_SECOND="${DX_LOAD_FILLS_PER_SECOND:-1000}"
FILL_MAKERS="${DX_LOAD_FILL_MAKERS:-3000}"
FILL_TAKERS="${DX_LOAD_FILL_TAKERS:-3000}"
FILL_PASSES="${DX_LOAD_FILL_PASSES:-4}"
FILL_PRICE_TICKS="${DX_LOAD_FILL_PRICE_TICKS:-5000}"
USERS="${DX_LOAD_USERS:-32}"
WS_CONNECTIONS="${DX_LOAD_WS_CONNECTIONS:-4}"
PROBE_RATE="${DX_LOAD_PROBE_RATE:-10}"
QUEUE_CAPACITY="${DX_LOAD_QUEUE_CAPACITY:-1024}"

case "$WORKLOAD" in
    place-cancel) result_suffix="load" ;;
    fills) result_suffix="fills" ;;
    *)
        echo "unknown DX_LOAD_WORKLOAD '$WORKLOAD' (expected place-cancel or fills)" >&2
        exit 2
        ;;
esac

result_dir="bench/results"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
json="$result_dir/${stamp}-${result_suffix}.json"
markdown="$result_dir/${stamp}-${result_suffix}.md"
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

load_args=(
    --base-url "http://$HOST:$PORT"
    --workload "$WORKLOAD"
    --duration-secs "$DURATION"
    --users "$USERS"
    --ws-connections "$WS_CONNECTIONS"
    --probe-rate "$PROBE_RATE"
    --json-out "$json"
)
if [[ "$WORKLOAD" == "fills" ]]; then
    title="fill-heavy load baseline"
    load_args+=(
        --fills-per-second "$FILLS_PER_SECOND"
        --fill-makers "$FILL_MAKERS"
        --fill-takers "$FILL_TAKERS"
        --fill-passes "$FILL_PASSES"
        --fill-price-ticks "$FILL_PRICE_TICKS"
    )
else
    title="load baseline"
    load_args+=(--placements-per-second "$RATE")
fi

{
    printf '# Darkhold Exchange %s\n\n' "$title"
    printf -- '- Generated: `%s`\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- Host: `%s`\n' "$(uname -a)"
    if [[ "$(uname -s)" == "Darwin" ]]; then
        printf -- '- CPU: `%s`\n' "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
    fi
    printf -- '- Gateway: release build, `DX_COMMAND_QUEUE_CAPACITY=%s`\n' "$QUEUE_CAPACITY"
    printf -- '- Command: `target/release/dx-load %s`\n\n' "${load_args[*]}"
    target/release/dx-load "${load_args[@]}"
} | tee "$markdown"

echo
echo "Wrote $markdown"
echo "Wrote $json"
