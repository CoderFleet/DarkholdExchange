# Darkhold Exchange — Development Plan

**Status:** Draft v1 · 2026-10-21
**Source spec:** [`docs/spec/darkhold-spec.pdf`](spec/darkhold-spec.pdf)
**Decisions log:** [`docs/DECISIONS.md`](DECISIONS.md)

---

## 1. How to use this plan

This is an ordered build plan, not a feature wish-list. Each block ends with a demoable result. If
time runs short, cut from the bottom of the priority list in §12, never from the invariants in §5.

The plan assumes six working blocks. Map them onto the real calendar once the deadline date is known.
The critical path is:

```
protocol/domain → engine + ledger → core → REST/WS vertical slice → fairness/trust features → hardening
```

Do not start with UI polish, PostgreSQL, WAL, or the full auction suite. They do not de-risk the core.

---

## 2. Target and success criteria

### Functional

- Two users can log in, receive simulated funds, place/cancel limit orders, and trade with each other.
- The book, trade tape, balances, and open orders update live without a page reload.
- A dropped WebSocket reconnects and resynchronises from a snapshot without corrupting the book.
- A market order sweeps only inside the protected price range.

### Fairness

- Orders outside the face-value collar are rejected server-side, even if the UI is bypassed.
- Per-identity holding limits are enforced.
- Per-user/per-IP rate limits and proof-of-work raise the cost of bot swarms.
- Dutch auction and frequent batch auction (commit-reveal) clear deterministically.

### Trust

- Every fill posts one balanced journal entry; no user balance can go negative.
- `held == open reservations`; per-asset conservation holds after every command.
- Replaying the command log reproduces a bit-identical state hash.
- A browser can verify a user's inclusion in the published Merkle-sum root and verify the audit hash chain.
- Escrow releases only against a valid signed delivery receipt and refunds on timeout.

### Performance

- 1,000+ order submissions/s sustained on demo hardware.
- Engine p99 in the microsecond range; end-to-end p99 < 10 ms at 1,000 orders/s.
- Actual measured numbers are recorded in `bench/results/`; we do not claim numbers we have not measured.

### Engineering

- CI green: Rust fmt/clippy/tests, web lint/typecheck/tests/build, contract tests.
- Property tests and differential tests for the engine and ledger.
- Playwright end-to-end test for the two-user trade flow.
- `docs/TOOLS.md` updated continuously (required deliverable).

---

## 3. Architecture

The source diagram is [`docs/spec/architecture.png`](spec/architecture.png). The class diagram is
[`docs/spec/class-diagram.png`](spec/class-diagram.png).

### Data flow

```
Browser
  │  REST (commands, history, proofs)
  │  WSS  (snapshot + sequenced book diffs, fills, balances, auctions)
  ▼
Caddy/Nginx (TLS, static cache, coarse rate limit)   [hardening]
  ▼
Gateway (axum/tokio)
  auth · token bucket · PoW · collar pre-check · schema validation · WS hub
  │  bounded lock-free MPSC ring (429/503 + Retry-After when full)
  ▼
Core (single writer, no I/O)
  sequencer · order books · ledger · auctions · escrow
  │  events (SPSC ring)                    │  book deltas (SPSC ring)
  ▼                                       ▼
WAL writer (group commit, hash chain)     Publisher (cached L2 snapshot, 20–50 ms batches)
  ▼                                       ▼
Append-only log + snapshots               WebSocket sessions
  ▼
Projection worker → PostgreSQL read models (history, statements, audit index)
```

### Crate responsibilities

| Crate | Responsibility | Must not |
|---|---|---|
| `dx-types` | Wire/domain types, market config, reason codes, snapshots, fixtures | Contain business logic or I/O |
| `dx-engine` | Pure order book + matching: price levels, FIFO, limit/market, cancel, STP, state hash | Know about users' cash, DB, network, clock |
| `dx-ledger` | Accounts, reservations, journal entries, postings, invariants, reconciliation | Match orders or do I/O |
| `dx-core` | Single-writer orchestration: sequence, idempotency, reserve → match → settle, auctions, escrow | Do network/disk I/O or allocate on the hot path |
| `dx-solvency` | Merkle-sum tree, proofs, verification, audit-chain helpers | Hold live balances |
| `dx-simulator` | Bot/human traffic generator and attack scenarios | Bypass the gateway in the "real" demo path |
| `dx-gateway` | axum REST/WS, auth, rate limit, PoW, rings, publisher, WAL thread, metrics | Mutate book/ledger state directly |
| `web` | React UI, Web Worker market-data client, Zustand stores, Canvas charts | Reimplement matching or trust client-side validation |

