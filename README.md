# Darkhold Exchange
High-frequency secondary marketplace with an in-memory limit order book.

## Quick start
    just setup && just doctor
    just db-up
    just api      # terminal 1
    just web      # terminal 2

## Docs
- [Development plan](docs/PLAN.md) — build order, sprints, protocol v1, risks
- [Decision log](docs/DECISIONS.md) — locked technical decisions and open questions
- [Source spec](docs/spec/README.md) — PDF, architecture diagram, class diagram

## Layout
crates/{types,engine,ledger,core,solvency,simulator,gateway} · web/ · contracts/ · bench/ · docs/

## Wire contract
Message shapes are pinned by `contracts/*.json` and the golden msgpack frames in `contracts/msgpack/`.
Rust (`crates/types/tests`) and the web app (`web/src/protocol`) both test against these files.
When you add or change a message, update the fixture, the Rust type, and the JS codec in the same PR.

`dx-types` is also the source of truth for the frontend declarations. Add `TS` to a wire type's derives
and `#[ts(export)]` to export it, then regenerate with `just types` (or
`cargo test -p dx-types export_bindings`). Check the checked-in JS with
`npm --prefix web run typecheck`. Generated files live in `web/src/protocol/generated/`; do not edit
them by hand.
