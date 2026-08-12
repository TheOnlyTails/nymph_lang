//! Opt-in counters used by the issue #81 acceptance executable.
//!
//! This module is compiled only with `test-support`; ordinary compiler builds
//! contain neither the branches nor the atomics below.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

const PHASE_COUNT: usize = 7;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Phase {
	Parse = 0,
	Environment = 1,
	Checker = 2,
	DiagnosticReporting = 3,
	StableLowering = 4,
	Emission = 5,
	Bundling = 6,
}

const NAMES: [&str; PHASE_COUNT] = [
	"parse",
	"interface_environment",
	"checker",
	"diagnostic_fold_wrapper",
	"stable_lowering",
	"emission",
	"bundling",
];

static ENABLED: AtomicBool = AtomicBool::new(false);
static NANOS: [AtomicU64; PHASE_COUNT] = [const { AtomicU64::new(0) }; PHASE_COUNT];
static COUNTS: [AtomicU64; PHASE_COUNT] = [const { AtomicU64::new(0) }; PHASE_COUNT];
static PREWARM_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static PREWARM_MAX_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static PREWARM_WORKERS: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct PhaseGuard(Option<(Phase, Instant)>);

impl Drop for PhaseGuard {
	fn drop(&mut self) {
		let Some((phase, started)) = self.0 else {
			return;
		};
		NANOS[phase as usize].fetch_add(
			started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
			Ordering::Relaxed,
		);
		COUNTS[phase as usize].fetch_add(1, Ordering::Relaxed);
	}
}

#[must_use]
pub(crate) fn phase(phase: Phase) -> PhaseGuard {
	PhaseGuard(
		ENABLED
			.load(Ordering::Relaxed)
			.then(|| (phase, Instant::now())),
	)
}

pub(crate) struct PrewarmGuard(bool);

impl Drop for PrewarmGuard {
	fn drop(&mut self) {
		if self.0 {
			PREWARM_ACTIVE.fetch_sub(1, Ordering::Relaxed);
		}
	}
}

#[must_use]
pub(crate) fn prewarm_task() -> PrewarmGuard {
	if !ENABLED.load(Ordering::Relaxed) {
		return PrewarmGuard(false);
	}
	let active = PREWARM_ACTIVE.fetch_add(1, Ordering::Relaxed) + 1;
	PREWARM_MAX_ACTIVE.fetch_max(active, Ordering::Relaxed);
	PrewarmGuard(true)
}

pub(crate) fn record_prewarm_workers(workers: usize) {
	if ENABLED.load(Ordering::Relaxed) {
		PREWARM_WORKERS.store(workers, Ordering::Relaxed);
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkPhaseTiming {
	pub name: &'static str,
	pub inclusive_nanos: u64,
	pub executions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkProfile {
	pub phases: Vec<BenchmarkPhaseTiming>,
	pub prewarm_configured_workers: usize,
	pub prewarm_max_active: usize,
}

/// Reset and enable counters. Call only when no compiler request is active.
pub fn begin_benchmark_profile() {
	for value in &NANOS {
		value.store(0, Ordering::Relaxed);
	}
	for value in &COUNTS {
		value.store(0, Ordering::Relaxed);
	}
	PREWARM_ACTIVE.store(0, Ordering::Relaxed);
	PREWARM_MAX_ACTIVE.store(0, Ordering::Relaxed);
	PREWARM_WORKERS.store(0, Ordering::Relaxed);
	ENABLED.store(true, Ordering::Release);
}

/// Disable and return one benchmark request's inclusive counters.
#[must_use]
pub fn finish_benchmark_profile() -> BenchmarkProfile {
	ENABLED.store(false, Ordering::Release);
	assert_eq!(PREWARM_ACTIVE.load(Ordering::Relaxed), 0);
	BenchmarkProfile {
		phases: NAMES
			.iter()
			.enumerate()
			.map(|(index, name)| BenchmarkPhaseTiming {
				name,
				inclusive_nanos: NANOS[index].load(Ordering::Relaxed),
				executions: COUNTS[index].load(Ordering::Relaxed),
			})
			.collect(),
		prewarm_configured_workers: PREWARM_WORKERS.load(Ordering::Relaxed),
		prewarm_max_active: PREWARM_MAX_ACTIVE.load(Ordering::Relaxed),
	}
}