### Hot-path rules

1. One writer owns the books and ledger. No locks around them.
2. No network, disk, database, or clock I/O in the core.
3. No `String`, `HashMap` iteration, or heap allocation on the matching path. Use enums, fixed-size
   commands, preallocated slabs/rings, and a reusable event buffer. Where a command can produce a
   variable number of fills/level changes, use a bounded `SmallVec`/arena and reject commands that
   exceed the bound; profile before claiming "zero allocation".
4. Bounded queues everywhere. Overload is an explicit rejection, not unbounded latency.
5. Every state change produces an event with a sequence number; every event is replayable.

---

## 4. Protocol v1

### 4.1 Transport split

| Plane | Transport | Format | Why |
|---|---|---|---|
| Commands/control | REST | JSON by default; msgpack accepted via `Content-Type` | Clean HTTP status codes, idempotency, easy k6/curl debugging |
| Market data/events | WebSocket | **msgpack frames** | Compact, fast browser decode, tick-rate friendly |
| Contract fixtures | Files | **JSON canonical schema + golden `.msgpack` bytes** | Reviewable diffs + exact wire verification |

REST responses may include fills for the submitting user so the UI can acknowledge immediately.
Other users receive the same trade over WS.

### 4.2 Core IDs and units

```rust
type UserId = u64;
type MarketId = u32;
type OrderId = u64;
type TradeId = u64;
type Seq = u64;       // global command sequence, assigned by the core
type BookSeq = u64;   // per-market book sequence, assigned on book change
type Minor = i64;     // paise; never float
type Ticks = u64;     // price in ticks; 1 tick = market.tick_size_minor
type Qty = u64;       // fungible units within a market
```

### 4.3 Commands (REST bodies)

```rust
struct PlaceLimit {
    client_order_id: u64,   // idempotency key, unique per user
    market_id: MarketId,
    side: Side,             // Buy | Sell
    price_ticks: u64,
    qty: u64,
}

struct PlaceMarket {
    client_order_id: u64,
    market_id: MarketId,
    side: Side,
    qty: u64,
    protect_price_ticks: u64, // buy: max price; sell: min price
}

struct Cancel {
    client_order_id: u64,   // idempotency key for the cancel request
    order_id: OrderId,
}
```

**Security change from the scaffold:** `user_id` is **removed** from the command body. The gateway
derives it from the JWT and passes an `AuthContext` to the core. A client must never be able to place
an order as another user. The existing `contracts/place_limit.json` fixture will change.

**Idempotency:** duplicate `(user_id, client_order_id)` returns the original result instead of
creating a second order. A bounded cache (or `client_order_id` sequence window) lives in the core.
Retries after a timeout are safe.

