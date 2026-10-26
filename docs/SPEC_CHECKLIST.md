# Darkhold Exchange — Spec Checklist

> **Source of truth:** [`docs/spec/darkhold-spec.pdf`](spec/darkhold-spec.pdf)
>
> **Baseline audited:** `main` @ `aab8997` on 2026-10-27
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
| Dutch-auction liquidation engine | ❌ | No auction types, state, command, settlement, gateway route, or UI. |
| Frequent batch auctions with commit-reveal | ❌ | Not implemented. |
| Escrow with signed delivery receipts and timeout refunds | ❌ | Ledger explicitly rejects escrow settlement; no ed25519 receipt flow. |
| Merkle-sum proof of solvency | ✅ | Real per-asset Merkle-sum trees, inclusion proofs, `/api/solvency`, `/api/me/solvency`, and `/verify`. Live check showed 4/4 proofs valid. |
| Hash-chained public audit log + browser verification | 🟡 | Hash primitives and browser verifier exist and are tested, but no audit records are produced or exposed, and there is no audit UI. |
| Per-identity/per-IP rate limits + adaptive proof-of-work | ❌ | Only login throttling exists. Command admission has no token buckets or PoW. |
| Bot/scalper simulator + bots-vs-humans switch | 🟡 | `dx-simulator` / `dx-load` can generate load, but there is no attack/defence UI switch. |
| Order-book replay/scrubber | 🟡 | Core replay is covered by a test; there is no replay API or UI scrubber. |
| Live metrics/latency panel | ✅ | Gateway histograms + `/api/metrics/latency` + `/speed` panel are merged. |
| PostgreSQL read models | 🟡 | `dkx-projection` and migrations exist; the gateway does not use them yet. |
| WAL + crash recovery | 🟡 | `dkx-wal` has records, snapshots, recovery, and tests; it is not wired into the gateway. |
| Nginx/Caddy + Docker deployment | ❌ | `docker-compose.yml` only starts PostgreSQL. There is no app container, proxy, or one-command deployment. |
| 1,000+ orders/s, µs engine p99, <10 ms e2e p99 | ✅ | Measured with the Rust load harness and recorded under `bench/results/`. Not enforced in CI. |
| Differential testing vs reference matcher | ❌ | Not implemented. |
| Deterministic replay from the log | 🟡 | In-memory core replay test exists; no wired WAL replay through the gateway. |
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
| 3 | Per-identity holding limits | ✅ | Core tracks open buys and held passes. | — |
| 4 | Double-entry ledger, reservations before matching | ✅ | `crates/ledger`, atomic settlement, conservation and negative-balance tests. | — |
| 5 | Idempotency keys and replay-safe retries | ✅ | Core idempotency cache + tests. | — |
| 6 | JWT auth + roles | 🟡 | Real login/signup/JWT and admin/issuer roles. Users are in memory. | Check WS handshake auth; decide whether durable users are in scope. |
| 7 | WebSocket auth | ❌ | WS accepts `?user_id=` and does not verify a JWT. | Authenticate the upgrade request; reject spoofed identities. |
| 8 | L2 snapshot + sequenced diffs + gap resync | ✅ | Gateway publisher, worker rebuild, market terminal. | — |
| 9 | 20–50 ms batching and slow-consumer protection | ✅ | WS batching, bounded buffers, slow-consumer metrics. | — |
| 10 | Dutch auction | ❌ | Nothing in `crates/types`, core, ledger, gateway, or web. | Implement linear decay, `Accept`, CAS quantity, settlement, expiry/refund, UI. |
| 11 | Batch auction + commit-reveal | ❌ | Nothing implemented. | Defer until P1 items are complete, unless the submission needs it. |
| 12 | Escrow state machine | ❌ | Ledger returns `escrow_not_ready`; demo markets have `escrow_enabled: false`. | Implement `Filled → PendingDelivery → Delivered/TimedOut → Refunded` if claimed. |
| 13 | ed25519 signed delivery receipts | ❌ | No receipt types, key registry, or signature verification. | Add with escrow. |
| 14 | Merkle-sum solvency | ✅ | `crates/solvency`, `crates/hash/src/merkle.rs`, gateway routes, `/verify`, shared golden vectors. | Fix the snapshot `seq` caveat below. |
| 15 | Hash-chained audit log | 🟡 | `crates/hash/src/audit.rs` + `web/src/lib/hash.ts` + golden vectors. No records/endpoint/UI. | Record sequenced commands, expose `GET /api/audit`, verify the chain on `/verify`. |
| 16 | Per-user/per-IP rate limits | ❌ | Only login has a throttle (`crates/auth`). | Add token buckets at command admission; return 429 + `Retry-After`. |
| 17 | Adaptive proof-of-work | ❌ | No challenge endpoint or client solver. | Add challenge/verify flow and browser Web Worker solver. |
| 18 | Bot/scalper simulator + defence switch | 🟡 | `dx-simulator`, `dx-load`, `scripts/load-test.sh`, recorded results. | Add attack/defence scenarios and a UI toggle if the demo needs it. |
| 19 | Order-book replay/scrubber | 🟡 | Core replay test; no live replay API. | Add snapshot + delta replay endpoint and a UI time scrubber. |
| 20 | Live metrics/latency panel | ✅ | `/api/metrics/latency`, gateway histograms, `/speed`, `latency.*`, `SpeedPanel.jsx`. | Populate it by placing orders during the demo. |
| 21 | PostgreSQL read models | 🟡 | `crates/dkx-projection`, migrations, SQL queries. Not wired into gateway. | Start the projection worker behind a flag; add integration test. |
| 22 | WAL + crash recovery | 🟡 | `crates/dkx-wal` records, snapshots, recovery tests. Not wired. | Wire writer/recovery into gateway; verify restart → same state hash. |
| 23 | Docker/Caddy/nginx deployment | ❌ | Compose only includes `db`. | Add app image, proxy config, health checks, one-command demo. |
| 24 | Performance targets | ✅ | `bench/results/` documents measured load. | Add CI thresholds or clearly document the substitute. |
| 25 | Differential matcher test | ❌ | No reference matcher. | Build a deliberately simple sorted-Vec matcher and compare streams. |
| 26 | Deterministic WAL replay | 🟡 | Core replay test; WAL recovery tests. | Prove gateway restart produces the same state hash. |
| 27 | k6/Locust load thresholds | ❌ | Empty `bench/k6` and `bench/locust`; Rust harness used instead. | Add k6 or document a justified equivalent. |
| 28 | Playwright E2E | ❌ | Package installed; no config/tests. | Add smoke flows: login, place order, fill, verify, speed. |
| 29 | fast-check frontend tests | ❌ | Package installed; no usage in `web/src`. | Add property tests for book rebuild/resync or remove the dependency. |
| 30 | TypeScript frontend | 🚫 | JavaScript decision recorded in `docs/DECISIONS.md`. | Keep the decision documented; do not half-migrate. |
| 31 | Comlink worker RPC | 🚫 | Package installed; worker uses raw `postMessage`. | Either adopt it or remove the unused dependency. |
| 32 | Admin publishing reaches the engine | ❌ | Gateway uses `AdminState::without_engine()`; publish locks the tier but creates no market. | Add runtime market creation and wire the publisher. |
| 33 | Issuer console | ❌ | `web/src/pages/IssuerPage.jsx` says “Coming soon.” | Add role-checked pass issuance + supply view. |

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
| **T1 — Rudransh** | Differential reference matcher; Playwright place/match/cancel test; market-setup command integration; escrow switch in the engine; replay/state-fingerprint helpers; WS endpoint auth (with T3). |
| **T2 — Shraddha** | Audit-chain records + `GET /api/audit` + browser verification; escrow state machine/receipts/disputes; WAL wiring + crash recovery; PostgreSQL projection; replay service/page; private user WS; solvency `seq` caveat. |
| **T3 — Stuti** | Admin publishing reaching the engine; Issuer console; per-identity/per-IP rate limits + adaptive PoW; Dutch auction; batch auction + commit-reveal; timer service; bot simulator UI; Docker/Caddy/nginx; k6/Locust thresholds; Playwright setup; WS auth (with T1). |

