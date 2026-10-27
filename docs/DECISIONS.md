# Decision log

Status: **locked for Sprint 0** unless explicitly superseded. Each decision should become an ADR
(`docs/adr/NNNN-*.md`) once the team starts implementing it.

## Locked decisions

| # | Area | Decision | Why | Revisit when |
|---|---|---|---|---|
| 1 | Scope | Build the **full spec in phases**; cut from the bottom if time runs out | The spec is the grading/demo contract; a thin slice is not enough | After the first end-to-end demo |
| 2 | Schedule | Fixed deadline, overtime allowed. Plan uses six working blocks; map them onto the real calendar | Keeps ordering stable even if the date moves | When the exact deadline is known |
| 3 | Frontend | **Stay JavaScript.** No TypeScript migration | Avoid a rewrite and extra build complexity | Only if the UI grows beyond ~15k lines |
| 4 | Frontend safety | Rust types are the source of truth; `ts-rs` generates one declaration per type in `web/src/protocol/generated/`; `// @ts-check` + `tsc --noEmit` in CI; golden msgpack fixtures; one runtime codec | Gets most of the type-safety benefit without authoring `.ts` files | If `checkJs` becomes noisy |
| 5 | Wire format | **WS frames are msgpack; REST control plane defaults to JSON (msgpack accepted); JSON fixtures are the canonical, reviewable schema** | Compact/fast market data, but commands and fixtures stay debuggable | Never for this project |
| 6 | Asset model | **Fungible quantity per market.** No unique seat/serial numbers | Matches qty-based orders, ledger, and escrow; simplest correct model | If the product requires seat assignment |
| 7 | Transport | **REST for commands, WebSocket for market data and private events** | Clean `429`/`Retry-After`, idempotency, easy k6 tests; WS for streaming | If latency demands WS commands |
| 8 | Money | `i64` minor units (paise); `u64` price ticks; `u64` quantity; **no floats** | Exact arithmetic and conservation | Never |
| 9 | IDs | `u64` user/market/order/trade IDs; `client_order_id: u64` for idempotency | Cheap, fixed-size, no UUID parsing on hot path | If clients need UUIDs |
| 10 | Sequence | Global command `seq` for audit; **per-market `book_seq`** for market-data diffs | Avoids false resyncs when other markets interleave | If a single global stream is preferred |
| 11 | Auth | Dev JWT login + simulated verified identity; real KYC/email/password out of scope | Demo needs identity-bound limits, not an auth product | Before any public deployment |
| 12 | Market config | Static seed config for the demo; DB-backed later | Fast to change, deterministic tests | When admin tooling is needed |
| 13 | Fees | Taker fee in basis points; maker fee zero by default; fees to a platform account | Simple, configurable, keeps settlement balanced | If maker rebates are required |
| 14 | Self-trade prevention | **Cancel the resting order**, continue matching the incoming order | Realistic and keeps the taker's intent; no wash trades | If users find it surprising |
| 15 | Database | PostgreSQL only for read models/projections; never on the hot path | Keeps matching latency predictable; satisfies history/statements | Never on hot path |
| 16 | Durability | Custom append-only, hash-chained WAL + periodic snapshots | Spec requires replay and tamper-evident audit | If a proven library fits better |
| 17 | Deployment | Docker Compose locally first; Caddy/Nginx in front; cloud optional | One-command demo and reproducible load tests | When hosting is chosen |
| 18 | Escrow | Opt-in per market. Fills move buyer cash and seller passes into system escrow accounts; a seller ed25519 receipt releases them, or either party can refund after a five-minute deadline | Demonstrates the spec's escrow state machine without making every market slower; per-fill key capture keeps replay deterministic | If production needs per-market timeouts, automatic refunds, or dispute arbitration |

## JavaScript type-safety strategy (no TypeScript migration)

The risk is client/server drift: a field renamed in Rust silently breaks the UI. We mitigate it in
five layers, cheapest first:

1. **One source of truth.** All message shapes live in `dx-types`. No hand-written duplicate schema.
2. **Generated declarations.** `ts-rs` exports one declaration per exported Rust type into
   `web/src/protocol/generated/`. JS files import them with JSDoc:
   `/** @typedef {import('./generated/PlaceLimit').PlaceLimit} PlaceLimit */`.
   `TS_RS_LARGE_INT=number` keeps JavaScript's safe-integer contract; all wire IDs and money values
   must stay within `Number.MAX_SAFE_INTEGER`.
3. **Checked JS, not authored TS.** `jsconfig.json` with `checkJs`, `strict`, `noEmit`; `// @ts-check`
   at the top of protocol/API/workflow files; `npm run typecheck` runs `tsc`. TypeScript is a dev-only
   checker; the only checked-in `.ts` files are the generated declarations.
4. **Golden wire fixtures.** Rust writes `.msgpack` bytes for every message into `contracts/msgpack/`.
   JS tests decode them and compare against the JSON fixture. A field change fails both sides' CI.
5. **One runtime codec.** All encode/decode goes through `web/src/protocol/index.js`, so message
   construction and raw msgpack access stay in one place. Components never parse raw msgpack.

Contract-change rule (already in the README): **fixture + Rust + JS in the same PR.** CI regenerates
the declarations with `just types` and fails on an uncommitted diff.

## Assumptions to confirm

- Six working blocks is the planning baseline; the actual deadline maps onto the cut list.
- Taker-only fee, default 100 bps, configurable per market.
- Self-trade prevention cancels the resting order.
- Simulated deposits/top-ups are allowed and posted through the ledger (system account may be
  negative; user accounts may not).
- Hosting is local Docker for the demo; no real payment/KYC integration.
- The team has three people; ownership is split by layer with one integrator.

## Open questions

1. **Exact deadline date** (needed to map six blocks to calendar weeks).
2. **Team strengths and hours/week** (Rust vs React; who owns engine/ledger/frontend).
3. **Brand/UI references** — is there a visual direction for "Darkhold", or should we design a dark
   exchange theme from scratch?
4. **Fee model** — is taker-only acceptable, or should both sides pay?
5. **Demo environment** — laptop only, or a hosted URL for judges?
6. **Spec version** — is the PDF in `docs/spec/` the final submission?
