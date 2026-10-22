set dotenv-load := true

default:
    @just --list

# one-time setup after cloning
setup:
    cargo install --locked cargo-nextest
    npm --prefix web ci

# verify your tool versions match the pinned ones
doctor:
    bash scripts/doctor.sh

db-up:
    docker compose up -d db

db-down:
    docker compose down

api:
    cargo run -p dx-gateway

web:
    npm --prefix web run dev

# regenerate the TypeScript declarations from the Rust wire types
types:
    cargo test -p dx-types export_bindings

check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    npm --prefix web run lint
    npm --prefix web run typecheck

test:
    cargo nextest run --workspace
    npm --prefix web test

bench:
    cargo bench -p dx-engine