### 4.4 REST endpoints

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/auth/dev-login` | Dev-only login; returns JWT for a seeded identity |
| `POST` | `/api/ws-ticket` | Short-lived one-time ticket for the WS connection |
| `GET` | `/api/markets` | Market list with face value, collar, status, last trade |
| `GET` | `/api/markets/:id` | Market detail + collar + holding limit + fee |
| `GET` | `/api/markets/:id/book?depth=50` | REST snapshot fallback |
| `GET` | `/api/markets/:id/trades?limit=100` | Recent trade tape |
| `POST` | `/api/orders` | Submit `PlaceLimit` / `PlaceMarket`; returns accepted/rejected |
| `DELETE` | `/api/orders/:id` | Cancel own order |
| `GET` | `/api/me/balances` | Cash + per-market pass balances |
| `GET` | `/api/me/orders?status=open` | Open orders |
| `GET` | `/api/me/fills?limit=100` | Own fills |
| `POST` | `/api/dev/deposit` | Simulated top-up; posts a balanced ledger entry |
| `GET` | `/api/auctions` | Dutch + batch auction state |
| `POST` | `/api/auctions/:id/commit` | Batch auction commit hash |
| `POST` | `/api/auctions/:id/reveal` | Batch auction reveal |
| `POST` | `/api/auctions/:id/accept` | Dutch auction acceptance |
| `GET` | `/api/me/solvency` | Merkle-sum proof for the caller |
| `GET` | `/api/audit?from_seq=` | Audit records for browser verification |
| `GET` | `/health` | Liveness/readiness |
| `GET` | `/metrics` | JSON metrics snapshot |

**Acknowledgement path:** each ring item carries a one-shot reply channel. The gateway waits for the
core's result with a short timeout (e.g. 2 s) and returns the accepted/rejected response. If the ring
is full it returns `429` immediately; if the core does not reply in time it returns `503`. The core
never blocks on the reply. Dev login is disabled unless `DX_DEV_AUTH=1` and should be bound to
localhost outside local development.

**Status mapping**

| Status | Meaning |
|---|---|
| `200/202` | Accepted by the core (includes `seq`, `order_id`, status, fills) |
| `400` | Malformed body / unknown fields / invalid enum |
| `401/403` | Missing/invalid token, wrong user |
| `404` | Unknown market/order |
| `409` | Duplicate idempotency key with different payload, or cancel not allowed |
| `422` | Business rejection: outside collar, insufficient funds/inventory, holding limit, self-trade |
| `429` | Rate limited or ring full; includes `Retry-After` |
| `503` | Core unavailable/overloaded; includes `Retry-After` |

### 4.5 WebSocket messages

Client → server:

```rust
enum ClientMessage {
    Subscribe { market_ids: Vec<MarketId> },
    Unsubscribe { market_ids: Vec<MarketId> },
    Resync { market_id: MarketId },
    Ping { client_time_ms: u64 },
}
```

Server → client:

```rust
enum ServerMessage {
    Snapshot(BookSnapshot),          // top 50 levels + book_seq
    Delta(BookDelta),                // absolute level quantities
    Trade(Trade),                    // public tape, no counterparty IDs
    OrderUpdate(OrderUpdate),        // private: own order state
    BalanceUpdate(BalanceUpdate),    // private
    AuctionUpdate(AuctionUpdate),
    EscrowUpdate(EscrowUpdate),
    Metrics(MetricsSnapshot),
    Pong { server_time_ms: u64 },
    ResyncRequired { market_id: MarketId },
}
```

`BookDelta` uses **absolute** quantity per price level (`qty = 0` means remove). This is idempotent
and avoids drift from a lost incremental delta. Each delta carries `market_id`, `book_seq`, and the
level changes. A client applies a delta only if `delta.book_seq == local.book_seq + 1`; otherwise it
requests a snapshot and replaces the book.

**Sequence model:** global `seq` for audit/commands; per-market `book_seq` for market data. This
avoids false resyncs caused by other markets interleaving on a global stream.

**Slow consumers:** each WS session has a bounded send buffer. If it lags, send `ResyncRequired` or
close the connection; never buffer indefinitely. Reconnect storms hit the publisher's cached snapshot,
not the engine.

### 4.6 Msgpack and contract workflow

1. Rust `dx-types` is the source of truth.
2. Every message has a JSON fixture in `contracts/` (reviewable, canonical).
3. A Rust test/xtask writes the golden msgpack bytes to `contracts/msgpack/`.
4. JS tests decode the golden bytes and compare with the JSON fixture.
5. `ts-rs` exports `web/src/protocol/generated.d.ts`; JS uses JSDoc imports and `// @ts-check`.
6. A contract change requires the fixture + Rust + JS in the same PR (existing README rule).

### 4.7 Reason codes

No `String` on the hot path. The core emits a stable enum; the UI maps it to text.

```
OutsideCollar, InsufficientFunds, InsufficientInventory, HoldingLimitExceeded,
SelfTrade, UnknownMarket, MarketClosed, DuplicateClientOrderId, InvalidQuantity,
InvalidPrice, PriceNotTickAligned, OrderNotFound, NotOrderOwner, CancelNotAllowed,
RateLimited, QueueFull, AuctionNotActive, AuctionClosed, StaleAuctionPrice,
EscrowNotReady, InvalidReceipt, ReceiptExpired, InternalError
```

`Rejected { reason: String }` in the scaffold is replaced by `Rejected { code: ReasonCode }` (plus an
optional server-side detail string only in dev logs, never on the hot path).

---

## 5. Domain rules

