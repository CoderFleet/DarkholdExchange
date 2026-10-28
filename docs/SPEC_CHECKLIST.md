# Darkhold Exchange — Spec Checklist

> **Source of truth:** [`docs/spec/darkhold-spec.pdf`](spec/darkhold-spec.pdf)
>
> **Baseline audited:** `main` @ `9a764ee` on 2026-10-28
>
> **Purpose:** this is the short working checklist for the Webster 2026 spec. Update the relevant
> row when work lands instead of re-reading the PDF or re-auditing the whole repository.
>
> **Status:** ✅ done · 🟡 partial · ❌ missing · 🚫 deliberate decision / out of scope

## Headline promises

| Spec promise | Status | Note |
|---|---|---|
| FIFO limit order book with limit/market/cancel/STP | ✅ | Engine, core, and UI paths are live and tested. |
| Server-enforced price collars + holding limits | ✅ | Enforced by the core on every command. The gateway relies on core validation; there is no separate gateway pre-check. |
| Atomic double-entry ledger with reservations and invariants | ✅ | Ledger, settlement, idempotency, and property tests are in place. |
| Live L2 ladder, depth chart, trade tape, WS diffs + resync | ✅ | Live market terminal is working, including charts and WebSocket rebuild/resync. |
| Dutch-auction liquidation engine | ✅ | Seller-backed auctions reserve passes and decay linearly from start to floor. Accept is a compare-and-set at the server-computed price; unsold passes return on expiry. `/auctions` UI and WAL replay are covered. |
| Frequent batch auctions with commit-reveal | ❌ | Not implemented. |
| Escrow with signed delivery receipts, disputes, and timeout refunds | ✅ | Per-market escrow holds buyer cash and seller passes in system accounts, releases only against an ed25519 receipt, and refunds after the deadline. A buyer can open a dispute after delivery and an admin ruling resolves it to a buyer refund or seller release. Live flow released a market-3 trade and survived WAL restart. |
| Merkle-sum proof of solvency | ✅ | Real per-asset Merkle-sum trees, inclusion proofs, `/api/solvency`, `/api/me/solvency`, and `/verify`. Live check showed 4/4 proofs valid. |
| Hash-chained public audit log + browser verification | 🟡 | Hash primitives and browser verifier exist and are tested, but no audit records are produced or exposed, and there is no audit UI. |
| Per-identity/per-IP rate limits + adaptive proof-of-work | ✅ | Token-bucket rate limits per IP, per user and for login/sign-up (429 + `Retry-After`, `docs/AUTH.md` section 8). Addresses that keep hitting them must solve signed, single-use, rising-difficulty puzzles (428; `docs/AUTH.md` section 9). |
| Bot/scalper simulator + bots-vs-humans switch | 🟡 | `dx-simulator` / `dx-load` can generate load, but there is no attack/defence UI switch. |
| Order-book replay/scrubber | 🟡 | Core replay is covered by a test; there is no replay API or UI scrubber. |
| Live metrics/latency panel | ✅ | Gateway histograms + `/api/metrics/latency` + `/speed` panel are merged. |
| PostgreSQL read models | 🟡 | `dkx-projection` and migrations exist; the gateway does not use them yet. |
| WAL + crash recovery | ✅ | Gateway writer group-commits every non-cached command, successful deposit/issuance, and market creation as fsynced operation records with a post-state checkpoint at least every 256 operations. Startup replays and verifies the checkpoints. Live SIGKILL restart preserved the same state hash and book. |
| Nginx/Caddy + Docker deployment | ❌ | `docker-compose.yml` only starts PostgreSQL. There is no app container, proxy, or one-command deployment. |
| 1,000+ orders/s, µs engine p99, <10 ms e2e p99 | ✅ | WAL-on 2026-10-26 runs: 995.50 placements/s and 999.90 fills/s; p99 7.54 ms place, 8.88 ms fill. Recorded under `bench/results/`. Not enforced in CI. |
| Differential testing vs reference matcher | ❌ | Deferred by T1; not planned for the current demo. |
| Deterministic replay from the log | ✅ | Gateway recovery replays WAL operations into the seeded core and checks stored post-state checkpoints. Workspace tests and a live SIGKILL restart produce the same final hash. |
| k6/Locust thresholds | ❌ | `bench/k6` and `bench/locust` are empty; the custom Rust harness is the current substitute. |
| Playwright E2E | ❌ | `@playwright/test` is installed, but there is no config or E2E suite. |
| fast-check property tests | ❌ | `fast-check` is installed, but no frontend property tests use it. |
| TypeScript frontend | 🚫 | Deliberate decision to stay on JavaScript; see `docs/DECISIONS.md`. |
| Comlink worker RPC | 🚫 | `comlink` is a declared dependency, but the worker uses raw `postMessage`. Keep the dependency only if there is a plan to use it, otherwise remove it. |

