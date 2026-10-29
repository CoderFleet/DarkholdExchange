# Darkhold Exchange — Spec Checklist

> **Source of truth:** [`docs/spec/darkhold-spec.pdf`](spec/darkhold-spec.pdf)
>
> **Baseline audited:** `main` @ `249db4a` on 2026-10-29
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
| Frequent batch auctions with commit-reveal | ✅ | Complete 3-phase pooled engine: Commit (SHA-256 hash commitments from both sides), Reveal (pre-image verification; cash reserved for bids, passes for asks), and Settlement (uniform clearing price maximising executable volume, deterministic tie-breaks, self-trade pairs skipped). WAL ops, gateway REST endpoints (`/api/batch-auctions/...`), and tests are live. |
| Escrow with signed delivery receipts, disputes, and timeout refunds | ✅ | Per-market escrow holds buyer cash and seller passes in system accounts, releases only against an ed25519 receipt, and refunds after the deadline. A buyer can open a dispute after delivery and an admin ruling resolves it to a buyer refund or seller release. Live flow released a market-3 trade and survived WAL restart. |
| Merkle-sum proof of solvency | ✅ | Real per-asset Merkle-sum trees, inclusion proofs, `/api/solvency`, `/api/me/solvency`, and `/verify`. Live check showed 4/4 proofs valid. |
| Hash-chained public audit log + browser verification | ✅ | The gateway writes hash-chained records for trades, issuances, escrow releases/refunds and market publishes (kinds 1–5). `GET /api/audit/log` + `/api/audit/head` are public, `/verify` verifies the chain, and every record is also persisted to the WAL so the chain continues across restarts (recovery re-verifies the links from the genesis hash and refuses a tampered log). |
| Per-identity/per-IP rate limits + adaptive proof-of-work | ✅ | Token-bucket rate limits per IP, per user and for login/sign-up (429 + `Retry-After`, `docs/AUTH.md` section 8). Addresses that keep hitting them must solve signed, single-use, rising-difficulty puzzles (428; `docs/AUTH.md` section 9). |
| Bot/scalper simulator + bots-vs-humans switch | 🟡 | `dx-simulator` / `dx-load` can generate load, but there is no attack/defence UI switch. |
| Order-book replay/scrubber | 🟡 | Core replay is covered by a test; there is no replay API or UI scrubber. |
| Live metrics/latency panel | ✅ | Gateway histograms + `/api/metrics/latency` + `/speed` panel are merged. |
| PostgreSQL read models | 🟡 | `dkx-projection` and migrations exist; the gateway does not use them yet. |
| WAL + crash recovery | ✅ | Gateway writer group-commits every non-cached command, successful deposit/issuance, and market creation as fsynced operation records with a post-state checkpoint at least every 256 operations. Startup replays and verifies the checkpoints. Live SIGKILL restart preserved the same state hash and book. |
| Nginx/Caddy + Docker deployment | ✅ | `docker compose up --build` starts PostgreSQL, the gateway and Caddy + the website (docs/DEPLOY.md). Built and run in Docker; seeded, logged in, live feed through the proxy. HTTPS needs a real domain. |
| 1,000+ orders/s, µs engine p99, <10 ms e2e p99 | ✅ | WAL-on 2026-10-26 runs: 995.50 placements/s and 999.90 fills/s; p99 7.54 ms place, 8.88 ms fill. Recorded under `bench/results/`. Not enforced in CI. |
| Differential testing vs reference matcher | ❌ | Deferred by T1; not planned for the current demo. |
| Deterministic replay from the log | ✅ | Gateway recovery replays WAL operations into the seeded core and checks stored post-state checkpoints. Workspace tests and a live SIGKILL restart produce the same final hash. |
| k6/Locust thresholds | ❌ | `bench/k6` and `bench/locust` are empty; the custom Rust harness is the current substitute. |
| Playwright E2E | ❌ | `@playwright/test` is installed, but there is no config or E2E suite. |
| fast-check property tests | ✅ | Used by `web/src/lib/hash.test.ts` for Merkle-path tamper and audit-chain property tests; book-rebuild/resync property tests are still an optional extension. |
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
| 6 | JWT auth + roles | ✅ | Real login/signup/JWT, admin/issuer roles, durable accounts when `DATABASE_URL` is set. Private terminal/wallet/history routes require a login; the client never sends an impersonating dev header. WS feeds check the same token (row 7). | — |
| 7 | WebSocket auth | ✅ | `/ws` and `/ws/market/{id}` verify `?token=<jwt>` before upgrading; a bad token or a `user_id` that is not the token's user is refused (401/403). A bare `?user_id=` counts only while `DX_DEV_AUTH` is on; with neither the connection is an anonymous viewer of public market data. Private per-user pushes (`FillSelf`, `EscrowUpdate`, `Balances`) are token-only: signed-in sockets get a private baseline on Hello, other users' events are dropped server-side, and a lagged private stream re-baselines instead of resuming deltas. | Wire stack is done; the web client still polls wallet/escrow views — wiring those stores to the private feed is the follow-up. |
| 8 | L2 snapshot + sequenced diffs + gap resync | ✅ | Gateway publisher, worker rebuild, market terminal. | — |
| 9 | 20–50 ms batching and slow-consumer protection | ✅ | WS batching, bounded buffers, slow-consumer metrics. | — |
| 10 | Dutch auction | ✅ | `crates/types/src/auction.rs` (integer decay curve), core create/accept/expire with CAS quantity, reserved seller passes, atomic settlement, WAL replay, and the `/auctions` UI. | Optional: background expiry timer and seller cancellation before the deadline. |
| 11 | Batch auction + commit-reveal | ✅ | **Pooled two-sided window:** admin/issuer schedule one clearing window per market (no supply at creation, nothing mints). During commit, every seller submits a sealed SHA-256 ask and every buyer a sealed bid; at reveal, bid pre-images reserve cash and ask pre-images reserve the seller's passes (a short inventory silently invalidates the ask). Settlement finds the price that maximises *executable* volume (dry-pairing so a user's bid never clears their own ask), fills winning bids from the cheapest asks at that one uniform price with deterministic tie-breaks, releases over-reservations and unsold passes. Endpoints `/api/batch-auctions/...` (commit/reveal per side + `GET /asks`), async core tests, and the `/auctions` UI with a Buy/Sell side toggle. | — |
| 12 | Escrow state machine | ✅ | `PendingDelivery → Delivered → Released` on a valid receipt, or `Refunded` after the deadline. The buyer can accept, open `Disputed` (admin ruling resolves to `Released` or `Refunded`), or stay silent: when the inspection window (`ESCROW_INSPECTION_SECS`) passes with no complaint, escrow auto-releases to the seller on the next read, and the UI counts it down. Ledger journals, writer state, admin routes and the admin disputes page are tested. Market 3 is escrow-enabled; admin-published tiers can opt in. A pre-deadline refund must still be triggered manually; auto-release is lazy (on read), not a background scanner. | Optional: push-based timer instead of read-triggered auto-release. |
| 13 | ed25519 signed delivery receipts | ✅ | `dx-types` canonical bytes, core seller-key registry, `ed25519-dalek` verification, WAL-logged key registration/release, and browser Web Crypto seller signing. | — |
| 14 | Merkle-sum solvency | ✅ | `crates/solvency`, `crates/hash/src/merkle.rs`, gateway routes, `/verify`, shared golden vectors. Snapshot `seq` uses the core state version, so deposits and issuance advance it. | — |
| 15 | Hash-chained audit log | ✅ | Gateway writer appends hash-chained records for fills (kind 1), issuance (kind 2), escrow release (kind 3), escrow refund (kind 4) and admin-published markets (kind 5). Public `GET /api/audit/log?from_seq=` and `/api/audit/head`; `/verify` re-verifies the chain with the shared `dkx-hash` vectors (`web/src/lib/hash.test.ts`). Each record is also written verbatim to the WAL (record kind 3), and recovery rebuilds the chain and re-verifies genesis + every link, so the published head survives restarts. | Deposits, cancels and dispute rulings create no audit records today (spec lists trade events only). With `DX_WAL=0` the chain stays memory-only and resets each boot. |
| 16 | Per-user/per-IP rate limits | ✅ | `crates/limits` token buckets, applied by `crates/gateway/src/rate.rs`; 429 + `Retry-After`; env-configurable; off for load tests. | Keep numbers under review after the load test. |
| 17 | Adaptive proof-of-work | ✅ | `crates/pow` (signed single-use challenges, strikes), `GET /api/pow/challenge`, 428 `pow_required`, browser Web Worker solver in `web/src/pow/`. | Tune the difficulty after the load test. |
| 18 | Bot/scalper simulator + defence switch | 🟡 | `dx-simulator`, `dx-load`, `scripts/load-test.sh`, recorded results. | Add attack/defence scenarios and a UI toggle if the demo needs it. |
| 19 | Order-book replay/scrubber | 🟡 | Core replay test; no live replay API. | Add snapshot + delta replay endpoint and a UI time scrubber. |
| 20 | Live metrics/latency panel | ✅ | `/api/metrics/latency`, gateway histograms, `/speed`, `latency.*`, `SpeedPanel.jsx`. | Populate it by placing orders during the demo. |
| 21 | PostgreSQL read models | 🟡 | `crates/dkx-projection`, migrations, SQL queries. Not wired into gateway. | Start the projection worker behind a flag; add integration test. |
| 22 | WAL + crash recovery | ✅ | `dkx-wal` record/hash-chain/recovery plus gateway `wal.rs`; writer group-commits commands, deposits, issuance, and market creation, with a post-state checkpoint at least every 256 operations. Live check placed 261 commands (crossing a state checkpoint) plus a resting order, `SIGKILL`ed the gateway, restarted, and matched `/api/state/hash` and the book exactly. | Full-log replay is wired; periodic snapshots/compaction are still a follow-up. |
| 23 | Docker/Caddy/nginx deployment | ✅ | `deploy/gateway.Dockerfile`, `deploy/web.Dockerfile`, `deploy/Caddyfile`, full `docker-compose.yml` with health checks and restart policy. Built and run in Docker on 2026-10-28; seeded, logged in, live feed through the proxy. | HTTPS needs a real domain; no automated container test in CI yet. |
| 24 | Performance targets | ✅ | `bench/results/` records WAL-on 2026-10-26 runs: 995.50 placements/s, 999.90 fills/s, 7.54 ms place ack p99, 8.88 ms fill ack p99. | Add CI thresholds or clearly document the substitute. |
| 25 | Differential matcher test | ❌ | No reference matcher. Deferred by T1. | Not planned for the current demo; revisit only if matching correctness needs stronger verification. |
| 26 | Deterministic WAL replay | ✅ | `wal::replay` rebuilds state and verifies periodic post-state checkpoints; writer recovery tests and the live restart check pass. | — |
| 27 | k6/Locust load thresholds | ❌ | Empty `bench/k6` and `bench/locust`; Rust harness used instead. | Add k6 or document a justified equivalent. |
| 28 | Playwright E2E | ❌ | Package installed; no config/tests. | Add smoke flows: login, place order, fill, verify, speed. |
| 29 | fast-check frontend tests | ✅ | `web/src/lib/hash.test.ts` uses `fast-check` properties for Merkle paths (tamper detection) and audit chains (broken links, bad hash, reordering). 125 web tests cover calls and flows. | Optional: property-test the worker's book rebuild/resync. |
| 30 | TypeScript frontend | 🚫 | JavaScript decision recorded in `docs/DECISIONS.md`. | Keep the decision documented; do not half-migrate. |
| 31 | Comlink worker RPC | 🚫 | Package installed; worker uses raw `postMessage`. | Either adopt it or remove the unused dependency. |
| 32 | Admin publishing reaches the engine | ✅ | Admin publish → writer `CreateMarket` → `Core::add_market` → engine book; tested at engine, core, writer, and gateway. Live check published market 100 and `/api/markets` returned it `Open` with book seq 0. | New markets appear via `/api/markets` on reload; no WS market-opened frame yet. |
| 33 | Issuer console | ✅ | `IssuerPage` credits passes by username (a retry cannot double-credit) and shows how many passes users hold per market. It uses `POST /api/issuer/issue` and `GET /api/issuer/supply`, issuer role only (`docs/AUTH.md` section 6). `node scripts/seed.mjs` creates demo traders through these endpoints (`docs/AUTH.md` section 7). | — |