### 5.1 Market and collar

```rust
struct MarketConfig {
    id: MarketId,
    event_id: u32,
    name: String,
    face_value_minor: i64,     // e.g. 500_000 paise = Rs 5,000
    tick_size_minor: i64,      // e.g. 100 paise = Rs 1 per tick
    collar_floor_bps: u16,     // e.g. 5000 = 50% of face
    collar_cap_bps: u16,       // e.g. 12000 = 120% of face
    holding_limit: u32,        // max passes held + open buys per identity
    taker_fee_bps: u16,        // e.g. 100 = 1%
    escrow_enabled: bool,
    status: MarketStatus,
}
```

Collar math (integer only, `u128` intermediates):

```
face_ticks  = face_value_minor / tick_size_minor      // config must divide exactly
floor_ticks = ceil(face_ticks * collar_floor_bps / 10_000)
cap_ticks   = floor(face_ticks * collar_cap_bps / 10_000)
price must be in [floor_ticks, cap_ticks]
```

The collar is enforced in three places: client validation (UX), gateway pre-check (cheap rejection),
and core validation (authoritative). A client that bypasses the UI still cannot gouge.

Market config is immutable while the market is open. Changing face value, tick size, collar, or fee
requires closing the market and cancelling open orders; otherwise resting orders could fall outside
the new bounds. This is a deliberate simplification for v1.

### 5.2 Order lifecycle

```
Received → Reserved → Accepted → (Rested | PartiallyFilled | Filled | Cancelled | Rejected)
```

- **Limit:** reserve cash (buy) or passes (sell), then match against the opposite side by price-time
  priority. Remainder rests at the limit price.
- **Market:** reserve at the protected price (buy) or reserve passes (sell); sweep only inside the
  collar; cancel any unfilled remainder (IOC semantics).
- **Cancel:** O(1) via order-id → slab handle; release the remaining reservation.
- **Self-trade prevention:** cancel the resting order, continue matching the incoming order. The
  cancelled maker's reservation is released. (Configurable per market later.)
- **Partial fills:** each fill is settled atomically; the order keeps its queue position until fully
  filled or cancelled.

### 5.3 Reservations and settlement

- Buy limit reserves `price_ticks * tick_size * qty + estimated_fee`. The actual fill price is `<=`
  limit, so the actual cost is `<=` reserved; the difference is released after each fill.
- Buy market reserves at `protect_price_ticks` (or the collar cap if absent).
- Sell reserves pass quantity.
- A trade journal entry moves cash and passes together:

```
Buyer cash held      -notional - fee
Seller cash available +notional
Platform cash        +fee
Seller pass held     -qty
Buyer pass available +qty
```

Each asset sums to zero across the postings. The entry is applied in one step with a unique
idempotency key (`trade_id` / `(seq, fill_index)`). A crash cannot leave money in limbo.

### 5.4 Holding limits

For a buy, require:

```
pass_available + pass_held + open_buy_remaining + qty <= holding_limit
```

`open_buy_remaining` is maintained by the core and decremented on fill/cancel. This reserves holding
capacity up front, so a fill cannot fail the limit mid-match. Sells are bounded by the pass reservation
(`pass_available >= qty`).

### 5.5 Ledger invariants

1. For each asset, `sum(available + held) == 0` across all accounts, including system/mint accounts.
2. Every user account has `available >= 0` and `held >= 0`.
3. `held` equals the sum of open reservations (orders + escrow).
4. Pass supply per market is constant.
5. Every journal entry balances per asset.
6. `seq` strictly increases; idempotency keys are unique.
7. A reconciliation job recomputes 1–5 continuously and reports mismatches.

System/mint accounts may be negative (they represent issuance/liabilities); user accounts may not.

### 5.6 Escrow

Per-market `escrow_enabled`. For an escrow fill:

```
Filled → PendingDelivery → Delivered/Released
                        ↘ TimedOut → Refunded
```

- Buyer cash and seller passes remain held in reservation accounts.
- Seller submits an ed25519-signed receipt over
  `(trade_id, market_id, qty, buyer_id, seller_id, delivered_at_ms)`.
- Gateway verifies the signature against the seller's registered public key; the core releases funds
  and passes atomically.
- On timeout, a refund command releases both sides' reservations.
- Disputes/admin resolution are out of scope for v1 (documented as a stretch).

