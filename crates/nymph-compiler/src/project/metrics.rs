use std::{
	cell::RefCell,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

#[derive(Clone, Copy)]
pub(crate) enum CompilerPhase {
	Parse,
	Graph,
	Rewrite,
	Check,
	Lower,
	Emit,
	Bundle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg(feature = "test-support")]
pub struct PhaseCounts {
	pub parse: u64,
	pub graph: u64,
	pub rewrite: u64,
	pub check: u64,
	pub lower: u64,
	pub emit: u64,
	pub bundle: u64,
}

#[derive(Default)]
pub(super) struct PhaseCountsAtomic {
	parse: AtomicU64,
	graph: AtomicU64,
	rewrite: AtomicU64,
	check: AtomicU64,
	lower: AtomicU64,
	emit: AtomicU64,
	bundle: AtomicU64,
}

thread_local! {
	static COLLECTOR: RefCell<Option<Arc<PhaseCountsAtomic>>> = const { RefCell::new(None) };
}

pub(crate) fn record_phase(phase: CompilerPhase) {
	COLLECTOR.with_borrow(|collector| {
		let Some(counts) = collector else { return };
		let counter = match phase {
			CompilerPhase::Parse => &counts.parse,
			CompilerPhase::Graph => &counts.graph,
			CompilerPhase::Rewrite => &counts.rewrite,
			CompilerPhase::Check => &counts.check,
			CompilerPhase::Lower => &counts.lower,
			CompilerPhase::Emit => &counts.emit,
			CompilerPhase::Bundle => &counts.bundle,
		};
		counter.fetch_add(1, Ordering::Relaxed);
	});
}

pub(crate) fn capture_collector() -> Option<Arc<PhaseCountsAtomic>> {
	COLLECTOR.with_borrow(Clone::clone)
}

pub(crate) fn install_collector<R>(
	collector: Option<Arc<PhaseCountsAtomic>>,
	f: impl FnOnce() -> R,
) -> R {
	COLLECTOR.with(|slot| {
		struct Restore<'a> {
			slot: &'a RefCell<Option<Arc<PhaseCountsAtomic>>>,
			previous: Option<Arc<PhaseCountsAtomic>>,
		}
		impl Drop for Restore<'_> {
			fn drop(&mut self) {
				self.slot.replace(self.previous.take());
			}
		}
		let _restore = Restore {
			slot,
			previous: slot.replace(collector),
		};
		f()
	})
}

#[cfg(feature = "test-support")]
pub fn with_phase_counts<R>(f: impl FnOnce() -> R) -> (R, PhaseCounts) {
	let counts = Arc::new(PhaseCountsAtomic::default());
	let result = install_collector(Some(Arc::clone(&counts)), f);
	let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
	(
		result,
		PhaseCounts {
			parse: load(&counts.parse),
			graph: load(&counts.graph),
			rewrite: load(&counts.rewrite),
			check: load(&counts.check),
			lower: load(&counts.lower),
			emit: load(&counts.emit),
			bundle: load(&counts.bundle),
		},
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn disabled_recording_is_a_no_op() {
		record_phase(CompilerPhase::Parse);
		assert!(capture_collector().is_none());
	}

	#[test]
	fn collector_is_restored_after_unwind() {
		let outer = Arc::new(PhaseCountsAtomic::default());
		install_collector(Some(Arc::clone(&outer)), || {
			let _ = std::panic::catch_unwind(|| {
				install_collector(Some(Arc::new(PhaseCountsAtomic::default())), || {
					panic!("stop")
				});
			});
			assert!(Arc::ptr_eq(
				capture_collector()
					.as_ref()
					.expect("outer collector restored"),
				&outer,
			));
		});
	}
}