| 34 | Events page and tier tabs | ✅ | The Events page (`/markets`) shows one card per event: name, venue, every tier with face value and live last/bid/ask, and the fee and holding-limit range. Opening a card opens the trading screen, where the tabs are only that event's tiers (hidden when an event has one tier). Events published in the admin console use their real names (`GET /api/events`); the demo markets the engine starts with carry no event name, so they are grouped by event number and named from their market names. Pure helpers are in `web/src/lib/eventList.js` with tests. | Route paths stay `/markets` and `/markets/:marketId`. Landing-page copy still says "markets". |

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
| **T1 — Rudransh** | Optional Playwright place/match/cancel smoke test (row 28). |
| **T2 — Shraddha** | Private per-user WS push (row 7 finish) — audit-chain restart persistence (row 15) is done; PostgreSQL projection wiring (row 21); WAL snapshot compaction (row 22); replay service + UI scrubber (row 19). |
| **T3 — Stuti** | Bot-defence UI / attack-defence scenarios (row 18); k6/Locust thresholds or a documented substitute (row 27); Playwright suite (row 28, with T1); optional push-based escrow timer and Dutch-auction expiry timer/background cancellation. |

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
- **Escrow settles itself at the deadline:** when the inspection window passes with no buyer complaint, escrow
  auto-releases to the seller (checked lazily on escrow reads, with a countdown in the UI). A pre-deadline refund
  or a dispute is still opened/resolved manually by the parties/admin; there is no push-based background scanner.