### 5.7 Dutch auction

- Published linear decay from `start_price_ticks` to `floor_price_ticks` over a fixed duration,
  rounded down to a tick.
- Price is a pure function of `server_time_ms`; the client computes it locally from a measured clock
  offset, and the server recomputes it when the command is sequenced.
- `Accept { auction_id, qty, max_price_ticks }` is a compare-and-set in the core: if active and
  `qty_remaining >= qty`, decrement and settle at the **current computed price**, provided it is
  `<= max_price_ticks`. First valid acceptance wins; ties are broken by `seq`. Using a maximum price
  instead of an exact expected price avoids rejecting a buyer because the price decayed during network
  latency; the buyer never pays more than they authorised.
- No price stream is sent tick-by-tick; the formula and parameters are published.

### 5.8 Batch auction (commit-reveal)

- Phases: `Commit → Reveal → Clearing → Settled`.
- Commit stores `sha256(canonical_order || nonce)`; reveal submits the order + nonce and is verified.
- Reservations happen at reveal, not commit, so a failed reveal costs nothing.
- At close, orders are cleared at one uniform price that maximises matched volume; ties are broken
  deterministically (closest to midpoint, then lower price, then `seq`). Unmatched orders are cancelled
  and reservations released.
- Speed inside the window confers no advantage; the UI shows phase countdowns.

### 5.9 Solvency and audit

- **Merkle-sum per asset:** leaves are `H(salt || user_id || asset || balance)` where
  `balance = available + held`; internal nodes commit to `left.sum + right.sum`. Publish one root and
  total liabilities per asset, plus the snapshot `seq`.
- User proofs include leaf, salt, path, root, total; the browser recomputes the path and sum.
- **Audit log:** `hash = sha256(prev_hash || canonical_msgpack(record))`; records are append-only.
  The browser can fetch records and verify the chain. Publish the head hash periodically.
- State openly that reserves are simulated; the proof shows inclusion in liabilities, not backing by
  real funds.

### 5.10 Replay and WAL

- The core keeps an ordered command log. Replaying from `seq = 0` (or the latest snapshot) must produce
  the same state hash at every checkpoint.
- WAL records: `[len][seq][timestamp][payload][prev_hash][hash]`, group-committed and fsynced. Snapshots
  are written atomically (temp + rename). Recovery = latest snapshot + log tail; hash chain verified.
- The UI replay uses periodic snapshots (every N commands) plus deltas; it never re-runs the live core.

---

## 6. Build order

Each block ends with a demoable result. Dependencies are explicit; parallel tracks are marked.

### Block 0 — Contracts and foundation (2–3 days)

**Goal:** freeze the shapes everyone codes against.

- Move spec/diagrams into `docs/spec/` (done) and write `docs/DECISIONS.md` (done).
- Expand `dx-types`: IDs, units, market config, commands, results, events, snapshots/deltas, reason
  codes, auction/escrow types.
- Remove `user_id` from command bodies; add `client_order_id`; replace `reason: String`.
- Add JSON fixtures for every message + a manifest; Rust round-trip tests.
- Add `rmp-serde`; generate golden msgpack bytes; JS decode tests.
- Add `ts-rs` export + `jsconfig.json` (`checkJs`) + `// @ts-check` + `npm run typecheck`.
- Add the single runtime codec `web/src/protocol/codec.js`.
- Add CI (Rust + web + contract generation check).
- Seed 3 demo markets (different face values/collars).

**Done when:** `cargo test`, `npm test`, `npm run typecheck`, and CI pass; every message round-trips
through JSON and msgpack; generated types are current.

### Block 1 — Engine + ledger v1 (8–12 days, parallel)

**Track A — engine**

- `OrderBook` with a flat price-level array bounded by the collar.
- Order slab + intrusive FIFO doubly linked lists per level.
- Best bid/ask bitset scan (O(1) typical).
- `add_limit`, `cancel`, `match`, partial fills, STP.
- Collar validation; market-order sweep within the protected range.
- Deterministic event output + state hash.
- Tests: unit, proptest (no crossed book, FIFO priority, qty conservation), differential vs a naive
  reference matcher, criterion benches (add/cancel/match).
- **Done when:** 100k+ randomized cases pass; benchmark baseline recorded.

**Track B — ledger**

