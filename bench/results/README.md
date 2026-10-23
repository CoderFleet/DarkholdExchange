# Load baselines

`just load` starts a fresh release gateway on `127.0.0.1:18080`, seeds 32 isolated load users,
and runs the `dx-load` harness for 10 seconds at 1,000 placements/s. Each load user rests a
one-unit buy below the best bid and immediately cancels it, so the run exercises the full REST
command path, the bounded writer queue, core matching, delta publication, and WebSocket fanout.

The runner writes a timestamped Markdown summary and JSON report here. A run is comparable only
with another run on the same host, release profile, queue capacity, and harness version: loopback
latency and CPU scheduling dominate the numbers.

## What is measured

- **Place acknowledgement / cancel acknowledgement**: client-observed HTTP round trip from the
  harness to the gateway and back.
- **Probe end-to-end**: a low-rate place/cancel loop on a separate market; measures from the
  command send to the matching `book_seq` delta arriving on a dedicated fixed-market WebSocket.
- **Gateway metrics**: cumulative command/queue/core histograms and queue depth from
  `/api/metrics/latency`.
- **WebSocket fanout**: per-connection message, delta, trade, resync, sequence-gap, and byte counts.

## Running

```sh
just load
```

Environment overrides:

```sh
DX_LOAD_DURATION_SECS=30 \
DX_LOAD_RATE=1500 \
DX_LOAD_USERS=64 \
DX_LOAD_WS_CONNECTIONS=8 \
DX_LOAD_QUEUE_CAPACITY=2048 \
just load
```

The gateway must be started fresh for each baseline because its latency histograms are cumulative
since startup. `scripts/load-test.sh` enforces that and kills the gateway on exit. Do not run the
harness against a long-lived development gateway when comparing numbers.

## Comparing runs

Keep the same host, profile, queue capacity, user count, market/price, and WS connection count.
Compare `Achieved placements/s`, `Probe end-to-end p99`, gateway `Core p99`, `Queue wait p99`,
`Slow consumers`, `Resyncs`, and `Seq gaps`. An increase in `429`/`503` outcomes or resyncs means
the offered load exceeded some bound; record it rather than hiding it by lowering the rate.

## Baselines

### 2026-10-23 — Apple M2, 1,000 placements/s target

- Report: [20261023T025941Z-load.md](20261023T025941Z-load.md) /
  [JSON](20261023T025941Z-load.json)
- Host: Apple M2 (8 logical CPUs), macOS 25.5, release build
- Gateway queue capacity: 1,024
- Load: 32 users, 4 market WebSockets, 10 seconds, 1,000 placements/s target
- Achieved: 998.90 placements/s and 2,017.70 accepted commands/s
- Client p99: 409 us place ack, 328 us cancel ack
- Probe end-to-end p99: 324 us place, 259 us cancel
- Gateway p99: 50 us queue wait, 12 us core
- Errors: 0 rejected, 0 429, 0 503, 0 resyncs, 0 sequence gaps
- Fanout: 79,912 deltas across 4 connections (7,793.25 deltas/s aggregate)

### 2026-10-23 — Apple M2, 1,100 placements/s headroom check

- Report: [20261023T030315Z-load.md](20261023T030315Z-load.md) /
  [JSON](20261023T030315Z-load.json)
- Host: Apple M2 (8 logical CPUs), macOS 25.5, release build
- Gateway queue capacity: 1,024
- Load: 32 users, 4 market WebSockets, 10 seconds, 1,100 placements/s target
- Achieved: 1,100.00 placements/s and 2,219.80 accepted commands/s
- Client p99: 381 us place ack, 314 us cancel ack
- Probe end-to-end p99: 274 us place, 265 us cancel
- Gateway p99: 53 us queue wait, 15 us core
- Errors: 0 rejected, 0 429, 0 503, 0 resyncs, 0 sequence gaps
- Fanout: 88,000 deltas across 4 connections (8,582.02 deltas/s aggregate)

## Spec target comparison

| Target from `docs/spec/README.md` | Measured | Status |
|---|---|---|
| 1,000+ order submissions/s sustained | 1,100.00 placements/s; 2,219.80 accepted commands/s | Met |
| Engine p99 in microseconds | 15 us core p99 | Met |
| End-to-end p99 < 10 ms at 1,000 orders/s | 274 us probe place p99; 381 us client place-ack p99 | Met |
| k6/Locust benchmarks | Rust `dx-load` harness | Not met as a named tool |

## Scope caveat

These runs are a **gateway command and market-data baseline**, not a fill-throughput benchmark. Orders
rest below the best bid and are immediately cancelled, so matching, settlement, and ledger writes are
not exercised under load. The k6/Locust artifact named in the spec is also not covered here; this
harness exists so the same workload can be run without an external load-tool dependency. The
end-to-end probe is loopback with no TLS, proxy, browser, or injected network latency.