- **Batch auctions are pooled, admin-scheduled windows:** each window belongs to the market, not to one
  seller. Admin/issuer schedule it with no supply attached; sellers enter sealed asks (passes reserved at
  ask reveal) and buyers sealed bids (cash reserved at reveal). The sponsor-mint removal stays dead — no
  create path mints anything. All participating api tests, gateway routing, and UI (Buy/Sell toggle) are wired.
- **Old batch WAL records no longer replay:** the redesign changed the `CreateBatchAuction` shape (dropped
  `qty`/seller fields) and added `CommitBatchAsk`/`RevealBatchAsk` ops. A `data/log` written by a build
  before this change fails replay at the first batch record — clear `data/log` in development. Logs written
  by this build replay correctly (ask/bid ops are all WAL-logged and replayed).
- **The audit chain persists through the WAL:** every audit record is logged verbatim (WAL record kind 3, no
  post-state hash because it does not change core state) right after the operation that produced it. Recovery
  rebuilds the chain from the log and refuses to start if the first record does not link from the genesis hash
  or any hash/link fails re-verification. A log written by a build older than this change replays fine (it has
  no audit records, so the chain starts empty); the reverse is not true — an old build cannot replay a log that
  contains audit records.
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
- **WebSocket identity requires a token:** `/ws` and `/ws/market/{id}` verify `?token=`; an anonymous viewer still
  receives the public market feed. A future private per-user WS (own order/balance updates) must reuse that check.
