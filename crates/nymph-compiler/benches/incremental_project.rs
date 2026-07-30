use std::{
	alloc::{GlobalAlloc, Layout, System},
	hint::black_box,
	sync::atomic::{AtomicUsize, Ordering},
};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nymph_compiler::{
	check_project_library, compile_project_library,
	project::{GraphFixture, GraphShape, PhaseCounts, with_phase_counts},
};

struct CountingAllocator;
static RETAINED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add_allocation(size: usize) {
	let retained = RETAINED.fetch_add(size, Ordering::Relaxed) + size;
	PEAK.fetch_max(retained, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CountingAllocator {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let pointer = unsafe { System.alloc(layout) };
		if !pointer.is_null() {
			add_allocation(layout.size());
		}
		pointer
	}
	unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
		RETAINED.fetch_sub(layout.size(), Ordering::Relaxed);
		unsafe { System.dealloc(pointer, layout) }
	}
	unsafe fn realloc(&self, pointer: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
		let replacement = unsafe { System.realloc(pointer, old, new_size) };
		if !replacement.is_null() {
			if new_size >= old.size() {
				add_allocation(new_size - old.size());
			} else {
				RETAINED.fetch_sub(old.size() - new_size, Ordering::Relaxed);
			}
		}
		replacement
	}
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn check(fixture: &GraphFixture) -> usize {
	let diags = check_project_library(fixture.entry(), &|key| fixture.load(key));
	assert!(diags.is_empty(), "benchmark fixture failed: {diags:?}");
	0
}

fn compile(fixture: &GraphFixture) -> usize {
	compile_project_library(fixture.entry(), &|key| fixture.load(key))
		.unwrap_or_else(|diags| panic!("benchmark fixture failed: {diags:?}"))
		.js
		.len()
}

fn measured(f: impl FnOnce() -> usize) -> (usize, usize) {
	let baseline = RETAINED.load(Ordering::Relaxed);
	PEAK.store(baseline, Ordering::Relaxed);
	let generated = f();
	let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
	black_box((generated, peak))
}

#[derive(Clone, Copy)]
struct Case {
	name: &'static str,
	operation: fn(&GraphFixture) -> usize,
	setup: fn(GraphShape) -> GraphFixture,
}

fn fresh(shape: GraphShape) -> GraphFixture {
	shape.generate()
}

fn prime_check(shape: GraphShape) -> GraphFixture {
	let fixture = shape.generate();
	check(&fixture);
	fixture
}

fn prime_compile(shape: GraphShape) -> GraphFixture {
	let fixture = shape.generate();
	compile(&fixture);
	fixture
}

// This setup seam deliberately models replacement as prime -> mutate -> compile.
// A later incremental session can be retained here without changing the measured closure.
fn prime_then_replace_private(shape: GraphShape) -> GraphFixture {
	let mut fixture = prime_compile(shape);
	fixture.replace_private_leaf_body();
	fixture
}

fn prime_then_replace_public(shape: GraphShape) -> GraphFixture {
	let mut fixture = prime_compile(shape);
	fixture.replace_public_leaf_signature();
	fixture
}

fn median_peak(case: Case, shape: GraphShape) -> usize {
	const SAMPLES: usize = 21;
	let mut peaks = Vec::with_capacity(SAMPLES);
	for _ in 0..SAMPLES {
		let fixture = (case.setup)(shape);
		let (_, peak) = measured(|| (case.operation)(&fixture));
		peaks.push(peak);
	}
	peaks.sort_unstable();
	peaks[SAMPLES / 2]
}

fn print_baseline(label: &str, case: Case, shape: GraphShape) {
	let fixture = (case.setup)(shape);
	let (generated, counts): (usize, PhaseCounts) = with_phase_counts(|| (case.operation)(&fixture));
	let median_peak = median_peak(case, shape);
	eprintln!(
		"BASELINE {label}: generated_bytes={generated} retained_source_bytes={} median_peak_retained_bytes={median_peak} peak_samples=21 counts={counts:?}",
		fixture.retained_bytes()
	);
}

fn throughput(case: Case, shape: GraphShape) -> Throughput {
	if case.name.contains("compile") || case.name.contains("leaf") {
		let fixture = (case.setup)(shape);
		Throughput::Bytes((case.operation)(&fixture) as u64)
	} else {
		Throughput::Elements(1)
	}
}

fn incremental_project(c: &mut Criterion) {
	for (shape_name, shape, cases) in [
		(
			"single",
			GraphShape::Single,
			&[
				Case {
					name: "fresh-check",
					operation: check,
					setup: fresh,
				},
				Case {
					name: "fresh-compile",
					operation: compile,
					setup: fresh,
				},
			][..],
		),
		("wide-16", GraphShape::Wide { leaves: 16 }, &CASES[..]),
		("deep-16", GraphShape::Deep { depth: 16 }, &CASES[..]),
		(
			"mixed-4x4",
			GraphShape::Mixed { width: 4, depth: 4 },
			&CASES[..],
		),
	] {
		let mut group = c.benchmark_group(shape_name);
		for &case in cases {
			print_baseline(&format!("{shape_name}/{}", case.name), case, shape);
			group.throughput(throughput(case, shape));
			group.bench_with_input(
				BenchmarkId::new(case.name, shape_name),
				&shape,
				|b, shape| {
					b.iter_batched(
						|| (case.setup)(*shape),
						|fixture| (case.operation)(black_box(&fixture)),
						BatchSize::SmallInput,
					);
				},
			);
		}
		group.finish();
	}
}

const CASES: [Case; 6] = [
	Case {
		name: "fresh-check",
		operation: check,
		setup: fresh,
	},
	Case {
		name: "fresh-compile",
		operation: compile,
		setup: fresh,
	},
	Case {
		name: "unchanged-check",
		operation: check,
		setup: prime_check,
	},
	Case {
		name: "unchanged-compile",
		operation: compile,
		setup: prime_compile,
	},
	Case {
		name: "private-leaf-body",
		operation: compile,
		setup: prime_then_replace_private,
	},
	Case {
		name: "public-leaf-signature",
		operation: compile,
		setup: prime_then_replace_public,
	},
];

criterion_group!(benches, incremental_project);
criterion_main!(benches);