- Accounts keyed by `(user_id, asset)`, `available/held`.
- `reserve`, `release`, `post_trade`, `deposit`, journal entries/postings.
- Idempotency keys; balanced-entry and non-negative checks.
- Invariants + reconciliation.
- Tests: unit + proptest on random postings/trades.
- **Done when:** conservation and `held == reservations` hold for random streams.

**Track C — frontend groundwork**

- Protocol codec + mock feed.
- Dark theme tokens; market list, ladder, order ticket, balances shell with mock data.
- Zustand stores; Web Worker skeleton; Canvas depth chart scaffold.
- **Done when:** the shell renders a mock book at 60 fps and the ticket validates input.

### Block 2 — Core + vertical slice (7–10 days)

**Goal:** the first real end-to-end demo.

- `dx-core`: `Core::process(auth, command) -> CommandResult + events`; sequencer; idempotency cache;
  market config; reserve → match → settle; holding limits; state hash.
- In-memory command log + replay test.
- Gateway: REST endpoints, dev JWT, WS ticket, rate limiter, collar pre-check, bounded ring, core
  thread, event fan-out.
- Publisher: cached L2 snapshot, batched absolute deltas (20–50 ms), bounded WS send buffers,
  `ResyncRequired`.
- Web: real WS in a worker via Comlink; decode msgpack; book reconstruction; gap detection/resync;
  live ladder, tape, ticket, balances, open orders.
- Playwright: two users place/cancel/fill; reconnect resync.

**Done when:** two browser sessions trade against each other, the ladder/tape/balances update, and a
forced disconnect resyncs without corruption. Record end-to-end latency.

### Block 3 — Fairness pack (7–10 days)

- Collar + holding-limit UI (server enforcement already in core); rejection explanations.
- Per-user/per-IP token buckets; adaptive PoW challenge; `429`/`Retry-After`; bot simulator.
- Dutch auction: schedule, price function, accept, UI countdown.
- Batch auction: commit/reveal/clear, uniform price, UI phases.
- Metrics: hdrhistogram, throughput/p50/p99/queue depth/reject rate, WS metrics message, UI panel.
- Order-book replay: in-memory snapshots + scrubber.

**Done when:** out-of-collar orders are rejected via API; the bot simulator shows measured rejection
rates with defenses on/off; both auctions clear deterministically; the metrics panel updates live.

### Block 4 — Trust pack (7–10 days)

- Merkle-sum tree per asset + proofs + browser verification page.
- Hash-chained audit log + browser verification.
- Escrow state machine + ed25519 receipt verification + timeout refunds.
- Continuous reconciliation job + invariant dashboard/endpoint.

**Done when:** a user can verify their balance inclusion in the browser; the audit chain verifies;
escrow releases only with a valid receipt and refunds on timeout.

### Block 5 — Hardening and ship (7–10 days)

- WAL + snapshots + crash recovery (kill -9 test, bit-identical state hash).
- PostgreSQL migrations + projection worker + read endpoints; DB outage does not stop trading.
- k6 load tests (1k orders/s), threshold checks, results in `bench/results/`; Locust optional.
- Dockerfiles + full Compose + Caddy/Nginx; one-command demo.
- Docs: README, ARCHITECTURE, PROTOCOL, TOOLS, demo script, ADRs.
- Security review: authz, input limits, CORS, WS auth, secrets, DB permissions.

**Done when:** the demo runs from a clean checkout, survives an API restart, meets the measured
throughput/latency targets on demo hardware, and the docs are complete.

---

## 7. Sprint 0 checklist — start here

Ordered; the **first code change is `dx-types`**.

1. **Docs**
   - [x] `docs/spec/` with PDF + diagrams + README
   - [x] `docs/DECISIONS.md`
   - [x] `docs/PLAN.md`
   - [ ] `docs/adr/0001..0008` for the locked decisions
   - [ ] Update root `README.md` with the new layout and commands
2. **`dx-types` v1**
   - [ ] `ids.rs`, `money.rs`, `market.rs`, `order.rs`, `command.rs`, `result.rs`, `event.rs`,
         `book.rs`, `error.rs`, `auction.rs`, `escrow.rs`, `solvency.rs`
   - [ ] `MarketConfig` + `seed_markets()` (3 markets)
   - [ ] `Command`, `CommandResult`, `Event`, `ClientMessage`, `ServerMessage`
   - [ ] `ReasonCode`; remove `String` reasons from the hot path
   - [ ] `client_order_id`; remove `user_id` from command bodies
   - [ ] `#[serde(deny_unknown_fields)]` on commands; explicit `type` tags
