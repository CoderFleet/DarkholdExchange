# Darkhold Exchange
High-frequency secondary marketplace with an in-memory limit order book.

## Quick start
    just setup && just doctor
    just db-up
    just api      # terminal 1
    just web      # terminal 2
    just load     # repeatable gateway load baseline
    just load-fills  # repeatable fill-heavy gateway baseline

## Persistence

The gateway is crash-safe by default. Every non-cached command, successful deposit/issuance,
runtime market creation, seller-key registration, escrow release/refund, and Dutch-auction
create/accept/expire is appended to a hash-chained write-ahead log in group-committed batches,
then fsynced before the caller gets a reply. A full post-state checkpoint is written at least every
256 operations. Startup replays the log into the seeded demo state and aborts if any stored
checkpoint does not match.

- `DX_WAL_DIR` sets the base directory; records land in `<dir>/data/log` (default `./data/log`).
- `DX_WAL=0` disables the WAL for in-memory benchmarks.
- `GET /api/state/hash` returns the current core state hash.
- Full-log replay is wired; periodic snapshot compaction is still a follow-up.
- State checkpoints are periodic, not per command; the record hash chain authenticates every record.
- The demo seed is the replay baseline and is not itself logged. Clear `data/log` after changing
  `seed_demo` in development.

## Docs
- [T1 protocol contract](docs/PROTOCOL.md) — frozen REST/WS shapes and delta rules
- [Development plan](docs/PLAN.md) — build order, sprints, protocol v1, risks
- [Decision log](docs/DECISIONS.md) — locked technical decisions and open questions
- [Source spec](docs/spec/README.md) — PDF, architecture diagram, class diagram
- [Spec checklist](docs/SPEC_CHECKLIST.md) — current implementation status vs. the Webster spec
- [Load baselines](bench/results/README.md) — repeatable gateway benchmark and recorded runs

## Layout
crates/{types,engine,ledger,core,solvency,simulator,gateway} · web/ · contracts/ · bench/ · docs/

## Wire contract
Message shapes are pinned by `contracts/*.json` and the golden msgpack frames in `contracts/msgpack/`.
Rust (`crates/types/tests`) and the web app (`web/src/protocol`) both test against these files.
When you add or change a message, update the fixture, the Rust type, and the JS codec in the same PR.
Run `just fixtures` after changing a wire type to regenerate the golden bytes, and `just types` to
regenerate the frontend declarations.

`dx-types` is also the source of truth for the frontend declarations. Add `TS` to a wire type's derives
and `#[ts(export)]` to export it, then regenerate with `just types` (or
`cargo test -p dx-types export_bindings`). Check the checked-in JS with
`npm --prefix web run typecheck`. Generated files live in `web/src/protocol/generated/`; do not edit
them by hand.