The trust work landed in **T2 (Shraddha)** territory: Merkle-sum solvency and the `/verify` page. The speed panel and market charts are **T1 (Rudransh)** work.

## Known caveats / quick fixes

- **Solvency snapshot `seq`:** it currently uses the core command sequence, but dev deposits/top-ups
  bypass `next_seq`. Replace it with a real state version when the WAL/state version is wired.
- **Admin publishing is a no-op:** `AdminState::without_engine()` means the admin UI can create and
  lock tiers but the engine never receives a new market.
- **Issuer page is a placeholder.**
- **WebSocket identity is unauthenticated:** `WsQuery.user_id` is accepted from the URL.
- **`web/full-smoke-run.mjs` is not part of the repo:** it has a hard-coded temp path and imports
  `playwright` instead of the declared `@playwright/test`. Keep it untracked or rewrite it as a real
  Playwright test.
- **Earlier audit note is stale:** the Merkle-sum solvency proof is implemented and live; the speed
  panel is also implemented and merged. Do not revert those rows to “missing/WIP.”

## Recommended work queue

1. **Trust finish (T2 — Shraddha):** audit-chain records + `GET /api/audit` + browser verification section.
2. **Escrow (T2 — Shraddha, with T1 switch + T3 timer):** state machine, ed25519 receipts, timeout refunds.
3. **Dutch auction (T3 — Stuti, with T1 engine hooks):** end-to-end vertical slice.
4. **Batch auction (T3 — Stuti, with T1 engine hooks):** commit-reveal and uniform clearing.
5. **Fairness (T3 — Stuti):** per-identity/per-IP rate limits + adaptive PoW.
6. **Verification (T1/T2/T3):** differential matcher, deterministic WAL replay, Playwright E2E, k6 or documented substitute, fast-check usage.
7. **Operations (T2 + T3):** wire WAL, wire PostgreSQL projection, Docker/Caddy deployment.
8. **Admin/issuer (T3 — Stuti, with T1 market setup):** make publishing create a live market; implement the issuer console.

## Submission honesty

The PDF abstract names a Dutch-auction engine, escrow, and a cryptographic proof of solvency. The
solvency proof is now real. Dutch auction, batch auction, and escrow are still missing. Before the
deadline, either implement them or explicitly mark them as **not delivered** in the final README and
demo script. Do not let the abstract imply that a running feature exists when it does not.

## Verification commands

```sh
cargo test --workspace
npm --prefix web test
npm --prefix web run typecheck
npm --prefix web run lint
npm --prefix web run build
```

Update this file whenever a row changes. Keep the baseline commit and date current.
