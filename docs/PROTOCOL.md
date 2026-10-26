# T1 Protocol Contract — v1 (frozen)

**Frozen:** 2026-10-22
**Owner:** T1 (matching engine and live market)
**Consumers:** T2 (ledger/private feed), T3 (gateway/auth/admin), web terminal

This document freezes the REST and WebSocket names and payloads for order entry and market data.
Changes to a frozen shape require a fixture, Rust type, and web codec change in the same PR.

The canonical JSON fixtures live in [`contracts/`](../contracts/), and their exact MessagePack
encodings live in `contracts/msgpack/`. `dx-types` is the source of truth.

---

## 1. Boundaries

| Area | Owner | Notes |
|---|---|---|
| Order entry, book, trades, market feed, engine metrics | T1 | This document |
| JWT/session login, admin, issuer UI/API | T3 | T1 keeps `x-dev-user` behind `dev_user()` until replaced |
| Ledger, escrow, proofs, private balance feed | T2 | `Balances` WS message is reserved for T2 |

**Auth rule:** commands never contain a `user_id`. The gateway derives identity from the request.
Until T3 lands JWT, `x-dev-user: <id>` is the temporary session header and defaults to user `1`.
Any future auth implementation must preserve the command bodies below.

**Units:** money is `i64` minor units; prices and quantities are `u64`; no floats. `seq` is the
global command sequence. `book_seq` is the per-market book version.

---

## 2. REST endpoints

### 2.1 Place an order

```http
POST /api/orders
Content-Type: application/json
x-dev-user: 3
```

Body is one of the two place commands:

```json
{
  "type": "PlaceLimit",
  "client_order_id": 171000000001,
  "market_id": 1,
  "side": "Buy",
  "price_ticks": 5000,
  "qty": 2
}
```

```json
{
  "type": "PlaceMarket",
  "client_order_id": 171000000002,
  "market_id": 1,
  "side": "Sell",
  "qty": 1,
  "protect_price_ticks": 4800
}
```

Successful response (`200`):

```json
{
  "seq": 42,
  "status": "Rested",
  "order_id": 1001,
  "filled_qty": 0,
  "remaining_qty": 2,
  "fills": [],
  "reason": null
}
```

`status` is one of `Accepted`, `Rested`, `PartiallyFilled`, `Filled`, `Cancelled`, `Rejected`.
A rejected command is returned as an HTTP error with `{"reason": "<ReasonCode>"}`. `Cancel` on this
endpoint is rejected with `405`; use `DELETE /api/orders/{id}`.

### 2.2 Cancel an order

```http
DELETE /api/orders/{order_id}?client_order_id=171000000003
x-dev-user: 3
```

`client_order_id` is a required idempotency key for the cancel. The response has the same
`CommandResponse` shape as above. Cancelling another user's order returns `403`; an unknown order
returns `404`.

### 2.3 Open orders for the current user

```http
GET /api/orders/open
x-dev-user: 3
```

Response is `OpenOrderView[]`:

```json
[
  {
    "market_id": 1,
    "order_id": 1001,
    "side": "Buy",
    "price_ticks": 5000,
    "qty": 2,
    "remaining": 1
  }
]
```

### 2.4 Book snapshot

```http
GET /api/markets/{market_id}/book?depth=20
```

`depth` defaults to `20` and is clamped to `1..=200`. Response is a **JSON** `BookSnapshot` by
default. MessagePack is reserved for WebSocket frames; REST content negotiation can be added later
without changing this path.

```json
{
  "market_id": 1,
  "book_seq": 11,
  "bids": [{ "price_ticks": 5000, "qty": 3 }],
  "asks": [{ "price_ticks": 5100, "qty": 2 }],
  "last_trade_price": 5050
}
```

Unknown market returns `404`.

### 2.5 Recent trades

```http
GET /api/markets/{market_id}/trades?limit=50
```

`limit` defaults to `50` and is clamped to `1..=200`. The path market id is authoritative; the
response is `Fill[]` in chronological order. Unknown market returns `404`.

### 2.6 Latency and health metrics

```http
GET /api/metrics/latency
```

```json
{
  "seq": 42,
  "queue_depth": 4,
  "commands_total": 120000,
  "rejected_total": 120,
  "queue_wait_p99_us": 850,
  "core_p99_us": 210,
  "ws_clients": 32,
  "slow_consumers": 1
}
```

`queue_wait_p99_us` and `core_p99_us` are rolling p99 values from the gateway's bounded
single-writer queue. Queue wait covers submission to writer pickup; core time covers
`process_with_deltas`. Both are zero until the first command completes. When the queue is full,
gateway endpoints return `429` with `ReasonCode::QueueFull` and `Retry-After: 1`; an unavailable
writer returns `503`. Capacity defaults to 1024 and is configurable with
`DX_COMMAND_QUEUE_CAPACITY`.