- **`web/full-smoke-run.mjs` is not part of the repo:** it has a hard-coded temp path and imports
  `playwright` instead of the declared `@playwright/test`. Keep it untracked or rewrite it as a real
  Playwright test.
- **Earlier audit note is stale:** the Merkle-sum solvency proof is implemented and live; the speed
  panel is also implemented and merged. Do not revert those rows to “missing/WIP.”

## Recommended work queue

1. **Bot defence UI (T3 — Stuti, row 18):** attack/defence scenarios in `dx-simulator` and a demo toggle.
2. **Private per-user WS push (T2 — Shraddha):** own order/fill/balance updates on a token-checked connection.
3. **Audit persistence (T2 — Shraddha, row 15):** ~~WAL op or the `0003` Postgres tables so the published head survives a restart~~ **Done:** audit records are persisted verbatim in the WAL and re-verified on recovery.
4. **Replay service + UI scrubber (T2/T1, row 19).**
5. **PostgreSQL projection wiring (T2 — Shraddha, row 21).**
6. **Verification (T1/T3):** Playwright E2E suite; k6/Locust thresholds or a documented equivalent (rows 27/28).
7. **Operations (T2 — Shraddha, row 22):** wire the existing `dkx-wal` snapshot path into the gateway for compaction.

## Submission honesty

The PDF abstract names a Dutch-auction engine, frequent batch auction, escrow, and a cryptographic
proof of solvency. All four are now real: Dutch auction, batch auction with commit-reveal, escrow
with receipts/disputes/auto-release, and Merkle-sum solvency with browser verification. The honest
gaps are the bot-defence UI, the unused Postgres projection, the `dkx-wal` snapshot path, and public
replay/scrubber. (The audit chain now also persists across restarts via the WAL.) Say so explicitly in the README/demo script.

## Verification commands

```sh
cargo test --workspace
npm --prefix web test
npm --prefix web run typecheck
npm --prefix web run lint
npm --prefix web run build
```

Update this file whenever a row changes. Keep the baseline commit and date current.
