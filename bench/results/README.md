# Load baselines

`just load` starts a fresh release gateway on `127.0.0.1:18080`, seeds 32 isolated load users,
and runs the `dx-load` harness for 10 seconds at 1,000 placements/s. Each load user rests a
one-unit buy below the best bid and immediately cancels it, so the run exercises the full REST
command path, the bounded writer queue, core matching, delta publication, and WebSocket fanout.

`just load-fills` starts a fresh gateway, pre-rests one-unit maker asks, then runs a measured
taker workload at 1,000 fills/s. Each accepted taker buy crosses one maker ask, so the measured
window exercises matching, settlement, ledger writes, delta publication, and trade fanout. Maker
seeding is intentionally outside the measured window.

The runner writes a timestamped Markdown summary and JSON report here. A run is comparable only
with another run on the same host, release profile, queue capacity, and harness version: loopback
latency and CPU scheduling dominate the numbers.

## What is measured

- **Place acknowledgement / cancel acknowledgement**: client-observed HTTP round trip from the
  harness to the gateway and back.
- **Fill acknowledgement**: client-observed HTTP round trip for a taker buy; the response carries
  the fill (or fills) produced by that command.
- **Fill validation**: total fills from `CommandResponse.fills`, compared with WS `Trade` frames
  received on each market socket.
- **Maker setup**: pre-rested maker ask count and setup time; setup commands are excluded from the
  measured fill rate.
- **Probe end-to-end**: a low-rate place/cancel loop on a separate market; measures from the
  command send to the matching `book_seq` delta arriving on a dedicated fixed-market WebSocket.
- **Gateway metrics**: cumulative command/queue/core histograms and queue depth from
  `/api/metrics/latency`.
- **WebSocket fanout**: per-connection message, delta, trade, resync, sequence-gap, and byte counts.

## Running

```sh
just load
just load-fills
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

```sh
DX_LOAD_FILLS_PER_SECOND=1100 \
DX_LOAD_FILL_MAKERS=3000 \
DX_LOAD_FILL_TAKERS=3000 \
DX_LOAD_FILL_PASSES=4 \
DX_LOAD_FILL_PRICE_TICKS=5000 \
just load-fills
```

Fill capacity must cover the target: `makers * passes` maker asks and `takers * passes` taker buys
must each be at least `fills/s * duration`. The runner fails before seeding if the configured
capacity is too small, and validates the fill price against the market collar and current spread.

The gateway must be started fresh for each baseline because its latency histograms are cumulative
since startup. `scripts/load-test.sh` enforces that and kills the gateway on exit. Do not run the
harness against a long-lived development gateway when comparing numbers.

## Comparing runs

Keep the same host, profile, queue capacity, worker/user count, market/price, and WS connection
count. For place-cancel runs, compare `Achieved placements/s`, `Probe end-to-end p99`, gateway
`Core p99`, `Queue wait p99`, `Slow consumers`, `Resyncs`, and `Seq gaps`. For fill runs, compare
`Achieved fills/s`, `Fill acknowledgement p99`, `Fills match WS trades`, gateway `Core p99`, and
the same error counters. An increase in `429`/`503` outcomes or resyncs means the offered load
exceeded some bound; record it rather than hiding it by lowering the rate. The 1,000-target fill
run landed at 997.30 fills/s; the 1,100-target headroom run landed at 1,100.00 fills/s. Treat the
headroom run as the sustained fill-throughput result.

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

### 2026-10-23 — Apple M2, 1,000 fills/s target

- Report: [20261023T034748Z-fills.md](20261023T034748Z-fills.md) /
  [JSON](20261023T034748Z-fills.json)
- Host: Apple M2 (8 logical CPUs), macOS 25.5, release build
- Gateway queue capacity: 1,024
- Setup: 3,000 makers, 12,000 one-unit asks at 5,000 ticks, rested in 122 ms
- Load: 3,000 takers, 32 workers, 4 market WebSockets, 10 seconds, 1,000 fills/s target
- Achieved: 997.30 fills/s; 9,974 acknowledged fills; 1,017.30 accepted commands/s (includes
  the 10/s probe)
- Fill acknowledgement p99: 450 us (p99.9 1,950 us; max 10,455 us outlier)
- Probe end-to-end p99: 553 us place, 421 us cancel
- Gateway p99: 71 us queue wait, 20 us core
- Errors: 0 rejected, 0 429, 0 503, 0 resyncs, 0 sequence gaps
- Fanout: 9,974 deltas and 9,974 trades per socket; 39,896 deltas and 39,896 trades across
  4 connections (3,890.77 deltas/s aggregate); all 9,974 response fills matched WS trades

### 2026-10-23 — Apple M2, 1,100 fills/s headroom check

- Report: [20261023T034813Z-fills.md](20261023T034813Z-fills.md) /
  [JSON](20261023T034813Z-fills.json)
- Host: Apple M2 (8 logical CPUs), macOS 25.5, release build
- Gateway queue capacity: 1,024
- Setup: 3,000 makers, 12,000 one-unit asks at 5,000 ticks, rested in 158 ms
- Load: 3,000 takers, 32 workers, 4 market WebSockets, 10 seconds, 1,100 fills/s target
- Achieved: 1,100.00 fills/s; 11,000 acknowledged fills; 1,120.00 accepted commands/s (includes
  the 10/s probe)
- Fill acknowledgement p99: 483 us (p99.9 1,480 us; max 2,980 us)
- Probe end-to-end p99: 521 us place, 266 us cancel
- Gateway p99: 77 us queue wait, 22 us core
- Errors: 0 rejected, 0 429, 0 503, 0 resyncs, 0 sequence gaps
- Fanout: 11,000 deltas and 11,000 trades per socket; 44,000 deltas and 44,000 trades across
  4 connections (4,291.43 deltas/s aggregate); all 11,000 response fills matched WS trades

## Spec target comparison

| Target from `docs/spec/README.md` | Measured | Status |
|---|---|---|
| 1,000+ order submissions/s sustained | 1,100.00 placements/s; 2,219.80 accepted commands/s | Met |
| 1,000+ fills/s with matching, settlement, and ledger writes | 1,100.00 fills/s; 11,000 acknowledged fills | Met |
| Engine p99 in microseconds | 15 us core p99 place-cancel; 22 us core p99 fills | Met |
| End-to-end p99 < 10 ms at 1,000 orders/s | 274 us probe place p99; 483 us fill-ack p99 | Met |
| k6/Locust benchmarks | Rust `dx-load` harness | Not met as a named tool |

## Scope caveat

The place-cancel runs are a gateway command and market-data baseline; they do not exercise fills.
The fill runs do exercise matching, settlement, ledger writes, delta publication, and trade fanout,
but they pre-rest maker liquidity before the measured window and count only taker fills during it.
Maker setup time is reported separately and is not included in the achieved fill rate. The k6/Locust
artifact named in the spec is also not covered here; this harness exists so the same workload can be
run without an external load-tool dependency. All runs are loopback with no TLS, proxy, browser, or
injected network latency.