## Detailed checklist

| # | Requirement | Status | Evidence / current reality | Next action |
|---|---|---|---|---|
| 1 | FIFO price-time priority, limit/market/cancel, STP | ✅ | `crates/engine`, `crates/core`, engine/core tests. | — |
| 2 | Server-enforced collars from face value | ✅ | Core rejects out-of-collar orders; UI shows the collar. | Optional: add a cheap gateway pre-check if the spec wording requires it. |
| 3 | Per-identity holding limits | ✅ | Core tracks open buys, held passes, and pending escrow passes for the buyer. | — |
| 4 | Double-entry ledger, reservations before matching | ✅ | `crates/ledger`, atomic settlement, conservation and negative-balance tests. | — |
| 5 | Idempotency keys and replay-safe retries | ✅ | Core idempotency cache + tests. | — |
| 6 | JWT auth + roles | 🟡 | Real login/signup/JWT, admin/issuer roles, durable accounts when `DATABASE_URL` is set. Private terminal/wallet/history routes require a login; the client never sends an impersonating dev header. | Check WS handshake auth (row 7). |
| 7 | WebSocket auth | ❌ | WS accepts `?user_id=` and does not verify a JWT. | Authenticate the upgrade request; reject spoofed identities. |
| 8 | L2 snapshot + sequenced diffs + gap resync | ✅ | Gateway publisher, worker rebuild, market terminal. | — |
| 9 | 20–50 ms batching and slow-consumer protection | ✅ | WS batching, bounded buffers, slow-consumer metrics. | — |
| 10 | Dutch auction | ✅ | `crates/types/src/auction.rs` (integer decay curve), core create/accept/expire with CAS quantity, reserved seller passes, atomic settlement, WAL replay, and the `/auctions` UI. | Optional: background expiry timer and seller cancellation before the deadline. |
| 11 | Batch auction + commit-reveal | ❌ | Nothing implemented. | Defer until P1 items are complete, unless the submission needs it. |
| 12 | Escrow state machine | ✅ | `PendingDelivery → Delivered → Released` on a valid receipt, or `Refunded` after the deadline. After delivery the buyer can open `Disputed`; an admin ruling resolves it to `Released` (seller) or `Refunded` (buyer). Ledger hold/release/refund journals, writer state, admin routes, and the admin disputes page are tested. Market 3 is escrow-enabled; admin-published tiers can opt in. Refund is triggered manually by either party after the deadline; there is no background timer yet. | Optional T3 timer service to auto-submit refunds. |
| 13 | ed25519 signed delivery receipts | ✅ | `dx-types` canonical bytes, core seller-key registry, `ed25519-dalek` verification, WAL-logged key registration/release, and browser Web Crypto seller signing. | — |
| 14 | Merkle-sum solvency | ✅ | `crates/solvency`, `crates/hash/src/merkle.rs`, gateway routes, `/verify`, shared golden vectors. Snapshot `seq` uses the core state version, so deposits and issuance advance it. | — |
| 15 | Hash-chained audit log | 🟡 | `crates/hash/src/audit.rs` + `web/src/lib/hash.ts` + golden vectors. No records/endpoint/UI. | Record sequenced commands, expose `GET /api/audit`, verify the chain on `/verify`. |
| 16 | Per-user/per-IP rate limits | ✅ | `crates/limits` token buckets, applied by `crates/gateway/src/rate.rs`; 429 + `Retry-After`; env-configurable; off for load tests. | Keep numbers under review after the load test. |
| 17 | Adaptive proof-of-work | ✅ | `crates/pow` (signed single-use challenges, strikes), `GET /api/pow/challenge`, 428 `pow_required`, browser Web Worker solver in `web/src/pow/`. | Tune the difficulty after the load test. |
| 18 | Bot/scalper simulator + defence switch | 🟡 | `dx-simulator`, `dx-load`, `scripts/load-test.sh`, recorded results. | Add attack/defence scenarios and a UI toggle if the demo needs it. |
| 19 | Order-book replay/scrubber | 🟡 | Core replay test; no live replay API. | Add snapshot + delta replay endpoint and a UI time scrubber. |
| 20 | Live metrics/latency panel | ✅ | `/api/metrics/latency`, gateway histograms, `/speed`, `latency.*`, `SpeedPanel.jsx`. | Populate it by placing orders during the demo. |
| 21 | PostgreSQL read models | 🟡 | `crates/dkx-projection`, migrations, SQL queries. Not wired into gateway. | Start the projection worker behind a flag; add integration test. |
| 22 | WAL + crash recovery | ✅ | `dkx-wal` record/hash-chain/recovery plus gateway `wal.rs`; writer group-commits commands, deposits, issuance, and market creation, with a post-state checkpoint at least every 256 operations. Live check placed 261 commands (crossing a state checkpoint) plus a resting order, `SIGKILL`ed the gateway, restarted, and matched `/api/state/hash` and the book exactly. | Full-log replay is wired; periodic snapshots/compaction are still a follow-up. |
| 23 | Docker/Caddy/nginx deployment | ❌ | Compose only includes `db`. | Add app image, proxy config, health checks, one-command demo. |
| 24 | Performance targets | ✅ | `bench/results/` records WAL-on 2026-10-26 runs: 995.50 placements/s, 999.90 fills/s, 7.54 ms place ack p99, 8.88 ms fill ack p99. | Add CI thresholds or clearly document the substitute. |
| 25 | Differential matcher test | ❌ | No reference matcher. Deferred by T1. | Not planned for the current demo; revisit only if matching correctness needs stronger verification. |
| 26 | Deterministic WAL replay | ✅ | `wal::replay` rebuilds state and verifies periodic post-state checkpoints; writer recovery tests and the live restart check pass. | — |
| 27 | k6/Locust load thresholds | ❌ | Empty `bench/k6` and `bench/locust`; Rust harness used instead. | Add k6 or document a justified equivalent. |
| 28 | Playwright E2E | ❌ | Package installed; no config/tests. | Add smoke flows: login, place order, fill, verify, speed. |
| 29 | fast-check frontend tests | ❌ | Package installed; no usage in `web/src`. | Add property tests for book rebuild/resync or remove the dependency. |
| 30 | TypeScript frontend | 🚫 | JavaScript decision recorded in `docs/DECISIONS.md`. | Keep the decision documented; do not half-migrate. |
| 31 | Comlink worker RPC | 🚫 | Package installed; worker uses raw `postMessage`. | Either adopt it or remove the unused dependency. |
| 32 | Admin publishing reaches the engine | ✅ | Admin publish → writer `CreateMarket` → `Core::add_market` → engine book; tested at engine, core, writer, and gateway. Live check published market 100 and `/api/markets` returned it `Open` with book seq 0. | New markets appear via `/api/markets` on reload; no WS market-opened frame yet. |
| 33 | Issuer console | ✅ | `IssuerPage` credits passes by username (a retry cannot double-credit) and shows how many passes users hold per market. It uses `POST /api/issuer/issue` and `GET /api/issuer/supply`, issuer role only (`docs/AUTH.md` section 6). `node scripts/seed.mjs` creates demo traders through these endpoints (`docs/AUTH.md` section 7). | — |