3. **Contracts**
   - [ ] `contracts/README.md` explaining JSON fixtures vs msgpack bytes
   - [ ] One JSON fixture per message + `contracts/manifest.json`
   - [ ] Rust round-trip tests for every fixture
   - [ ] Golden `.msgpack` generator + JS decode tests
4. **JS safety rails**
   - [ ] `ts-rs` export to `web/src/protocol/generated.d.ts`
   - [ ] `web/jsconfig.json` with `checkJs`/`strict`/`noEmit`
   - [ ] `// @ts-check` in protocol/store/worker files
   - [ ] `npm run typecheck` + CI step
   - [ ] `web/src/protocol/codec.js` (encode/decode/validate) + tests
5. **CI**
   - [ ] `.github/workflows/ci.yml`: rust fmt/clippy/nextest; web lint/typecheck/test/build;
         contract-generation check
6. **Engine start (first slice)**
   - [ ] `OrderBook`, `BookSide`, `PriceLevel`, `OrderSlab` skeletons
   - [ ] `add_limit` rests an order and updates best bid/ask
   - [ ] `cancel` removes it in O(1)
   - [ ] First unit tests + a criterion bench

**Sprint 0 done when:** CI is green, every message round-trips, the JS codec decodes golden msgpack,
and the engine can rest/cancel a limit order with a correct best bid/ask.

---

## 8. Testing and verification

### Rust

- **Unit:** price-level add/remove, FIFO, partial fills, cancel, collar boundaries, fee rounding,
  reservation release, auction price rounding.
- **Property (`proptest`):** random command streams; assert no crossed book, price-time priority,
  quantity conservation, no negative balances, balanced entries, `held == reservations`, deterministic
  state hash.
- **Differential:** a deliberately simple reference matcher (sorted Vec) runs the same streams; fills
  and resting book must match exactly.
- **Replay:** log a random stream, replay into a fresh core, compare state hashes at checkpoints.
- **Criterion:** add/cancel/match at shallow and deep books; record results in `bench/results/`.

### Contracts

- Rust serializes every fixture and deserializes it back.
- Rust writes golden msgpack; JS decodes and compares to the JSON fixture.
- Unknown command fields are rejected; unknown event fields are ignored for forward compatibility
  (document the choice).
- CI checks generated `.d.ts` and fixtures are current.

### Web

- **Vitest:** codec, book reconstruction, absolute-delta application, gap detection, resync,
  Zustand selectors.
- **fast-check:** random deltas with drops/reorders; the reconstructed book must converge after a
  resync.
- **Playwright:** two users trade; cancel; out-of-collar rejection; reconnect; auction flow; solvency
  proof verification; audit-chain verification.

### Load

- **k6:** open-model arrival rate, ramp to 1,000 orders/s; thresholds on throughput, p50/p99, error
  rate; results committed.
- **Locust:** optional Python user flows (browse → order → cancel).
- **WS load:** optional extension; slow-consumer/resync behavior.