The gateway also exposes a public state fingerprint used to verify WAL recovery:

```http
GET /api/state/hash
```

```json
{ "state_hash": "ddeb4382d5b67d0ea610c86891e43017b548eed02ac65a7219287da1a0aabf8e" }
```

It is a deterministic hash of the engine books, ledger balances, open-buy reservations, and core
sequence. It is not a balance or account identifier.

### 2.7 Temporary aliases

These remain available so the current terminal keeps working. New code should use the canonical
paths above.

| Temporary alias | Canonical path |
|---|---|
| `POST /api/commands` | `POST /api/orders` |
| `GET /api/users/{id}/orders` | `GET /api/orders/open` |
| `/ws` (all markets) | `/ws/market/{market_id}` |

`POST /api/commands` still accepts all three command variants, including `Cancel`, for backward
compatibility. It will be removed after the terminal migrates.

---

## 3. Error contract

All gateway errors use:

```json
{ "reason": "outside_collar" }
```

| HTTP | Meaning | Examples |
|---|---|---|
| `400` | Malformed body/query, invalid enum, invalid quantity/price/tick, missing `client_order_id` | `invalid_request`, `invalid_quantity`, `price_not_tick_aligned` |
| `403` | Authenticated user does not own the order | `not_order_owner` |
| `404` | Unknown market or order | `unknown_market`, `order_not_found` |
| `405` | Wrong method for the operation (cancel sent to `POST /api/orders`) | `cancel_not_allowed` |
| `409` | Conflicting lifecycle state | `duplicate_client_order_id`, `market_closed`, `cancel_not_allowed` |
| `422` | Sequenced business rejection | `outside_collar`, `insufficient_funds`, `self_trade` |
| `429` | Command queue full; includes `Retry-After: 1` | `queue_full` |
| `500` | Internal engine failure | `internal_error` |
| `503` | Core writer unavailable | `internal_error` |

The core still produces a `CommandResponse` with `status: Rejected`; the gateway translates it to
the table above once at the edge. Do not add a second mapping in the browser.

---

## 4. WebSocket: `/ws/market/{market_id}`

Canonical URL:

```text
ws://host/ws/market/1?user_id=3
```

The optional `user_id` query parameter is the temporary identity for private messages. The
`/ws` alias keeps the old all-markets behavior and subscription flow.

### 4.1 Initial sequence

After the upgrade, the server sends:

```text
Hello
Snapshot (one or more; only the requested market on /ws/market/{id})
Trade (recent tape, oldest first)
Metrics
```

Then it streams `Delta`, `Trade`, `Metrics`, and control messages. A client must store the snapshot
before applying any delta. Normal command traffic publishes only `Delta` and `Trade`; a `Snapshot`
is sent on initial connection, after a `Subscribe` changes the selection or depth, and during lag
recovery.

### 4.2 Client → server

```json
{ "type": "Subscribe", "market_ids": [1, 3], "depth": 20 }
{ "type": "Ping" }
```

On `/ws/market/{id}`, the fixed market cannot be changed by `Subscribe`; only `depth` is applied.
On `/ws`, an empty `market_ids` means all markets. `depth` is clamped to `1..=200`. A successful
`Subscribe` sends fresh snapshots for the new selection; discard the previous baseline before
applying subsequent deltas.

### 4.3 Server → client messages

All frames are MessagePack on the wire. JSON below is the readable contract.

#### Hello

```json
{
  "type": "Hello",
  "markets": [],
  "user_id": 3
}
```

`markets` is the market metadata list (same `MarketConfig` shape as `/api/markets`).

#### Snapshot

```json
{
  "type": "Snapshot",
  "snapshot": {
    "market_id": 1,
    "book_seq": 11,
    "bids": [{ "price_ticks": 5000, "qty": 3 }],
    "asks": [{ "price_ticks": 5100, "qty": 2 }],
    "last_trade_price": 5050
  }
}
```

A snapshot is the full book state at `book_seq`. The first snapshot for a market establishes the
client's baseline. A later snapshot replaces the baseline; the server sends one only for initial
connection, subscription changes, and lag recovery.

#### Delta

```json
{
  "type": "Delta",
  "delta": {
    "market_id": 1,
    "book_seq": 12,
    "changes": [
      { "side": "Buy", "price_ticks": 5000, "qty": 3 },
      { "side": "Sell", "price_ticks": 5100, "qty": 0 }
    ]
  }
}
```