## Work ownership

Source: `Darkhold_Work_Distribution.pdf` (8 pages, read 2026-10-27).

| Owner | Owns |
|---|---|
| **Teammate 1 — Rudransh Pratap Singh** | Shared message types, command queue/main loop, order book, matching, pre-book checks, escrow switch, live update sender, order/WS/market endpoints, market setup command, replay/recovery helpers, state fingerprint, speed measurements. Frontend: market page, order form, ladder, depth chart, trade list/price chart, open orders, speed panel, shared live worker, market store. |
| **Teammate 2 — Shraddha Sharma** | Ledger, locking, settlement, safety checks, wallet, escrow, disputes, shared hashing helper, hash-chained log, Merkle-sum solvency, PostgreSQL copy, replay service, private user WebSocket. Frontend: wallet, history/statements, trade/escrow detail, admin disputes, proof page, replay page. |
| **Teammate 3 — Stuti Agarwal** | Gateway, login/roles, rate limits/PoW, admin setup, Issuer, seed script, timer service, batch auction, Dutch auction, bot simulator, server clock, Docker/proxy, k6/Locust/Playwright. Frontend: app shell, API helper, auth store, login/signup, events list, event room, batch page, Dutch page, admin console, issuer console, bot simulator. |

### Remaining work by owner

| Owner | Remaining checklist items |
|---|---|
| **T1 — Rudransh** | WS endpoint auth (with T3); optional Playwright place/match/cancel smoke test. |
| **T2 — Shraddha** | Audit-chain records + `GET /api/audit` + browser verification; WAL snapshot compaction; PostgreSQL projection; replay service/page; private user WS. |
| **T3 — Stuti** | Batch auction + commit-reveal; escrow timer service; bot simulator UI; Docker/Caddy/nginx; k6/Locust thresholds; Playwright setup; WS auth (with T1). |

