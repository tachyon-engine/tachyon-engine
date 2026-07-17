//! Compares retained Eden backing reuse with the former immediate-release policy.

use std::{hint::black_box, time::Instant};

use tachyon_gc::{AllocationSpace, Heap, HeapLimit, SPAN_SIZE_BYTES, Trace, Tracer, TypeRegistry};
use tachyon_value::Value;

const SAMPLES: usize = 9;
const ITERATIONS_PER_SAMPLE: usize = 4_096;
const WARMUP_ITERATIONS: usize = 512;

struct EmptyRoots;

impl Trace for EmptyRoots {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

/// Runs one allocation/minor cycle per iteration and optionally restores immediate release.
fn run_workload(iterations: usize, trim_each_cycle: bool) -> u128 {
    let mut types = TypeRegistry::new();
    let value_type = types.try_register::<Value>("Value").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let mut roots = EmptyRoots;
    let start = Instant::now();
    for value in 0..iterations {
        heap.try_allocate(
            value_type,
            0,
            0,
            Value::from_i32(value as i32),
            AllocationSpace::Young,
        )
        .unwrap();
        black_box(heap.collect_minor(&mut roots).unwrap());
        if trim_each_cycle {
            black_box(heap.trim_eden_pool_storage().unwrap());
        }
    }
    let elapsed = start.elapsed().as_nanos();
    black_box(heap.eden_pool_stats());
    elapsed
}

/// Collects interleaved samples and returns stable medians instead of a single timing observation.
fn measure() -> (u128, u128) {
    black_box(run_workload(WARMUP_ITERATIONS, false));
    black_box(run_workload(WARMUP_ITERATIONS, true));
    let mut pooled = Vec::with_capacity(SAMPLES);
    let mut immediate = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        if sample.is_multiple_of(2) {
            pooled.push(run_workload(ITERATIONS_PER_SAMPLE, false));
            immediate.push(run_workload(ITERATIONS_PER_SAMPLE, true));
        } else {
            immediate.push(run_workload(ITERATIONS_PER_SAMPLE, true));
            pooled.push(run_workload(ITERATIONS_PER_SAMPLE, false));
        }
    }
    pooled.sort_unstable();
    immediate.sort_unstable();
    (pooled[SAMPLES / 2], immediate[SAMPLES / 2])
}

fn main() {
    let (pooled, immediate) = measure();
    let pooled_per_op = pooled as f64 / ITERATIONS_PER_SAMPLE as f64;
    let immediate_per_op = immediate as f64 / ITERATIONS_PER_SAMPLE as f64;
    println!("eden_pool pooled median: {pooled_per_op:.1} ns/op");
    println!("eden_pool immediate-release median: {immediate_per_op:.1} ns/op");
    println!(
        "eden_pool immediate/pooled ratio: {:.3}x",
        immediate_per_op / pooled_per_op
    );
}