Each change is an **absolute** quantity at that price level. `qty: 0` removes the level. A delta is
applied only when:

```text
delta.book_seq == local.book_seq + 1
```

Apply every change, update `local.book_seq = delta.book_seq`, and re-sort/truncate for display. If
the sequence is not the next one, do not apply the delta; discard the local book and wait for a
fresh snapshot.

#### Trade

```json
{
  "type": "Trade",
  "fill": {
    "trade_id": 77,
    "market_id": 1,
    "maker_order_id": 1001,
    "taker_order_id": 1002,
    "maker_user_id": 3,
    "taker_user_id": 5,
    "maker_side": "Buy",
    "price_ticks": 5000,
    "qty": 2,
    "seq": 42
  }
}
```

A trade records an execution. It updates the tape and last-trade price, but it is **not** a book
replacement. A command that changes the book without trading (rest/cancel) produces a delta but no
trade. A command that trades may produce both a trade and one or more deltas.

#### ResyncRequired

```json
{ "type": "ResyncRequired", "market_id": 1 }
```

The client missed messages or cannot apply a delta. It must clear the local book for that market,
wait for the next `Snapshot`, and resume applying deltas from that snapshot's `book_seq`.

#### Metrics

```json
{
  "type": "Metrics",
  "metrics": {
    "seq": 42,
    "queue_depth": 4,
    "commands_total": 120000,
    "rejected_total": 120,
    "queue_wait_p99_us": 850,
    "core_p99_us": 210,
    "ws_clients": 32,
    "slow_consumers": 1
  }
}
```

Same payload as `GET /api/metrics/latency`. Metrics are operational telemetry, not book state.

#### Pong / Error / Balances

```json
{ "type": "Pong" }
{ "type": "Error", "code": "outside_collar" }
{ "type": "Balances", "user_id": 3, "balances": [] }
```

`Balances` is reserved for T2's private feed. T1 does not emit it.

---

## 5. Delta application rules

1. Store `snapshot.book_seq` when a snapshot arrives.
2. For each delta, require `delta.book_seq == snapshot.book_seq + 1`.
3. Set each changed level to the delta's absolute `qty`; remove it when `qty == 0`.
4. Update `snapshot.book_seq` to `delta.book_seq`.
5. On a gap, stop applying deltas, clear the book, and wait for `ResyncRequired`/`Snapshot`.
6. Trades do not alter `book_seq`; use them for the tape and last price.
7. Never compare `book_seq` with the global command `seq`. Other markets can advance `seq`
   without advancing this market's `book_seq`.

A duplicate delta is harmless if applied twice because the quantities are absolute. A **missing**
delta is not harmless; sequence detection is mandatory.

---

## 6. Current implementation status

Frozen now:

- Message types and generated TypeScript declarations
- JSON fixtures and golden MessagePack frames for `Delta`, `ResyncRequired`, `Metrics`
- REST canonical routes and temporary aliases
- Error mapping at the gateway edge
- Explicit `ResyncRequired` on a lagged WebSocket
- `/ws/market/{id}` fixed-market feed

Core command processing returns ordered per-market `BookDelta` values through
`Core::process_with_deltas`; `Core::process` remains the response-only wrapper. The gateway submits
all core operations through a bounded queue owned by one writer thread. That writer processes jobs in
queue order and publishes deltas/trades before taking the next job, so queue order is commit order.
An initial baseline, subscription change, or resync is a single writer job: it subscribes, takes the
snapshot, and reads the recent tape without a command publishing in between. Cached and rejected
commands publish nothing. Snapshots remain reserved for baselines, subscription changes, and lag
recovery.

The writer also keeps a lazy max-depth (200) snapshot cache per market. Book deltas and fills
invalidate the affected market; the next snapshot request rebuilds it from the core. Baselines,
resyncs, REST book reads, and market views slice the cached top levels instead of rebuilding the
same book for every client. WebSocket sends coalesce already-ready messages into bounded batches
(64 messages or 256 KiB) and flush once per batch; message types, ordering, filtering, and the
one-message-per-frame contract are unchanged.

The terminal now consumes the canonical `/ws/market/{id}` feed through a module worker and a Zustand
market store. The worker owns the socket, MessagePack decoding, reconnect backoff, and subscription
frames; React components never open the socket directly. The store applies absolute deltas, detects
`book_seq` gaps, clears the stale baseline, requests a fresh snapshot with a `Subscribe` frame, and
resumes from the new `book_seq`. REST remains the bootstrap path for the market list and initial
tape, and command-response fills are merged into the tape by `trade_id`.