The trust work landed in **T2 (Shraddha)** territory: Merkle-sum solvency and the `/verify` page. The speed panel and market charts are **T1 (Rudransh)** work. **T1 landed:** gateway WAL/crash recovery, deterministic replay, `/api/state/hash`, WAL-on benchmarks, and the solvency state-version fix.

## Known caveats / quick fixes

- **WAL snapshots/compaction are not wired:** startup rejects a snapshot and replays the full log.
  Periodic snapshots remain a follow-up; the current path is correct but grows with the log.
- **WAL replay baseline is the demo seed:** `seed_demo` is not logged. Changing the seed changes the
  starting state, so an old log will fail its stored state-hash check and startup will abort. Clear
  `data/log` after changing the seed in development.
- **WAL is enabled by default:** records land under `<DX_WAL_DIR>/data/log` (default `./data/log`).
  Set `DX_WAL=0` for in-memory benchmarks. If append/fsync fails, the writer stops rather than
  serving unlogged state.
- **WAL state checkpoints are periodic:** a full core state hash is written at least every 256
  operations, not after every command. The per-record hash chain still authenticates every WAL
  record; the checkpoint catches non-deterministic replay at the next boundary. A short run may end
  with a trailing uncheckpointed batch, which replay accepts.
- **Admin-published markets need a reload:** publishing creates the market and its book in the
  engine, but there is no WebSocket “market opened” frame yet. The market appears through
  `/api/markets` on the next load. Issuance is still required before trading.
- **Escrow timeout is 5 minutes and manual:** either party can trigger the refund after the deadline;
  there is no background timer service yet. Buyer disputes and admin rulings are implemented after
  delivery, but they are still opened and resolved manually.
- **Escrow seller keys in the browser demo live in `localStorage`:** the public key is registered in
  core/WAL, but the private key is browser-local and is not a production key-management story.
- **Dutch auctions expire manually:** after the deadline the seller must call expire to recover
  unsold passes; there is no background timer and no cancellation before the deadline. Auction
  fills settle atomically and bypass the escrow receipt flow, even on escrow-enabled markets. The
  `/auctions` page polls for updates rather than streaming them, and auction fills are not yet
  added to the order-book tape or `/api/history`; balances and wallet holdings update immediately.
- **Market 3 is the seeded escrow market:** it starts with an empty book and seeded pass inventory.
  Old WALs from before the market-3 seed change fail their state-hash checkpoint; clear `data/log`
  in development.
- **WebSocket identity is unauthenticated:** `WsQuery.user_id` is accepted from the URL.
- **`web/full-smoke-run.mjs` is not part of the repo:** it has a hard-coded temp path and imports
  `playwright` instead of the declared `@playwright/test`. Keep it untracked or rewrite it as a real
  Playwright test.
- **Earlier audit note is stale:** the Merkle-sum solvency proof is implemented and live; the speed
  panel is also implemented and merged. Do not revert those rows to “missing/WIP.”

## Recommended work queue

1. **WS auth (T1/T3):** authenticate the WebSocket upgrade and reject spoofed user IDs.
2. **Trust finish (T2 — Shraddha):** audit-chain records + `GET /api/audit` + browser verification section.
3. **Escrow follow-ups (T3):** background timer service for automatic timeout refunds. Buyer disputes and admin rulings are already implemented.
4. **Batch auction (T3 — Stuti, with T1 engine hooks):** commit-reveal and uniform clearing.
5. **Fairness (T3 — Stuti):** rate limits and adaptive proof-of-work are done (rows 16 and 17). The bot-defence UI (bot simulator page) is still to do.
6. **Verification (T1/T2/T3):** optional Playwright E2E, k6 or documented substitute, fast-check usage. WAL replay is done; the differential matcher is deferred.
7. **Operations (T2 + T3):** WAL snapshot compaction, PostgreSQL projection, Docker/Caddy deployment.

## Submission honesty

The PDF abstract names a Dutch-auction engine, escrow, and a cryptographic proof of solvency. The
Dutch auction, escrow v1, and solvency proof are now real. Batch auction is still missing. Before the
deadline, either implement it or explicitly mark it as **not delivered** in the final README and demo
script. Do not let the abstract imply that a running feature exists when it does not.

## Verification commands

```sh
cargo test --workspace
npm --prefix web test
npm --prefix web run typecheck
npm --prefix web run lint
npm --prefix web run build
```

Update this file whenever a row changes. Keep the baseline commit and date current.
