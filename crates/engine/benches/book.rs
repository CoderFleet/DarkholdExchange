use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dx_engine::Engine;
use dx_types::{Side, seed_markets};

const MARKET: u32 = 1;

fn empty_engine() -> Engine {
    Engine::new(seed_markets()).unwrap()
}

fn engine_with_asks(n: usize) -> Engine {
    let mut engine = empty_engine();
    for _ in 0..n {
        engine.place_limit(2, MARKET, Side::Sell, 5_000, 1).unwrap();
    }
    engine
}

fn bench_add_limit(c: &mut Criterion) {
    c.bench_function("add_limit", |b| {
        b.iter_batched(
            empty_engine,
            |mut engine| {
                black_box(engine.place_limit(1, MARKET, Side::Buy, 5_000, 1).unwrap());
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_match_one_against_100(c: &mut Criterion) {
    c.bench_function("match_one_against_100_asks", |b| {
        b.iter_batched(
            || engine_with_asks(100),
            |mut engine| {
                black_box(engine.place_limit(1, MARKET, Side::Buy, 5_000, 1).unwrap());
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_cancel(c: &mut Criterion) {
    c.bench_function("cancel", |b| {
        b.iter_batched(
            || {
                let mut engine = empty_engine();
                let outcome = engine.place_limit(1, MARKET, Side::Buy, 5_000, 1).unwrap();
                (engine, outcome.order_id)
            },
            |(mut engine, order_id)| {
                black_box(engine.cancel(1, order_id).unwrap());
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    benches,
    bench_add_limit,
    bench_match_one_against_100,
    bench_cancel
);
criterion_main!(benches);
