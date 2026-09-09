//! Copy-pipeline benchmarks (spec §3 `benches/`, and the `flux benchmark`
//! command in spec §4).
//!
//! Placeholder so `cargo bench` is wired from day one. Replace with real
//! pipeline benchmarks once the engine exists.

use criterion::{Criterion, criterion_group, criterion_main};

fn placeholder(c: &mut Criterion) {
    c.bench_function("placeholder", |b| b.iter(|| std::hint::black_box(0u64)));
}

criterion_group!(benches, placeholder);
criterion_main!(benches);
