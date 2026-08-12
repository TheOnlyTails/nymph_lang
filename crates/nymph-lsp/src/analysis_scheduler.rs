//! Bounded execution for compiler-backed LSP work.
//!
//! The protocol loop never lends its mutable state to a worker. Every job
//! carries an immutable [`DocumentStore`] snapshot, while each fixed worker
//! owns an independent [`CompilerState`] and reconciles that private session
//! to the requested revision. In particular, this does not use Salsa database
//! clones: no memo storage is shared between an owner mutation and analysis.

use std::{
	panic::{AssertUnwindSafe, catch_unwind},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	thread::JoinHandle,
};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use lsp_types::PublishDiagnosticsParams;

use crate::{compiler_state::CompilerState, document_store::DocumentStore};

pub(crate) const WORKER_COUNT: usize = 2;
pub(crate) const WORK_QUEUE_CAPACITY: usize = 16;
pub(crate) const OWNER_PENDING_CAPACITY: usize = 64;
pub(crate) const OUTSTANDING_WORK_CAPACITY: usize =
	WORKER_COUNT + WORK_QUEUE_CAPACITY + OWNER_PENDING_CAPACITY;

#[derive(Clone, Default)]
pub(crate) struct CancellationToken {
	cancelled: Arc<AtomicBool>,
	#[cfg(test)]
	cancel_after: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl CancellationToken {
	#[cfg(test)]
	pub(crate) fn cancel_after(checkpoints: usize) -> Self {
		Self {
			cancelled: Arc::new(AtomicBool::new(false)),
			cancel_after: Some(Arc::new(std::sync::atomic::AtomicUsize::new(checkpoints))),
		}
	}

	pub(crate) fn cancel(&self) {
		self.cancelled.store(true, Ordering::Release);
	}

	pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.cancelled, &other.cancelled)
	}

	pub(crate) fn is_cancelled(&self) -> bool {
		self.cancelled.load(Ordering::Acquire)
	}

	pub(crate) fn checkpoint(&self) -> Result<(), Cancelled> {
		#[cfg(test)]
		if let Some(remaining) = &self.cancel_after
			&& remaining
				.try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
					value.checked_sub(1)
				})
				.is_err()
		{
			self.cancel();
		}
		if self.is_cancelled() {
			Err(Cancelled)
		} else {
			Ok(())
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cancelled;

#[derive(Debug)]
pub(crate) enum TaskError {
	Cancelled,
	ContentModified(String),
	InvalidParams(String),
	Internal(String),
}

impl From<Cancelled> for TaskError {
	fn from(_: Cancelled) -> Self {
		Self::Cancelled
	}
}

pub(crate) enum TaskResult {
	Request(Result<serde_json::Value, TaskError>),
	Diagnostics(Result<Vec<PublishDiagnosticsParams>, TaskError>),
}

impl TaskResult {
	fn is_error(&self) -> bool {
		match self {
			Self::Request(result) => result.is_err(),
			Self::Diagnostics(result) => result.is_err(),
		}
	}
}

pub(crate) type Task =
	Box<dyn FnOnce(&mut WorkerState, &CancellationToken) -> TaskResult + Send + 'static>;

pub(crate) struct WorkItem {
	pub id: u64,
	/// Stable project/document affinity. All work for one analysis ownership
	/// domain is routed to the same private compiler session.
	pub route: u64,
	pub cancellation: CancellationToken,
	pub task: Task,
}

pub(crate) struct CompletedWork {
	pub id: u64,
	pub result: TaskResult,
}

/// Compiler and open-document mirror owned by exactly one worker thread.
/// Sessions are neither cloned nor shared across workers.
pub(crate) struct WorkerState {
	pub compiler: CompilerState,
	pub documents: DocumentStore,
	owner_snapshot: DocumentStore,
	initialized: bool,
}

impl WorkerState {
	fn new(template: &CompilerState) -> Self {
		Self {
			compiler: template.isolated_empty(),
			documents: DocumentStore::default(),
			owner_snapshot: DocumentStore::default(),
			initialized: false,
		}
	}

	/// Reconcile this worker's private compiler session to an immutable owner
	/// snapshot. Filesystem revisions force deep reconstruction so a worker that
	/// did not execute an earlier watcher job cannot retain stale disk inputs.
	pub(crate) fn synchronize(
		&mut self,
		snapshot: &DocumentStore,
		cancellation: &CancellationToken,
	) -> Result<(), TaskError> {
		let result = self.synchronize_inner(snapshot, cancellation);
		if result.is_err() {
			self.invalidate();
		}
		result
	}

	fn synchronize_inner(
		&mut self,
		snapshot: &DocumentStore,
		cancellation: &CancellationToken,
	) -> Result<(), TaskError> {
		cancellation.checkpoint()?;
		if !self.initialized
			|| self.owner_snapshot.filesystem_revision() != snapshot.filesystem_revision()
			|| self.owner_snapshot.revision() > snapshot.revision()
		{
			self.reconstruct(snapshot, cancellation)?;
			return Ok(());
		}

		let mut removed = self
			.owner_snapshot
			.iter()
			.filter(|(uri, _)| snapshot.get(uri).is_none())
			.map(|(uri, document)| (document.update_revision, uri.clone()))
			.collect::<Vec<_>>();
		removed.sort_by(|left, right| right.cmp(left));
		for (_, uri) in removed {
			cancellation.checkpoint()?;
			self
				.compiler
				.close(&mut self.documents, &uri)
				.map_err(|error| TaskError::Internal(error.to_string()))?;
		}

		for (uri, desired) in snapshot.documents_in_update_order() {
			let previous = self.owner_snapshot.get(uri);
			if previous.is_some_and(|previous| previous == desired) {
				continue;
			}
			cancellation.checkpoint()?;
			let reopened =
				previous.is_some_and(|previous| previous.lifecycle_revision != desired.lifecycle_revision);
			if reopened {
				self
					.compiler
					.close(&mut self.documents, uri)
					.map_err(|error| TaskError::Internal(error.to_string()))?;
			}
			if self.documents.get(uri).is_some() {
				self
					.compiler
					.change(
						&mut self.documents,
						uri,
						desired.text.to_string(),
						desired.version,
					)
					.map_err(|error| TaskError::Internal(error.to_string()))?;
			} else {
				self
					.compiler
					.open(
						&mut self.documents,
						uri.clone(),
						desired.text.to_string(),
						desired.version,
					)
					.map_err(|error| TaskError::Internal(error.to_string()))?;
			}
		}
		cancellation.checkpoint()?;
		self.owner_snapshot = snapshot.clone();
		Ok(())
	}

	/// Record that a task itself applied the exact protocol transition from the
	/// previous snapshot to `snapshot` (currently used for close, whose
	/// diagnostic affected-set depends on pre-close compiler graph state).
	pub(crate) fn adopt_snapshot(&mut self, snapshot: &DocumentStore) {
		self.owner_snapshot = snapshot.clone();
	}

	fn invalidate(&mut self) {
		self.initialized = false;
	}

	fn reconstruct(
		&mut self,
		snapshot: &DocumentStore,
		cancellation: &CancellationToken,
	) -> Result<(), TaskError> {
		self.compiler = self.compiler.isolated_empty();
		self.documents = DocumentStore::default();
		for (uri, document) in snapshot.documents_in_update_order() {
			cancellation.checkpoint()?;
			self
				.compiler
				.open(
					&mut self.documents,
					uri.clone(),
					document.text.to_string(),
					document.version,
				)
				.map_err(|error| TaskError::Internal(error.to_string()))?;
		}
		cancellation.checkpoint()?;
		self.owner_snapshot = snapshot.clone();
		self.initialized = true;
		Ok(())
	}
}

pub(crate) struct WorkerPool {
	senders: Vec<Sender<WorkItem>>,
	completed: Receiver<CompletedWork>,
	threads: Vec<JoinHandle<()>>,
}

impl WorkerPool {
	pub(crate) fn new(template: &CompilerState) -> Self {
		Self::with_limits(template, WORKER_COUNT, WORK_QUEUE_CAPACITY)
	}

	#[cfg(test)]
	pub(crate) fn disconnected() -> Self {
		let (sender, receiver) = crossbeam_channel::bounded(1);
		drop(receiver);
		let (completed_sender, completed) = crossbeam_channel::bounded(1);
		drop(completed_sender);
		Self {
			senders: vec![sender],
			completed,
			threads: Vec::new(),
		}
	}

	fn with_limits(template: &CompilerState, worker_count: usize, queue_capacity: usize) -> Self {
		assert!(worker_count > 0);
		assert!(queue_capacity >= worker_count);
		// Physical admission in the protocol owner is capped at the same total,
		// so even cancelled/superseded completions cannot block worker shutdown.
		let (completed_sender, completed) = crossbeam_channel::bounded(OUTSTANDING_WORK_CAPACITY);
		let queue_capacity = queue_capacity.div_ceil(worker_count);
		let mut senders = Vec::with_capacity(worker_count);
		let mut threads = Vec::with_capacity(worker_count);
		for index in 0..worker_count {
			let (sender, receiver) = crossbeam_channel::bounded::<WorkItem>(queue_capacity);
			senders.push(sender);
			let completed_sender = completed_sender.clone();
			let state = WorkerState::new(template);
			threads.push(
				std::thread::Builder::new()
					.name(format!("nymph-lsp-analysis-{index}"))
					.spawn(move || worker_loop(state, &receiver, &completed_sender))
					.expect("failed to spawn bounded LSP analysis worker"),
			);
		}
		drop(completed_sender);
		Self {
			senders,
			completed,
			threads,
		}
	}

	pub(crate) fn completed(&self) -> &Receiver<CompletedWork> {
		&self.completed
	}

	pub(crate) fn try_submit(&self, item: WorkItem) -> Result<(), TrySendError<WorkItem>> {
		let worker = item.route as usize % self.senders.len();
		self.senders[worker].try_send(item)
	}

	/// Close the bounded queue, drain already submitted jobs, and join every
	/// fixed worker. Callers cancel request/diagnostic tokens before this step.
	pub(crate) fn finish(mut self) {
		self.senders.clear();
		for thread in self.threads {
			let _ = thread.join();
		}
	}
}

fn worker_loop(
	mut state: WorkerState,
	receiver: &Receiver<WorkItem>,
	completed: &Sender<CompletedWork>,
) {
	while let Ok(item) = receiver.recv() {
		let id = item.id;
		let result = match catch_unwind(AssertUnwindSafe(|| {
			(item.task)(&mut state, &item.cancellation)
		})) {
			Ok(result) => result,
			Err(_) => {
				state.invalidate();
				TaskResult::Request(Err(TaskError::Internal(
					"analysis worker panicked".to_string(),
				)))
			}
		};
		if result.is_error() {
			state.invalidate();
		}
		if completed.send(CompletedWork { id, result }).is_err() {
			break;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{
		Barrier,
		atomic::{AtomicUsize, Ordering},
	};

	fn request_task(
		body: impl FnOnce(&CancellationToken) -> Result<serde_json::Value, TaskError> + Send + 'static,
	) -> Task {
		Box::new(move |_, cancellation| TaskResult::Request(body(cancellation)))
	}

	#[test]
	fn cancellation_before_and_during_work_is_observed_at_checkpoints() {
		let pool = WorkerPool::with_limits(&CompilerState::new(), 1, 2);
		let before = CancellationToken::default();
		before.cancel();
		pool
			.try_submit(WorkItem {
				id: 1,
				route: 0,
				cancellation: before,
				task: request_task(|cancellation| {
					cancellation.checkpoint()?;
					Ok(serde_json::Value::Null)
				}),
			})
			.unwrap();
		let completed = pool.completed().recv().unwrap();
		assert!(matches!(
			completed.result,
			TaskResult::Request(Err(TaskError::Cancelled))
		));

		let during = CancellationToken::default();
		let worker_token = during.clone();
		let (reached_sender, reached) = crossbeam_channel::bounded(0);
		let (continue_sender, continue_receiver) = crossbeam_channel::bounded(0);
		pool
			.try_submit(WorkItem {
				id: 2,
				route: 0,
				cancellation: worker_token,
				task: request_task(move |cancellation| {
					reached_sender.send(()).unwrap();
					continue_receiver.recv().unwrap();
					cancellation.checkpoint()?;
					Ok(serde_json::Value::Null)
				}),
			})
			.unwrap();
		reached.recv().unwrap();
		during.cancel();
		continue_sender.send(()).unwrap();
		let completed = pool.completed().recv().unwrap();
		assert!(matches!(
			completed.result,
			TaskResult::Request(Err(TaskError::Cancelled))
		));
		pool.finish();
	}

	#[test]
	fn workers_and_queue_are_bounded_while_requests_can_run_concurrently() {
		let pool = WorkerPool::with_limits(&CompilerState::new(), 2, 2);
		let barrier = Arc::new(Barrier::new(3));
		let active = Arc::new(AtomicUsize::new(0));
		let maximum = Arc::new(AtomicUsize::new(0));
		let (started_sender, started) = crossbeam_channel::bounded(2);
		for id in 0..2 {
			let barrier = barrier.clone();
			let active = active.clone();
			let maximum = maximum.clone();
			let started_sender = started_sender.clone();
			pool
				.try_submit(WorkItem {
					id,
					route: id,
					cancellation: CancellationToken::default(),
					task: request_task(move |_| {
						let current = active.fetch_add(1, Ordering::SeqCst) + 1;
						maximum.fetch_max(current, Ordering::SeqCst);
						started_sender.send(()).unwrap();
						barrier.wait();
						active.fetch_sub(1, Ordering::SeqCst);
						Ok(serde_json::Value::Null)
					}),
				})
				.unwrap();
			started.recv().unwrap();
		}
		barrier.wait();
		assert_eq!(maximum.load(Ordering::SeqCst), 2);
		for _ in 0..2 {
			pool.completed().recv().unwrap();
		}
		pool.finish();

		let pool = WorkerPool::with_limits(&CompilerState::new(), 1, 1);
		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		pool
			.try_submit(WorkItem {
				id: 3,
				route: 0,
				cancellation: CancellationToken::default(),
				task: request_task(move |_| {
					started_sender.send(()).unwrap();
					release.recv().unwrap();
					Ok(serde_json::Value::Null)
				}),
			})
			.unwrap();
		started.recv().unwrap();
		pool
			.try_submit(WorkItem {
				id: 4,
				route: 0,
				cancellation: CancellationToken::default(),
				task: request_task(|_| Ok(serde_json::Value::Null)),
			})
			.unwrap();
		let overflow = pool.try_submit(WorkItem {
			id: 5,
			route: 0,
			cancellation: CancellationToken::default(),
			task: request_task(|_| Ok(serde_json::Value::Null)),
		});
		assert!(matches!(overflow, Err(TrySendError::Full(_))));
		release_sender.send(()).unwrap();
		for _ in 0..2 {
			pool.completed().recv().unwrap();
		}
		pool.finish();
	}

	#[test]
	fn shutdown_drains_cancelled_work_and_joins_every_worker() {
		let pool = WorkerPool::with_limits(&CompilerState::new(), 1, 2);
		let running = CancellationToken::default();
		let queued = CancellationToken::default();
		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		let queued_ran = Arc::new(AtomicUsize::new(0));
		pool
			.try_submit(WorkItem {
				id: 1,
				route: 0,
				cancellation: running.clone(),
				task: request_task(move |cancellation| {
					started_sender.send(()).unwrap();
					release.recv().unwrap();
					cancellation.checkpoint()?;
					Ok(serde_json::Value::Null)
				}),
			})
			.unwrap();
		started.recv().unwrap();
		let queued_ran_worker = queued_ran.clone();
		pool
			.try_submit(WorkItem {
				id: 2,
				route: 0,
				cancellation: queued.clone(),
				task: request_task(move |cancellation| {
					queued_ran_worker.fetch_add(1, Ordering::SeqCst);
					cancellation.checkpoint()?;
					Ok(serde_json::Value::Null)
				}),
			})
			.unwrap();
		running.cancel();
		queued.cancel();
		release_sender.send(()).unwrap();
		pool.finish();
		assert_eq!(queued_ran.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn a_failed_state_mutation_forces_deep_reconstruction_before_worker_reuse() {
		let pool = WorkerPool::with_limits(&CompilerState::new(), 1, 2);
		let stray: lsp_types::Uri = "untitled:stray".parse().unwrap();
		pool
			.try_submit(WorkItem {
				id: 1,
				route: 0,
				cancellation: CancellationToken::default(),
				task: Box::new(move |state, _| {
					state.documents.open(stray, "stray".into(), 1);
					TaskResult::Request(Err(TaskError::Internal("controlled worker failure".into())))
				}),
			})
			.unwrap();
		let failed = pool.completed().recv().unwrap();
		assert!(matches!(
			failed.result,
			TaskResult::Request(Err(TaskError::Internal(_)))
		));

		let desired = DocumentStore::default();
		pool
			.try_submit(WorkItem {
				id: 2,
				route: 0,
				cancellation: CancellationToken::default(),
				task: Box::new(move |state, cancellation| {
					let result = state.synchronize(&desired, cancellation).and_then(|()| {
						if state.documents.iter().next().is_none() {
							Ok(serde_json::Value::Null)
						} else {
							Err(TaskError::Internal(
								"partial state survived reconstruction".into(),
							))
						}
					});
					TaskResult::Request(result)
				}),
			})
			.unwrap();
		assert!(matches!(
			pool.completed().recv().unwrap().result,
			TaskResult::Request(Ok(_))
		));
		pool.finish();
	}
}