### CI gates

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
npm --prefix web run lint
npm --prefix web run typecheck
npm --prefix web test
npm --prefix web run build
```

Nightly/manual: Playwright, k6, benchmark regression.

---

## 9. Performance plan

1. **Measure early.** Criterion in Block 1; k6 in Block 2/3; do not wait for Block 5.
2. **Release profile:** `lto = "thin"`, `codegen-units = 1`; benchmark release builds.
3. **Hot path:** no allocation, no `String`, no locks, no I/O; preallocate slab, rings, event buffer;
   reuse buffers; avoid `HashMap` iteration for output ordering.
4. **Rings:** `crossbeam-queue::ArrayQueue` (bounded), power-of-two capacity; gateway rejects when full
   with `429/503` + `Retry-After`.
5. **WS:** batch diffs every 20–50 ms; bounded per-client buffers; `permessage-deflate` off.
6. **DB:** projection off the hot path; batched inserts; pooled connections; reads isolated.
7. **Metrics:** hdrhistogram for p50/p99/p99.9; record core processing, gateway ack, and client-visible
   latency separately.
8. **Report honestly:** publish the hardware, command mix, and measured numbers; no invented claims.

---

## 10. Security and operations

- **Authz:** derive `user_id` from the JWT; never trust it from the body. Cancel only own orders.
- **Input validation:** schema, enum, size, quantity/price bounds; checked arithmetic; reject overflow.
- **Abuse:** per-user and per-IP token buckets; adaptive PoW; body size limits; WS ticket auth;
  CORS allowlist; connection limits.
- **Ledger:** append-only journal; DB `CHECK (available >= 0)`; unique idempotency keys; no
  `UPDATE`/`DELETE` on ledger rows (privileges/triggers as a second line of defence).
- **Secrets:** env only; `.env` not committed; JWT secret and ed25519 keys rotated for demos.
- **Audit:** hash-chained records; publish head; browser verification.
- **Deploy:** Docker multi-stage builds; Caddy/Nginx TLS + static cache; health checks; restart policy;
  one-command demo script.

---

## 11. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Scope too large for the deadline | High | Vertical slice first; cut list in §12; weekly demo |
| JS/server protocol drift | Medium | Generated `.d.ts`, `checkJs`, golden msgpack fixtures, one codec, same-PR rule |
| Performance claims not met | High | Benchmark from Block 1; measure real hardware; tune rings/allocations; report actuals |
| Concurrency/settlement bug | High | Single writer, reservations before match, proptest, differential tests, replay hash, reconciliation |
| Auction complexity | High | Dutch first (simple formula), batch second; deterministic clearing with tests |
| Crypto bugs | Medium | Audited crates (`sha2`, `ed25519-dalek`), fixed test vectors, simple tree, browser verifier |
| Integration discovered late | Medium | End-to-end slice in Block 2; Playwright from then on |
| Team bandwidth | Unknown | Assign owners per track; rotate integration; cut from P2 |
| WAL/DB destabilizes demo | Medium | Keep in-memory path authoritative; WAL/DB off by default with feature flags |

---

## 12. Priority and cut list

### P0 — cannot demo without it

- Engine: limit, cancel, FIFO, collar, STP.
- Ledger: reservations, balanced settlement, invariants.
- Core: single writer, sequencer, idempotency.
- REST + WS + live L2 UI + order ticket + balances.
- Reconnect/resync.
- Tests + CI.

### P1 — the spec's fairness/trust story

- Market orders.
- Holding limits + collar UI.
- Dutch auction.
- Merkle-sum proof + audit log.
- Metrics/latency panel.
- Rate limits + PoW.
- In-memory replay.

### P2 — full-spec depth

- Batch auction + commit-reveal.
- Escrow + signed receipts.
- Bot simulator.
- WAL + crash recovery.
- PostgreSQL projections.
- k6 thresholds in CI.
- Caddy/Nginx + hosted demo.

**Cut order if time runs out:** escrow → PostgreSQL → WAL → batch auction → bot simulator → Dutch
auction. Keep the engine, ledger, live UI, collars, and solvency proof.

---

## 13. Target demo script

1. Log in as two seeded users; show simulated deposits and identity-bound limits.
2. Browse markets: face value, live L2 ladder, depth chart, trade tape, collar.
3. User A places a bid; user B places an ask; trade fills; ladder, tape, balances, and open orders update.
4. Submit an out-of-collar order via the API/UI; show the server rejection.
5. Turn the bot simulator on/off; show measured rejection rates and latency with collars/rate limits.
6. Dutch auction: price decays on schedule; accept; settlement updates balances.
7. Batch auction: commit, reveal, clear at one uniform price.
8. Solvency page: verify the user's leaf and the published root in the browser.
9. Audit page: verify the hash chain.
10. Replay: scrub backwards through the book/price history.
11. Metrics panel: throughput, p50/p99, queue depth, reject rate.
12. Restart the API (if WAL done); state recovers to the same hash.

---

## 14. Open questions

1. Exact deadline date (to map six blocks to weeks).
2. Team strengths/hours and ownership (engine/ledger/frontend/integration).
3. Brand/UI direction for "Darkhold" (dark exchange theme from scratch?).
4. Taker-only fee acceptable?
5. Hosted demo URL or laptop only?
6. Is the PDF in `docs/spec/` the final spec?
