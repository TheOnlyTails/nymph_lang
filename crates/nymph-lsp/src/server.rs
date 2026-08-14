use std::{
	collections::{BTreeSet, HashMap, HashSet, VecDeque},
	hash::{DefaultHasher, Hash, Hasher},
	path::PathBuf,
	sync::{Arc, Mutex},
	thread::JoinHandle,
};

use crossbeam_channel::TrySendError;
use lsp_server::{
	Connection, Message, Notification as ServerNotification, Request as ServerRequest, RequestId,
	Response,
};
use lsp_types::{
	CompletionParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
	DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentFormattingParams,
	DocumentRangeFormattingParams, DocumentSymbolParams, GotoDefinitionParams, HoverParams,
	PublishDiagnosticsParams, ReferenceParams, RenameParams, SemanticTokensParams, Uri,
	WorkspaceSymbolParams,
	notification::{
		DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
		Notification as _, PublishDiagnostics,
	},
	request::{
		Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
		PrepareRenameRequest, RangeFormatting, References, Rename, Request as _,
		SemanticTokensFullRequest, WorkspaceSymbolRequest,
	},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
	ClientState,
	analysis_scheduler::{
		CancellationToken, CompletedWork, OUTSTANDING_WORK_CAPACITY, OWNER_PENDING_CAPACITY, Task,
		TaskError, TaskResult, WorkItem, WorkerPool, WorkerState,
	},
	compiler_state::{CloseAction, CompilerState},
	diagnostics,
	document_store::{Document, DocumentStore, DocumentStoreRevision},
	workspace,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum DiagnosticKey {
	Project(PathBuf),
	Document(String),
}

type DiagnosticOwner = String;

#[derive(Default)]
struct DiagnosticPublication {
	replace_project: bool,
	retire_keys: Vec<DiagnosticKey>,
	retire_document: Option<String>,
	clear_before: bool,
}

enum RequestRevision {
	Global(DocumentStoreRevision),
	Analysis {
		key: DiagnosticKey,
		generation: u64,
		target: Uri,
		document: Option<Document>,
	},
}

enum JobMetadata {
	Request {
		id: RequestId,
		revision: RequestRevision,
		cancellation: CancellationToken,
	},
	Diagnostics {
		key: DiagnosticKey,
		owner: DiagnosticOwner,
		generation: u64,
		cancellation: CancellationToken,
		publication: DiagnosticPublication,
	},
}

struct BarrierRequest {
	id: RequestId,
	through: u64,
}

struct ProtocolOwner<'a> {
	connection: &'a Connection,
	documents: Arc<Mutex<DocumentStore>>,
	pool: WorkerPool,
	pending: VecDeque<WorkItem>,
	/// Work already accepted by a worker. IDs remain here after logical
	/// cancellation/supersession until the completion is consumed, so queue
	/// admission cannot forget physical work that is still running or buffered.
	submitted: HashSet<u64>,
	jobs: HashMap<u64, JobMetadata>,
	unfinished: BTreeSet<u64>,
	requests: HashMap<RequestId, u64>,
	diagnostic_generations: HashMap<DiagnosticKey, u64>,
	diagnostic_cancellations: HashMap<DiagnosticKey, CancellationToken>,
	diagnostic_targets: HashMap<DiagnosticOwner, Vec<Uri>>,
	diagnostic_owner_keys: HashMap<DiagnosticOwner, DiagnosticKey>,
	document_keys: HashMap<String, DiagnosticKey>,
	target_owners: HashMap<Uri, DiagnosticOwner>,
	canonical_targets: HashMap<String, Uri>,
	barriers: Vec<BarrierRequest>,
	deferred_completions: VecDeque<CompletedWork>,
	defer_completion_effects: bool,
	next_job: u64,
	shutting_down: bool,
}

impl<'a> ProtocolOwner<'a> {
	fn new(
		connection: &'a Connection,
		documents: Arc<Mutex<DocumentStore>>,
		compiler_template: &CompilerState,
	) -> Self {
		Self {
			connection,
			documents,
			pool: WorkerPool::new(compiler_template),
			pending: VecDeque::new(),
			submitted: HashSet::new(),
			jobs: HashMap::new(),
			unfinished: BTreeSet::new(),
			requests: HashMap::new(),
			diagnostic_generations: HashMap::new(),
			diagnostic_cancellations: HashMap::new(),
			diagnostic_targets: HashMap::new(),
			diagnostic_owner_keys: HashMap::new(),
			document_keys: HashMap::new(),
			target_owners: HashMap::new(),
			canonical_targets: HashMap::new(),
			barriers: Vec::new(),
			deferred_completions: VecDeque::new(),
			defer_completion_effects: false,
			next_job: 1,
			shutting_down: false,
		}
	}

	fn snapshot(&self) -> Arc<DocumentStore> {
		Arc::new(self.documents.lock().unwrap().clone())
	}

	fn next_job_id(&mut self) -> u64 {
		let id = self.next_job;
		self.next_job = self.next_job.checked_add(1).expect("LSP job id exhausted");
		id
	}

	fn schedule_request_with_snapshot(
		&mut self,
		id: RequestId,
		target: Option<Uri>,
		snapshot: Arc<DocumentStore>,
		task: Task,
	) -> anyhow::Result<()> {
		// A reused JSON-RPC ID retires the prior logical request before admission.
		// If the replacement is rejected, the one rejection is the ID's terminal
		// response and the old physical completion is ignored.
		self.retire_duplicate_request(&id);
		if !self.reserve_work_slot(false)? {
			self.send_error(
				id,
				lsp_server::ErrorCode::ServerCancelled as i32,
				"analysis queue is full",
			)?;
			return Ok(());
		}
		let (revision, route) = if let Some(target) = target {
			let key = self.diagnostic_key_for_uri(&target);
			let generation = self.diagnostic_generations.get(&key).copied().unwrap_or(0);
			let document = snapshot.get(&target).cloned();
			(
				RequestRevision::Analysis {
					key: key.clone(),
					generation,
					target,
					document,
				},
				route_for(&key),
			)
		} else {
			(RequestRevision::Global(snapshot.revision()), 0)
		};
		let cancellation = CancellationToken::default();
		let job = self.next_job_id();
		self.requests.insert(id.clone(), job);
		self.jobs.insert(
			job,
			JobMetadata::Request {
				id: id.clone(),
				revision,
				cancellation: cancellation.clone(),
			},
		);
		self.unfinished.insert(job);
		let item = WorkItem {
			id: job,
			route,
			cancellation,
			task,
		};
		if !self.enqueue(item, false)? {
			self.jobs.remove(&job);
			self.unfinished.remove(&job);
			self.requests.remove(&id);
			self.send_error(
				id,
				lsp_server::ErrorCode::ServerCancelled as i32,
				"analysis queue is full",
			)?;
		}
		Ok(())
	}

	#[cfg(test)]
	fn schedule_request(
		&mut self,
		id: RequestId,
		target: Option<Uri>,
		task: Task,
	) -> anyhow::Result<()> {
		let snapshot = self.snapshot();
		self.schedule_request_with_snapshot(id, target, snapshot, task)
	}

	fn schedule_diagnostics(
		&mut self,
		key: DiagnosticKey,
		owner: DiagnosticOwner,
		publication: DiagnosticPublication,
		task: Task,
	) -> anyhow::Result<()> {
		// Invalidate the predecessor before capacity handling can drain a buffered
		// completion. This makes publication linearize after supersession even at
		// the exact outstanding-work limit. If admission still fails, the advanced
		// generation suppresses the stale result rather than publishing old state.
		let generation = self.supersede_diagnostics(&key);
		if !self.reserve_diagnostic_slot(&key)? {
			return Ok(());
		}
		let cancellation = CancellationToken::default();
		self
			.diagnostic_cancellations
			.insert(key.clone(), cancellation.clone());
		let job = self.next_job_id();
		self.jobs.insert(
			job,
			JobMetadata::Diagnostics {
				key: key.clone(),
				owner,
				generation,
				cancellation: cancellation.clone(),
				publication,
			},
		);
		self.unfinished.insert(job);
		let item = WorkItem {
			id: job,
			route: route_for(&key),
			cancellation,
			task,
		};
		if !self.enqueue(item, true)? {
			if let Some(JobMetadata::Diagnostics {
				key, cancellation, ..
			}) = self.jobs.remove(&job)
			{
				cancellation.cancel();
				if self
					.diagnostic_cancellations
					.get(&key)
					.is_some_and(|current| current.ptr_eq(&cancellation))
				{
					self.diagnostic_cancellations.remove(&key);
				}
			}
			self.unfinished.remove(&job);
		}
		Ok(())
	}

	fn supersede_diagnostics(&mut self, key: &DiagnosticKey) -> u64 {
		if let Some(previous) = self.diagnostic_cancellations.remove(key) {
			previous.cancel();
		}
		let generation = self
			.diagnostic_generations
			.entry(key.clone())
			.and_modify(|generation| *generation += 1)
			.or_insert(1);
		let generation = *generation;
		let superseded = self
			.jobs
			.iter()
			.filter_map(|(job, metadata)| {
				matches!(
					metadata,
					JobMetadata::Diagnostics { key: pending_key, .. } if pending_key == key
				)
				.then_some(*job)
			})
			.collect::<Vec<_>>();
		for job in superseded {
			if let Some(JobMetadata::Diagnostics { cancellation, .. }) = self.jobs.remove(&job) {
				cancellation.cancel();
			}
			self.unfinished.remove(&job);
		}
		self.pending.retain(|item| self.jobs.contains_key(&item.id));
		generation
	}

	fn outstanding_work(&self) -> usize {
		self.submitted.len() + self.pending.len()
	}

	fn reserve_work_slot(&mut self, evict_pending: bool) -> anyhow::Result<bool> {
		if self.outstanding_work() < OUTSTANDING_WORK_CAPACITY {
			return Ok(true);
		}
		self.drain_completed()?;
		if self.outstanding_work() < OUTSTANDING_WORK_CAPACITY {
			return Ok(true);
		}
		if evict_pending && self.evict_pending()? {
			return Ok(true);
		}
		Ok(false)
	}

	fn reserve_diagnostic_slot(&mut self, key: &DiagnosticKey) -> anyhow::Result<bool> {
		if self.outstanding_work() < OUTSTANDING_WORK_CAPACITY {
			return Ok(true);
		}
		self.drain_completed()?;
		if self.outstanding_work() < OUTSTANDING_WORK_CAPACITY
			|| self.pending.iter().any(|item| {
				matches!(
					self.jobs.get(&item.id),
					Some(JobMetadata::Diagnostics { key: pending_key, .. }) if pending_key == key
				)
			}) {
			return Ok(true);
		}
		self.evict_pending()
	}

	fn enqueue(&mut self, item: WorkItem, replace_pending: bool) -> anyhow::Result<bool> {
		let id = item.id;
		match self.pool.try_submit(item) {
			Ok(()) => {
				self.submitted.insert(id);
				Ok(true)
			}
			Err(TrySendError::Full(item)) => {
				if self.pending.len() >= OWNER_PENDING_CAPACITY
					&& (!replace_pending || !self.evict_pending()?)
				{
					return Ok(false);
				}
				self.pending.push_back(item);
				Ok(true)
			}
			Err(TrySendError::Disconnected(item)) => {
				self.fail_disconnected_job(item.id)?;
				Ok(true)
			}
		}
	}

	fn retire_duplicate_request(&mut self, id: &RequestId) {
		let Some(job) = self.requests.remove(id) else {
			return;
		};
		self.pending.retain(|item| item.id != job);
		self.unfinished.remove(&job);
		if let Some(JobMetadata::Request { cancellation, .. }) = self.jobs.remove(&job) {
			cancellation.cancel();
		}
	}

	fn evict_pending(&mut self) -> anyhow::Result<bool> {
		let Some(index) = self
			.pending
			.iter()
			.position(|item| matches!(self.jobs.get(&item.id), Some(JobMetadata::Request { .. })))
			.or_else(|| (!self.pending.is_empty()).then_some(0))
		else {
			return Ok(false);
		};
		let item = self
			.pending
			.remove(index)
			.expect("pending index was selected");
		item.cancellation.cancel();
		self.unfinished.remove(&item.id);
		match self.jobs.remove(&item.id) {
			Some(JobMetadata::Request {
				id, cancellation, ..
			}) => {
				cancellation.cancel();
				if self.requests.get(&id) == Some(&item.id) {
					self.requests.remove(&id);
				}
				self.send_error(
					id,
					lsp_server::ErrorCode::ServerCancelled as i32,
					"analysis queue is full",
				)?;
			}
			Some(JobMetadata::Diagnostics {
				key, cancellation, ..
			}) => {
				cancellation.cancel();
				if self
					.diagnostic_cancellations
					.get(&key)
					.is_some_and(|current| current.ptr_eq(&cancellation))
				{
					self.diagnostic_cancellations.remove(&key);
				}
			}
			None => {}
		}
		Ok(true)
	}

	fn drain_completed(&mut self) -> anyhow::Result<()> {
		while let Ok(completed) = self.pool.completed().try_recv() {
			if self.defer_completion_effects {
				// Reclaim physical capacity while preserving the protocol batch's
				// response/publication ordering boundary.
				self.submitted.remove(&completed.id);
				self.deferred_completions.push_back(completed);
			} else {
				self.complete(completed)?;
			}
		}
		Ok(())
	}

	fn finish_protocol_batch(&mut self) -> anyhow::Result<()> {
		self.defer_completion_effects = false;
		while let Some(completed) = self.deferred_completions.pop_front() {
			self.complete(completed)?;
		}
		Ok(())
	}

	fn dispatch_pending(&mut self) -> anyhow::Result<()> {
		let pending = self.pending.len();
		for _ in 0..pending {
			let item = self
				.pending
				.pop_front()
				.expect("pending length was captured");
			let id = item.id;
			match self.pool.try_submit(item) {
				Ok(()) => {
					self.submitted.insert(id);
				}
				Err(TrySendError::Full(item)) => {
					self.pending.push_back(item);
				}
				Err(TrySendError::Disconnected(item)) => {
					self.fail_disconnected_job(item.id)?;
				}
			}
		}
		self.finish_barriers()
	}

	fn fail_disconnected_job(&mut self, job: u64) -> anyhow::Result<()> {
		self.unfinished.remove(&job);
		match self.jobs.remove(&job) {
			Some(JobMetadata::Request {
				id, cancellation, ..
			}) => {
				cancellation.cancel();
				self.requests.remove(&id);
				self.send_error(
					id,
					lsp_server::ErrorCode::InternalError as i32,
					"analysis workers stopped unexpectedly",
				)?;
			}
			Some(JobMetadata::Diagnostics {
				key, cancellation, ..
			}) => {
				cancellation.cancel();
				if self
					.diagnostic_cancellations
					.get(&key)
					.is_some_and(|current| current.ptr_eq(&cancellation))
				{
					self.diagnostic_cancellations.remove(&key);
				}
			}
			None => {}
		}
		Ok(())
	}

	fn diagnostic_key_for_uri(&mut self, uri: &Uri) -> DiagnosticKey {
		let document = canonical_document(uri);
		match discovered_diagnostic_key(uri) {
			Ok(key @ DiagnosticKey::Project(_)) => {
				self.document_keys.insert(document, key.clone());
				key
			}
			Ok(document_key) => {
				let key = self
					.document_keys
					.get(&document)
					.cloned()
					.unwrap_or(document_key);
				self.document_keys.insert(document, key.clone());
				key
			}
			Err(()) => {
				let key = self
					.document_keys
					.get(&document)
					.cloned()
					.unwrap_or_else(|| DiagnosticKey::Document(document.clone()));
				self.document_keys.insert(document, key.clone());
				key
			}
		}
	}

	fn cancel_request(&mut self, id: &RequestId) -> anyhow::Result<()> {
		let Some(job) = self.requests.remove(id) else {
			return Ok(());
		};
		if let Some(JobMetadata::Request { cancellation, .. }) = self.jobs.remove(&job) {
			cancellation.cancel();
			self.unfinished.remove(&job);
			self.pending.retain(|item| item.id != job);
			self.send_error(
				id.clone(),
				lsp_server::ErrorCode::RequestCanceled as i32,
				"request cancelled",
			)?;
		}
		self.finish_barriers()?;
		Ok(())
	}

	fn complete(&mut self, completed: CompletedWork) -> anyhow::Result<()> {
		self.submitted.remove(&completed.id);
		self.unfinished.remove(&completed.id);
		let Some(metadata) = self.jobs.remove(&completed.id) else {
			self.finish_barriers()?;
			return Ok(());
		};
		match (metadata, completed.result) {
			(
				JobMetadata::Request {
					id,
					revision,
					cancellation,
				},
				TaskResult::Request(result),
			) => {
				self.requests.remove(&id);
				if cancellation.is_cancelled() {
					self.send_error(
						id,
						lsp_server::ErrorCode::RequestCanceled as i32,
						"request cancelled",
					)?;
				} else {
					self.finish_request(id, revision, result)?;
				}
			}
			(
				JobMetadata::Diagnostics {
					key,
					owner,
					generation,
					cancellation,
					publication,
				},
				TaskResult::Diagnostics(Ok(publications)),
			) => {
				if !cancellation.is_cancelled()
					&& self.diagnostic_generations.get(&key) == Some(&generation)
				{
					self.publish_diagnostics(key, owner, publications, publication)?;
				}
			}
			(JobMetadata::Diagnostics { .. }, TaskResult::Diagnostics(Err(_))) => {}
			(JobMetadata::Request { id, .. }, _) => {
				self.send_error(
					id,
					lsp_server::ErrorCode::InternalError as i32,
					"analysis worker returned the wrong result kind",
				)?;
			}
			(JobMetadata::Diagnostics { .. }, _) => {}
		}
		self.finish_barriers()?;
		Ok(())
	}

	fn finish_request(
		&self,
		id: RequestId,
		revision: RequestRevision,
		result: Result<serde_json::Value, TaskError>,
	) -> anyhow::Result<()> {
		match result {
			Ok(value) => {
				let documents = self.documents.lock().unwrap();
				let current = match revision {
					RequestRevision::Global(revision) => documents.revision() == revision,
					RequestRevision::Analysis {
						key,
						generation,
						target,
						document,
					} => {
						self.diagnostic_generations.get(&key).copied().unwrap_or(0) == generation
							&& documents.get(&target) == document.as_ref()
					}
				};
				drop(documents);
				if current {
					self
						.connection
						.sender
						.send(Message::Response(Response::new_ok(id, value)))?;
				} else {
					self.send_error(
						id,
						lsp_server::ErrorCode::ContentModified as i32,
						"document or project changed while analyzing request",
					)?;
				}
			}
			Err(TaskError::Cancelled) => self.send_error(
				id,
				lsp_server::ErrorCode::RequestCanceled as i32,
				"request cancelled",
			)?,
			Err(TaskError::ContentModified(message)) => {
				self.send_error(id, lsp_server::ErrorCode::ContentModified as i32, &message)?
			}
			Err(TaskError::InvalidParams(message)) => {
				self.send_error(id, lsp_server::ErrorCode::InvalidParams as i32, &message)?
			}
			Err(TaskError::Internal(message)) => {
				self.send_error(id, lsp_server::ErrorCode::InternalError as i32, &message)?
			}
		}
		Ok(())
	}

	fn publish_diagnostics(
		&mut self,
		key: DiagnosticKey,
		owner: DiagnosticOwner,
		publications: Vec<PublishDiagnosticsParams>,
		publication: DiagnosticPublication,
	) -> anyhow::Result<()> {
		let DiagnosticPublication {
			replace_project,
			retire_keys,
			retire_document,
			clear_before,
		} = publication;
		let new_targets = publications
			.iter()
			.map(|publication| publication.uri.clone())
			.collect::<Vec<_>>();
		let retired_owner_uri = retire_document
			.as_ref()
			.and_then(|_| owner.parse::<Uri>().ok());
		let mut owners = vec![owner.clone()];
		if replace_project {
			let mut project_owners = self
				.diagnostic_owner_keys
				.iter()
				.filter_map(|(candidate, candidate_key)| {
					(candidate_key == &key).then_some(candidate.clone())
				})
				.collect::<Vec<_>>();
			project_owners.sort();
			owners.extend(project_owners);
		}
		if !retire_keys.is_empty() {
			let mut retired_owners = self
				.diagnostic_owner_keys
				.iter()
				.filter_map(|(candidate, candidate_key)| {
					retire_keys
						.contains(candidate_key)
						.then_some(candidate.clone())
				})
				.collect::<Vec<_>>();
			retired_owners.sort();
			owners.extend(retired_owners);
		}
		if let Some(retire_document) = retire_document {
			let mut equivalent_owners = self
				.diagnostic_owner_keys
				.keys()
				.filter_map(|candidate| {
					candidate
						.parse::<Uri>()
						.ok()
						.is_some_and(|uri| canonical_document(&uri) == retire_document)
						.then_some(candidate.clone())
				})
				.collect::<Vec<_>>();
			equivalent_owners.sort();
			owners.extend(equivalent_owners);
		}
		let mut seen_owners = HashSet::new();
		owners.retain(|owner| seen_owners.insert(owner.clone()));
		let mut stale_targets = Vec::new();
		for previous_owner in owners {
			for stale in self
				.diagnostic_targets
				.get(&previous_owner)
				.into_iter()
				.flatten()
				.filter(|target| !new_targets.contains(*target))
				.cloned()
				.collect::<Vec<_>>()
			{
				if self.target_owners.get(&stale) == Some(&previous_owner) {
					stale_targets.push((previous_owner.clone(), stale));
				}
			}
		}
		if let Some(ref retired_owner_uri) = retired_owner_uri {
			stale_targets.sort_by(|(_, left), (_, right)| {
				(left != retired_owner_uri)
					.cmp(&(right != retired_owner_uri))
					.then_with(|| left.as_str().cmp(right.as_str()))
			});
		}

		let mut publications = publications;
		if clear_before
			&& let Some(retired_owner_uri) = retired_owner_uri
			&& let Some(index) = publications
				.iter()
				.position(|publication| publication.uri == retired_owner_uri)
		{
			let publication = publications.remove(index);
			self.publish_owned_diagnostics(&owner, publication, false)?;
		}
		if clear_before {
			self.publish_stale_clears(&stale_targets)?;
		}
		for publication in publications {
			// Close jobs publish against the exact URI lifecycle that closed. They
			// must not displace a still-open equivalent spelling; ordinary analysis
			// publications do replace the prior canonical spelling.
			self.publish_owned_diagnostics(&owner, publication, !clear_before)?;
		}
		if !clear_before {
			self.publish_stale_clears(&stale_targets)?;
		}
		self.diagnostic_targets.insert(owner.clone(), new_targets);
		self.diagnostic_owner_keys.insert(owner, key);
		Ok(())
	}

	fn publish_owned_diagnostics(
		&mut self,
		owner: &DiagnosticOwner,
		publication: PublishDiagnosticsParams,
		replace_canonical: bool,
	) -> anyhow::Result<()> {
		if replace_canonical {
			let canonical = canonical_document(&publication.uri);
			if let Some(previous) = self.canonical_targets.get(&canonical).cloned()
				&& previous != publication.uri
				&& let Some(previous_owner) = self.target_owners.remove(&previous)
			{
				self.send_diagnostics(current_clear(&self.documents, previous.clone()))?;
				if let Some(targets) = self.diagnostic_targets.get_mut(&previous_owner) {
					targets.retain(|target| target != &previous);
				}
			}
			self
				.canonical_targets
				.insert(canonical, publication.uri.clone());
		}
		if let Some(previous_owner) = self
			.target_owners
			.insert(publication.uri.clone(), owner.clone())
			&& previous_owner != *owner
			&& let Some(targets) = self.diagnostic_targets.get_mut(&previous_owner)
		{
			targets.retain(|target| target != &publication.uri);
		}
		self.send_diagnostics(publication)
	}

	fn publish_stale_clears(
		&mut self,
		stale_targets: &[(DiagnosticOwner, Uri)],
	) -> anyhow::Result<()> {
		for (previous_owner, stale) in stale_targets {
			if self.target_owners.get(stale) == Some(previous_owner) {
				self.send_diagnostics(current_clear(&self.documents, stale.clone()))?;
				self.target_owners.remove(stale);
				let canonical = canonical_document(stale);
				if self.canonical_targets.get(&canonical) == Some(stale) {
					self.canonical_targets.remove(&canonical);
				}
			}
		}
		Ok(())
	}

	fn send_diagnostics(&self, params: PublishDiagnosticsParams) -> anyhow::Result<()> {
		self
			.connection
			.sender
			.send(Message::Notification(ServerNotification::new(
				PublishDiagnostics::METHOD.to_string(),
				serde_json::to_value(params)?,
			)))?;
		Ok(())
	}

	fn send_error(&self, id: RequestId, code: i32, message: &str) -> anyhow::Result<()> {
		self
			.connection
			.sender
			.send(Message::Response(Response::new_err(
				id,
				code,
				message.into(),
			)))?;
		Ok(())
	}

	fn add_barrier(&mut self, id: RequestId) -> anyhow::Result<()> {
		let through = self.next_job.saturating_sub(1);
		self.barriers.push(BarrierRequest { id, through });
		self.finish_barriers()
	}

	fn finish_barriers(&mut self) -> anyhow::Result<()> {
		let mut pending = Vec::new();
		let barriers = std::mem::take(&mut self.barriers);
		for barrier in barriers {
			if self.unfinished.range(..=barrier.through).next().is_none() {
				self
					.connection
					.sender
					.send(Message::Response(Response::new_ok(
						barrier.id,
						serde_json::Value::Null,
					)))?;
			} else {
				pending.push(barrier);
			}
		}
		self.barriers = pending;
		Ok(())
	}

	fn begin_shutdown(&mut self, id: RequestId) -> anyhow::Result<()> {
		self
			.connection
			.sender
			.send(Message::Response(Response::new_ok(
				id,
				serde_json::Value::Null,
			)))?;
		self.shutting_down = true;
		let request_ids = self.requests.keys().cloned().collect::<Vec<_>>();
		for request in request_ids {
			self.cancel_request(&request)?;
		}
		for cancellation in self.diagnostic_cancellations.values() {
			cancellation.cancel();
		}
		Ok(())
	}

	fn finish(mut self) {
		for metadata in self.jobs.values() {
			match metadata {
				JobMetadata::Request { cancellation, .. }
				| JobMetadata::Diagnostics { cancellation, .. } => cancellation.cancel(),
			}
		}
		self.pending.clear();
		self.pool.finish();
	}
}

enum OwnerEvent {
	Protocol(Result<Message, crossbeam_channel::RecvError>),
	Completed(Result<CompletedWork, crossbeam_channel::RecvError>),
}

const PROTOCOL_QUEUE_CAPACITY: usize = 256;

/// Fixed-capacity ingress buffer for the serialized owner. The stdio
/// transport is itself a rendezvous channel, so buffering here establishes an
/// observable, finite protocol batch without applying state off-owner.
struct ProtocolInbox {
	receiver: crossbeam_channel::Receiver<Message>,
	flush: crossbeam_channel::Sender<()>,
	flushed: crossbeam_channel::Receiver<u64>,
	consumed: u64,
	stop: crossbeam_channel::Sender<()>,
	thread: Option<JoinHandle<()>>,
}

impl ProtocolInbox {
	fn new(source: crossbeam_channel::Receiver<Message>) -> Self {
		let (sender, receiver) = crossbeam_channel::bounded(PROTOCOL_QUEUE_CAPACITY);
		let (flush, flush_requested) = crossbeam_channel::bounded(1);
		let (flushed_sender, flushed) = crossbeam_channel::bounded(1);
		let (stop, stopped) = crossbeam_channel::bounded(1);
		let thread = std::thread::Builder::new()
			.name("nymph-lsp-protocol-inbox".to_string())
			.spawn(move || {
				let mut forwarded = 0_u64;
				'relay: loop {
					let message = crossbeam_channel::select_biased! {
						recv(stopped) -> _ => break,
						recv(flush_requested) -> request => {
							if request.is_err() { break }
							// Include source messages already ready at the handoff while
							// bounding the flush even under a continuously writing client.
							for _ in 0..PROTOCOL_QUEUE_CAPACITY {
								let Ok(message) = source.try_recv() else { break };
								crossbeam_channel::select_biased! {
									recv(stopped) -> _ => break 'relay,
									send(sender, message) -> result => if result.is_err() { break 'relay },
								}
								forwarded += 1;
							}
							if flushed_sender.send(forwarded).is_err() { break }
							continue;
						},
						recv(source) -> message => match message {
							Ok(message) => message,
							Err(_) => break,
						},
					};
					crossbeam_channel::select_biased! {
						recv(stopped) -> _ => break,
						send(sender, message) -> result => if result.is_err() { break },
					}
					forwarded += 1;
				}
			})
			.expect("failed to spawn LSP protocol inbox");
		Self {
			receiver,
			flush,
			flushed,
			consumed: 0,
			stop,
			thread: Some(thread),
		}
	}

	/// Capture every message the relay accepted before this flush request. The
	/// owner drains while waiting, so a full queue cannot deadlock an in-flight
	/// relay send. Relay-side flush priority closes the handoff race.
	fn batch(
		&mut self,
		first: Result<Message, crossbeam_channel::RecvError>,
	) -> Vec<Result<Message, crossbeam_channel::RecvError>> {
		let mut batch = vec![first];
		self.consumed += 1;
		if self.flush.send(()).is_err() {
			return batch;
		}
		let mut target = None;
		loop {
			if target.is_some_and(|target| self.consumed >= target) {
				break;
			}
			crossbeam_channel::select_biased! {
					recv(self.flushed) -> result => match result {
						Ok(flushed) => target = Some(flushed),
						Err(_) => break,
					},
				recv(self.receiver) -> message => match message {
					Ok(message) => {
						self.consumed += 1;
						batch.push(Ok(message));
					},
					Err(error) => {
						batch.push(Err(error));
						break;
					},
				},
			}
		}
		batch
	}

	fn finish(&mut self) {
		let _ = self.stop.try_send(());
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

impl Drop for ProtocolInbox {
	fn drop(&mut self) {
		self.finish();
	}
}

fn receive_owner_event(
	protocol: &crossbeam_channel::Receiver<Message>,
	completed: &crossbeam_channel::Receiver<CompletedWork>,
) -> OwnerEvent {
	crossbeam_channel::select_biased! {
		recv(protocol) -> message => OwnerEvent::Protocol(message),
		recv(completed) -> result => OwnerEvent::Completed(result),
	}
}

fn handle_protocol_message(
	owner: &mut ProtocolOwner<'_>,
	client_state: &mut ClientState,
	message: Result<Message, crossbeam_channel::RecvError>,
) -> anyhow::Result<bool> {
	match message {
		Ok(Message::Request(request)) => handle_request(owner, request),
		Ok(Message::Notification(notification)) => {
			handle_notification(owner, client_state, notification)
		}
		Ok(Message::Response(response)) => {
			client_state.handle_response(&response);
			Ok(false)
		}
		Err(_) => Ok(true),
	}
}

pub(crate) fn main_loop(
	connection: &Connection,
	documents: &Arc<Mutex<DocumentStore>>,
	compiler: &Arc<Mutex<CompilerState>>,
	client_state: &mut ClientState,
) -> anyhow::Result<()> {
	let mut owner = ProtocolOwner::new(connection, documents.clone(), &compiler.lock().unwrap());
	let mut inbox = ProtocolInbox::new(connection.receiver.clone());
	let completed = owner.pool.completed().clone();
	let result = (|| {
		let mut exit = false;
		while !exit {
			owner.dispatch_pending()?;
			// Protocol mutations and cancellations already queued at this boundary
			// linearize before worker results. The captured batch is finite: messages
			// arriving while it is processed cannot indefinitely starve a completion.
			match receive_owner_event(&inbox.receiver, &completed) {
				OwnerEvent::Protocol(message) => {
					owner.defer_completion_effects = true;
					for message in inbox.batch(message) {
						exit = handle_protocol_message(&mut owner, client_state, message)?;
						if exit {
							break;
						}
					}
					owner.drain_completed()?;
					owner.finish_protocol_batch()?;
					if !exit && let Ok(result) = completed.try_recv() {
						owner.complete(result)?;
					}
				}
				OwnerEvent::Completed(result) => match result {
					Ok(result) => owner.complete(result)?,
					Err(_) => exit = true,
				},
			}
		}
		Ok(())
	})();
	owner.finish();
	inbox.finish();
	result
}

fn handle_request(owner: &mut ProtocolOwner<'_>, request: ServerRequest) -> anyhow::Result<bool> {
	if owner.shutting_down {
		owner.send_error(
			request.id,
			lsp_server::ErrorCode::InvalidRequest as i32,
			"server is shutting down",
		)?;
		return Ok(false);
	}
	if request.method == "shutdown" {
		owner.retire_duplicate_request(&request.id);
		owner.begin_shutdown(request.id)?;
		return Ok(false);
	}
	if request.method == "test/barrier" {
		owner.add_barrier(request.id)?;
		return Ok(false);
	}

	let id = request.id.clone();
	let snapshot = owner.snapshot();
	let method = request.method.clone();
	let (target, task) = if method == WorkspaceSymbolRequest::METHOD {
		let Some(params) = decode_request_params::<WorkspaceSymbolParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		(None, workspace_symbol_task(snapshot.clone(), params))
	} else if method == HoverRequest::METHOD {
		let Some(params) = decode_request_params::<HoverParams>(owner, &id, request.params)? else {
			return Ok(false);
		};
		let uri = params
			.text_document_position_params
			.text_document
			.uri
			.clone();
		(Some(uri), hover_task(snapshot.clone(), params))
	} else if method == Formatting::METHOD {
		let Some(params) =
			decode_request_params::<DocumentFormattingParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		let uri = params.text_document.uri.clone();
		(Some(uri), formatting_task(snapshot.clone(), params))
	} else if method == RangeFormatting::METHOD {
		let Some(params) =
			decode_request_params::<DocumentRangeFormattingParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		let uri = params.text_document.uri.clone();
		(Some(uri), range_formatting_task(snapshot.clone(), params))
	} else if method == DocumentSymbolRequest::METHOD {
		let Some(params) = decode_request_params::<DocumentSymbolParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		let uri = params.text_document.uri.clone();
		(Some(uri), document_symbol_task(snapshot.clone(), params))
	} else if method == GotoDefinition::METHOD {
		let Some(params) = decode_request_params::<GotoDefinitionParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		let uri = params
			.text_document_position_params
			.text_document
			.uri
			.clone();
		(Some(uri), definition_task(snapshot.clone(), params))
	} else if method == Completion::METHOD {
		let Some(params) = decode_request_params::<CompletionParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		let uri = params.text_document_position.text_document.uri.clone();
		(Some(uri), completion_task(snapshot.clone(), params))
	} else if method == References::METHOD {
		let Some(params) = decode_request_params::<ReferenceParams>(owner, &id, request.params)? else {
			return Ok(false);
		};
		let uri = params.text_document_position.text_document.uri.clone();
		(Some(uri), references_task(snapshot.clone(), params))
	} else if method == PrepareRenameRequest::METHOD {
		let Some(params) =
			decode_request_params::<lsp_types::TextDocumentPositionParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		let uri = params.text_document.uri.clone();
		(Some(uri), prepare_rename_task(snapshot.clone(), params))
	} else if method == Rename::METHOD {
		let Some(params) = decode_request_params::<RenameParams>(owner, &id, request.params)? else {
			return Ok(false);
		};
		let uri = params.text_document_position.text_document.uri.clone();
		(Some(uri), rename_task(snapshot.clone(), params))
	} else if method == SemanticTokensFullRequest::METHOD {
		let Some(params) = decode_request_params::<SemanticTokensParams>(owner, &id, request.params)?
		else {
			return Ok(false);
		};
		let uri = params.text_document.uri.clone();
		(Some(uri), semantic_tokens_task(snapshot.clone(), params))
	} else {
		owner.send_error(
			request.id,
			lsp_server::ErrorCode::MethodNotFound as i32,
			&format!("unhandled request method `{method}`"),
		)?;
		return Ok(false);
	};
	owner.schedule_request_with_snapshot(id, target, snapshot, task)?;
	Ok(false)
}

fn decode_request_params<T: DeserializeOwned>(
	owner: &ProtocolOwner<'_>,
	id: &RequestId,
	params: serde_json::Value,
) -> anyhow::Result<Option<T>> {
	match serde_json::from_value(params) {
		Ok(params) => Ok(Some(params)),
		Err(error) => {
			owner.send_error(
				id.clone(),
				lsp_server::ErrorCode::InvalidParams as i32,
				&format!("invalid request parameters: {error}"),
			)?;
			Ok(None)
		}
	}
}

fn handle_notification(
	owner: &mut ProtocolOwner<'_>,
	client_state: &ClientState,
	notification: ServerNotification,
) -> anyhow::Result<bool> {
	if notification.method == "exit" {
		return Ok(true);
	}
	if notification.method == "$/cancelRequest" {
		if let Some(value) = notification.params.get("id")
			&& let Ok(id) = serde_json::from_value::<RequestId>(value.clone())
		{
			owner.cancel_request(&id)?;
		}
		return Ok(false);
	}
	if owner.shutting_down {
		return Ok(false);
	}
	if notification.method == DidChangeWatchedFiles::METHOD && !client_state.watchers_authoritative()
	{
		return Ok(false);
	}

	match notification.method.as_str() {
		method if method == DidOpenTextDocument::METHOD => {
			let Ok(params) = serde_json::from_value::<DidOpenTextDocumentParams>(notification.params)
			else {
				return Ok(false);
			};
			let uri = params.text_document.uri;
			owner.documents.lock().unwrap().open(
				uri.clone(),
				params.text_document.text,
				params.text_document.version,
			);
			schedule_document_diagnostics(owner, uri, false)?;
		}
		method if method == DidChangeTextDocument::METHOD => {
			let Ok(params) = serde_json::from_value::<DidChangeTextDocumentParams>(notification.params)
			else {
				return Ok(false);
			};
			let uri = params.text_document.uri;
			if let Some(change) = params.content_changes.into_iter().last() {
				let mut documents = owner.documents.lock().unwrap();
				let stale_untitled = is_untitled(&uri)
					&& documents
						.version(&uri)
						.is_some_and(|version| params.text_document.version <= version);
				let changed =
					!stale_untitled && documents.change_full(&uri, change.text, params.text_document.version);
				drop(documents);
				if changed {
					schedule_document_diagnostics(owner, uri, false)?;
				}
			}
		}
		method if method == DidCloseTextDocument::METHOD => {
			let Ok(params) = serde_json::from_value::<DidCloseTextDocumentParams>(notification.params)
			else {
				return Ok(false);
			};
			let uri = params.text_document.uri;
			let key = owner.diagnostic_key_for_uri(&uri);
			let before = owner.snapshot();
			owner.documents.lock().unwrap().close(&uri);
			if matches!(key, DiagnosticKey::Project(_)) {
				schedule_close_diagnostics(owner, key, uri, before)?;
			} else {
				owner.supersede_diagnostics(&key);
				clear_owner_diagnostics(owner, &canonical_owner(&uri), uri)?;
			}
		}
		method if method == DidChangeWatchedFiles::METHOD => {
			let Ok(params) = serde_json::from_value::<DidChangeWatchedFilesParams>(notification.params)
			else {
				return Ok(false);
			};
			let uris = params
				.changes
				.into_iter()
				.map(|change| change.uri)
				.collect::<Vec<_>>();
			owner.documents.lock().unwrap().filesystem_changed();
			schedule_watcher_diagnostics(owner, uris)?;
		}
		_ => {}
	}
	Ok(false)
}

fn schedule_document_diagnostics(
	owner: &mut ProtocolOwner<'_>,
	uri: Uri,
	closed: bool,
) -> anyhow::Result<()> {
	let key = owner.diagnostic_key_for_uri(&uri);
	let diagnostic_owner = canonical_owner(&uri);
	let snapshot = owner.snapshot();
	let task_uri = uri.clone();
	let task = Box::new(
		move |state: &mut WorkerState, cancellation: &CancellationToken| {
			let result = (|| {
				state.synchronize(&snapshot, cancellation)?;
				if closed && state.documents.get(&task_uri).is_none() {
					seed_closed_project(state, &task_uri, cancellation)?;
				}
				cancellation.checkpoint()?;
				let affected = state
					.compiler
					.affected_documents(&state.documents, &task_uri);
				diagnostics::collect_affected(
					&state.documents,
					&state.compiler,
					&task_uri,
					&affected,
					cancellation,
				)
			})();
			TaskResult::Diagnostics(result)
		},
	);
	owner.schedule_diagnostics(
		key,
		diagnostic_owner,
		DiagnosticPublication::default(),
		task,
	)
}

fn schedule_close_diagnostics(
	owner: &mut ProtocolOwner<'_>,
	key: DiagnosticKey,
	uri: Uri,
	before: Arc<DocumentStore>,
) -> anyhow::Result<()> {
	let diagnostic_owner = canonical_owner(&uri);
	let after = owner.snapshot();
	let retired_document = canonical_document(&uri);
	let retire_document = (!after
		.iter()
		.any(|(candidate, _)| canonical_document(candidate) == retired_document))
	.then_some(retired_document);
	let task_uri = uri.clone();
	let task = Box::new(
		move |state: &mut WorkerState, cancellation: &CancellationToken| {
			let result = (|| {
				state.synchronize(&before, cancellation)?;
				cancellation.checkpoint()?;
				let action = state
					.compiler
					.close(&mut state.documents, &task_uri)
					.map_err(|error| TaskError::Internal(error.to_string()))?;
				state.adopt_snapshot(&after);
				let CloseAction::PublishProject(affected) = action else {
					return Ok(vec![diagnostics::clear_params(task_uri.clone())]);
				};
				diagnostics::collect_affected(
					&state.documents,
					&state.compiler,
					&task_uri,
					&affected,
					cancellation,
				)
			})();
			TaskResult::Diagnostics(result)
		},
	);
	owner.schedule_diagnostics(
		key,
		diagnostic_owner,
		DiagnosticPublication {
			retire_document,
			clear_before: true,
			..DiagnosticPublication::default()
		},
		task,
	)
}

fn schedule_watcher_diagnostics(
	owner: &mut ProtocolOwner<'_>,
	mut uris: Vec<Uri>,
) -> anyhow::Result<()> {
	let manifests = uris
		.iter()
		.filter(|uri| is_manifest(uri))
		.cloned()
		.collect::<Vec<_>>();
	let manifest_roots = manifests
		.iter()
		.filter_map(manifest_root)
		.collect::<Vec<_>>();
	for manifest in manifests {
		schedule_manifest_diagnostics(owner, &manifest)?;
	}
	// Manifest reconstruction observes every current disk source below its
	// root, so source events from the same watcher batch must not schedule a
	// second job that supersedes the transition publication.
	uris.retain(|uri| {
		!is_manifest(uri)
			&& workspace::uri_to_path(uri)
				.and_then(|path| std::path::absolute(path).ok())
				.is_none_or(|path| !manifest_roots.iter().any(|root| path.starts_with(root)))
	});
	if uris.is_empty() {
		return Ok(());
	}
	let mut groups: HashMap<DiagnosticKey, Vec<Uri>> = HashMap::new();
	for uri in uris {
		let key = owner.diagnostic_key_for_uri(&uri);
		groups.entry(key).or_default().push(uri);
	}
	for (key, uris) in groups {
		let owner_key = uris
			.first()
			.map(canonical_owner)
			.unwrap_or_else(|| "watcher".to_string());
		let replace_project = false;
		let snapshot = owner.snapshot();
		let task_key = key.clone();
		let task = Box::new(
			move |state: &mut WorkerState, cancellation: &CancellationToken| {
				let result = (|| {
					state.synchronize(&snapshot, cancellation)?;
					let mut publications = Vec::new();
					if replace_project {
						for (uri, _) in state.documents.documents_in_update_order() {
							cancellation.checkpoint()?;
							if diagnostic_key(uri) == task_key {
								publications.extend(diagnostics::collect_state(
									&state.documents,
									&state.compiler,
									uri,
									cancellation,
								)?);
							}
						}
					} else {
						let refreshes = state
							.compiler
							.watched_files_changed(&mut state.documents, &uris)
							.map_err(|error| TaskError::Internal(error.to_string()))?;
						for refresh in refreshes {
							publications.extend(diagnostics::collect_affected(
								&state.documents,
								&state.compiler,
								&refresh.origin,
								&refresh.affected,
								cancellation,
							)?);
						}
						let unreadable_source = uris.iter().any(|uri| {
							!is_manifest(uri)
								&& workspace::uri_to_path(uri)
									.is_some_and(|path| std::fs::read_to_string(path).is_err())
						});
						if unreadable_source {
							for (uri, _) in state.documents.documents_in_update_order() {
								cancellation.checkpoint()?;
								if diagnostic_key(uri) == task_key {
									publications.extend(diagnostics::collect_state(
										&state.documents,
										&state.compiler,
										uri,
										cancellation,
									)?);
								}
							}
						}
						if publications.is_empty() {
							for uri in &uris {
								let affected = state.compiler.affected_documents(&state.documents, uri);
								publications.extend(diagnostics::collect_affected(
									&state.documents,
									&state.compiler,
									uri,
									&affected,
									cancellation,
								)?);
							}
						}
					}
					let mut seen = HashSet::new();
					publications.retain(|publication| seen.insert(publication.uri.as_str().to_string()));
					Ok(publications)
				})();
				TaskResult::Diagnostics(result)
			},
		);
		owner.schedule_diagnostics(
			key,
			owner_key,
			DiagnosticPublication {
				replace_project,
				..DiagnosticPublication::default()
			},
			task,
		)?;
	}
	Ok(())
}

fn schedule_manifest_diagnostics(
	owner: &mut ProtocolOwner<'_>,
	manifest: &Uri,
) -> anyhow::Result<()> {
	let Some(root) = manifest_root(manifest) else {
		return Ok(());
	};
	let snapshot = owner.snapshot();
	let mut affected_documents = snapshot
		.documents_in_update_order()
		.into_iter()
		.filter_map(|(uri, _)| {
			workspace::uri_to_path(uri)
				.and_then(|path| std::path::absolute(path).ok())
				.is_some_and(|path| path.starts_with(&root))
				.then_some(uri.clone())
		})
		.collect::<Vec<_>>();
	affected_documents.sort_by(|left, right| left.as_str().cmp(right.as_str()));

	let mut retired_keys = owner
		.document_keys
		.iter()
		.filter_map(|(document, key)| {
			PathBuf::from(document)
				.starts_with(&root)
				.then_some(key.clone())
		})
		.collect::<HashSet<_>>();
	retired_keys.extend(
		owner
			.diagnostic_owner_keys
			.iter()
			.filter_map(|(diagnostic_owner, key)| {
				diagnostic_owner
					.parse::<Uri>()
					.ok()
					.and_then(|uri| workspace::uri_to_path(&uri))
					.and_then(|path| std::path::absolute(path).ok())
					.is_some_and(|path| path.starts_with(&root))
					.then_some(key.clone())
			}),
	);

	let mut groups: HashMap<DiagnosticKey, Vec<Uri>> = HashMap::new();
	for uri in affected_documents {
		let document = canonical_document(&uri);
		let previous = owner.document_keys.get(&document).cloned();
		let current = discovered_diagnostic_key(&uri)
			.unwrap_or_else(|()| previous.unwrap_or_else(|| DiagnosticKey::Document(document.clone())));
		owner.document_keys.insert(document, current.clone());
		groups.entry(current).or_default().push(uri);
	}

	let current_keys = groups.keys().cloned().collect::<HashSet<_>>();
	let mut retired_keys = retired_keys
		.difference(&current_keys)
		.cloned()
		.collect::<Vec<_>>();
	for retired in &retired_keys {
		owner.supersede_diagnostics(retired);
	}
	retired_keys.sort_by_key(route_for);

	if groups.is_empty() {
		for key in retired_keys.clone() {
			owner.schedule_diagnostics(
				key,
				canonical_owner(manifest),
				DiagnosticPublication {
					replace_project: true,
					retire_keys: retired_keys.clone(),
					..DiagnosticPublication::default()
				},
				Box::new(|_, cancellation| {
					TaskResult::Diagnostics(
						cancellation
							.checkpoint()
							.map_err(TaskError::from)
							.map(|()| Vec::new()),
					)
				}),
			)?;
		}
		return Ok(());
	}

	for (key, mut uris) in groups {
		uris.sort_by(|left, right| left.as_str().cmp(right.as_str()));
		let diagnostic_owner = uris
			.first()
			.map(canonical_owner)
			.unwrap_or_else(|| canonical_owner(manifest));
		let task_snapshot = snapshot.clone();
		let task = Box::new(
			move |state: &mut WorkerState, cancellation: &CancellationToken| {
				let result = (|| {
					state.synchronize(&task_snapshot, cancellation)?;
					let mut publications = Vec::new();
					for uri in &uris {
						cancellation.checkpoint()?;
						publications.extend(diagnostics::collect_state(
							&state.documents,
							&state.compiler,
							uri,
							cancellation,
						)?);
					}
					let mut seen = HashSet::new();
					publications.retain(|publication| seen.insert(publication.uri.as_str().to_string()));
					Ok(publications)
				})();
				TaskResult::Diagnostics(result)
			},
		);
		owner.schedule_diagnostics(
			key,
			diagnostic_owner,
			DiagnosticPublication {
				replace_project: true,
				retire_keys: retired_keys.clone(),
				..DiagnosticPublication::default()
			},
			task,
		)?;
	}
	Ok(())
}

fn seed_closed_project(
	state: &mut WorkerState,
	uri: &Uri,
	cancellation: &CancellationToken,
) -> Result<(), TaskError> {
	let Ok(workspace::UriClass::ProjectFile { path, .. }) = workspace::classify_uri(uri) else {
		return Ok(());
	};
	cancellation.checkpoint()?;
	let source = std::fs::read_to_string(path).unwrap_or_default();
	state
		.compiler
		.open(&mut state.documents, uri.clone(), source, 0)
		.map_err(|error| TaskError::Internal(error.to_string()))?;
	let action = state
		.compiler
		.close(&mut state.documents, uri)
		.map_err(|error| TaskError::Internal(error.to_string()))?;
	if matches!(action, CloseAction::Clear) {
		return Ok(());
	}
	cancellation.checkpoint()?;
	Ok(())
}

fn clear_owner_diagnostics(
	owner: &mut ProtocolOwner<'_>,
	diagnostic_owner: &str,
	uri: Uri,
) -> anyhow::Result<()> {
	if let Some(targets) = owner.diagnostic_targets.remove(diagnostic_owner) {
		for target in targets {
			if owner
				.target_owners
				.get(&target)
				.is_some_and(|current| current == diagnostic_owner)
			{
				owner.send_diagnostics(current_clear(&owner.documents, target.clone()))?;
				owner.target_owners.remove(&target);
				let canonical = canonical_document(&target);
				if owner.canonical_targets.get(&canonical) == Some(&target) {
					owner.canonical_targets.remove(&canonical);
				}
			}
		}
	} else {
		owner.send_diagnostics(diagnostics::clear_params(uri))?;
	}
	Ok(())
}

fn current_clear(documents: &Mutex<DocumentStore>, uri: Uri) -> PublishDiagnosticsParams {
	let version = documents.lock().unwrap().version(&uri);
	PublishDiagnosticsParams {
		uri,
		diagnostics: Vec::new(),
		version,
	}
}

fn compiler_task(
	snapshot: Arc<DocumentStore>,
	work: impl FnOnce(&mut WorkerState, &CancellationToken) -> Result<serde_json::Value, TaskError>
	+ Send
	+ 'static,
) -> Task {
	Box::new(move |state, cancellation| {
		let result = (|| {
			state.synchronize(&snapshot, cancellation)?;
			cancellation.checkpoint()?;
			let result = work(state, cancellation)?;
			cancellation.checkpoint()?;
			Ok(result)
		})();
		TaskResult::Request(result)
	})
}

fn parse_task(
	work: impl FnOnce(&CancellationToken) -> Result<serde_json::Value, TaskError> + Send + 'static,
) -> Task {
	Box::new(move |_, cancellation| TaskResult::Request(work(cancellation)))
}

fn json(value: impl Serialize) -> Result<serde_json::Value, TaskError> {
	serde_json::to_value(value).map_err(|error| TaskError::Internal(error.to_string()))
}

fn workspace_symbol_task(snapshot: Arc<DocumentStore>, params: WorkspaceSymbolParams) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		state
			.compiler
			.refresh_workspace_symbols_cancellable(&state.documents, cancellation)?;
		let snapshot = state
			.compiler
			.workspace_symbol_snapshot_cancellable(&state.documents, cancellation)?;
		json(crate::workspace_symbols::workspace_symbols_cancellable(
			&snapshot,
			&params,
			cancellation,
		)?)
	})
}

fn hover_task(snapshot: Arc<DocumentStore>, params: HoverParams) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		let uri = &params.text_document_position_params.text_document.uri;
		let snapshot = state.compiler.analysis_for_uri(&state.documents, uri);
		cancellation.checkpoint()?;
		json(snapshot.and_then(|snapshot| crate::hover::hover_snapshot(&snapshot, &params)))
	})
}

fn formatting_task(snapshot: Arc<DocumentStore>, params: DocumentFormattingParams) -> Task {
	parse_task(move |cancellation| {
		cancellation.checkpoint()?;
		let result = crate::formatting::document_formatting(&snapshot, &params);
		cancellation.checkpoint()?;
		json(result)
	})
}

fn range_formatting_task(
	snapshot: Arc<DocumentStore>,
	params: DocumentRangeFormattingParams,
) -> Task {
	parse_task(move |cancellation| {
		cancellation.checkpoint()?;
		let result = crate::formatting::document_range_formatting(&snapshot, &params);
		cancellation.checkpoint()?;
		json(result)
	})
}

fn document_symbol_task(snapshot: Arc<DocumentStore>, params: DocumentSymbolParams) -> Task {
	parse_task(move |cancellation| {
		cancellation.checkpoint()?;
		let result =
			crate::document_symbols::document_symbols_cancellable(&snapshot, &params, cancellation)?;
		cancellation.checkpoint()?;
		json(result)
	})
}

fn definition_task(snapshot: Arc<DocumentStore>, params: GotoDefinitionParams) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		let uri = &params.text_document_position_params.text_document.uri;
		let Some(snapshot) = state.compiler.analysis_for_uri(&state.documents, uri) else {
			return json(crate::definition::definition(&state.documents, &params));
		};
		cancellation.checkpoint()?;
		let candidate = crate::definition::definition_snapshot_candidate(
			&state.documents,
			&state.compiler,
			&snapshot,
			&params,
		);
		cancellation.checkpoint()?;
		let result = candidate
			.map(|candidate| {
				candidate.validate_disk_source_result().map_err(|()| {
					TaskError::ContentModified(
						"definition target changed while analyzing request".to_string(),
					)
				})
			})
			.transpose()?;
		json(result)
	})
}

fn completion_task(snapshot: Arc<DocumentStore>, params: CompletionParams) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		let uri = &params.text_document_position.text_document.uri;
		let result = if let Some(snapshot) = state.compiler.completion_for_uri(&state.documents, uri) {
			cancellation.checkpoint()?;
			Some(crate::completion::completion_snapshot(&snapshot, &params))
		} else {
			crate::completion::completion(&state.documents, &params)
		};
		json(result)
	})
}

fn references_task(snapshot: Arc<DocumentStore>, params: ReferenceParams) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		let uri = &params.text_document_position.text_document.uri;
		let snapshot = state
			.compiler
			.references_analysis_for_uri(&state.documents, uri);
		cancellation.checkpoint()?;
		let result = match snapshot {
			Some(snapshot) => crate::references::references_snapshot_candidate_cancellable(
				&state.documents,
				&state.compiler,
				&snapshot,
				&params,
				cancellation,
			)?
			.map(|candidate| candidate.validate_disk_sources_cancellable(cancellation))
			.transpose()?,
			None => None,
		};
		cancellation.checkpoint()?;
		json(result)
	})
}

fn prepare_rename_task(
	snapshot: Arc<DocumentStore>,
	params: lsp_types::TextDocumentPositionParams,
) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		let uri = &params.text_document.uri;
		let snapshot = state
			.compiler
			.references_analysis_for_uri(&state.documents, uri);
		cancellation.checkpoint()?;
		let result = match snapshot {
			Some(snapshot) => crate::rename::rename_candidate_cancellable(
				&state.documents,
				&state.compiler,
				&snapshot,
				params.position,
				"",
				cancellation,
			)?
			.map(|candidate| candidate.validate_prepare_cancellable(cancellation))
			.transpose()?,
			None => None,
		};
		json(result)
	})
}

fn rename_task(snapshot: Arc<DocumentStore>, params: RenameParams) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		if !crate::rename::valid_new_name(&params.new_name) {
			return Err(TaskError::InvalidParams(
				"new name must be exactly one Nymph identifier".to_string(),
			));
		}
		let uri = &params.text_document_position.text_document.uri;
		let Some(snapshot) = state
			.compiler
			.references_analysis_for_uri(&state.documents, uri)
		else {
			return Err(TaskError::InvalidParams(
				"target is not renameable".to_string(),
			));
		};
		cancellation.checkpoint()?;
		let Some(candidate) = crate::rename::rename_candidate_cancellable(
			&state.documents,
			&state.compiler,
			&snapshot,
			params.text_document_position.position,
			&params.new_name,
			cancellation,
		)?
		else {
			return Err(TaskError::InvalidParams(
				"target is not renameable".to_string(),
			));
		};
		cancellation.checkpoint()?;
		let edit = candidate.validate_disk_sources_cancellable(cancellation)?;
		json(edit)
	})
}

fn semantic_tokens_task(snapshot: Arc<DocumentStore>, params: SemanticTokensParams) -> Task {
	compiler_task(snapshot, move |state, cancellation| {
		let uri = &params.text_document.uri;
		let result = if let Some(snapshot) = state.compiler.analysis_for_uri(&state.documents, uri) {
			cancellation.checkpoint()?;
			crate::semantic_tokens::semantic_tokens_snapshot_cancellable(
				&snapshot,
				&params,
				cancellation,
			)?
		} else {
			crate::semantic_tokens::semantic_tokens_for_open_document_cancellable(
				&state.documents,
				&params,
				cancellation,
			)?
		};
		cancellation.checkpoint()?;
		json(result)
	})
}

fn diagnostic_key(uri: &Uri) -> DiagnosticKey {
	discovered_diagnostic_key(uri)
		.unwrap_or_else(|()| DiagnosticKey::Document(canonical_document(uri)))
}

fn discovered_diagnostic_key(uri: &Uri) -> Result<DiagnosticKey, ()> {
	let Some(path) = workspace::uri_to_path(uri) else {
		return Ok(DiagnosticKey::Document(uri.as_str().to_string()));
	};
	let absolute = std::path::absolute(&path).map_err(|_| ())?;
	match workspace::detect(&absolute) {
		Ok(Some(project)) => Ok(DiagnosticKey::Project(
			std::path::absolute(project.src_root).map_err(|_| ())?,
		)),
		Ok(None) => Ok(DiagnosticKey::Document(
			absolute.to_string_lossy().into_owned(),
		)),
		Err(_) => Err(()),
	}
}

fn canonical_document(uri: &Uri) -> String {
	workspace::uri_to_path(uri)
		.and_then(|path| std::path::absolute(path).ok())
		.map_or_else(
			|| uri.as_str().to_string(),
			|path| path.to_string_lossy().into_owned(),
		)
}

fn route_for(key: &DiagnosticKey) -> u64 {
	let mut hasher = DefaultHasher::new();
	key.hash(&mut hasher);
	hasher.finish()
}

fn canonical_owner(uri: &Uri) -> DiagnosticOwner {
	uri.as_str().to_string()
}

fn manifest_root(uri: &Uri) -> Option<PathBuf> {
	let manifest = workspace::uri_to_path(uri)?;
	std::path::absolute(manifest.parent()?).ok()
}

fn is_untitled(uri: &Uri) -> bool {
	uri
		.scheme()
		.is_some_and(|scheme| scheme.as_str().eq_ignore_ascii_case("untitled"))
}

fn is_manifest(uri: &Uri) -> bool {
	workspace::uri_to_path(uri)
		.and_then(|path| path.file_name().map(|name| name == "nymph.toml"))
		.unwrap_or(false)
}

#[cfg(test)]
mod tests {
	use super::*;
	use lsp_server::Notification;
	use std::sync::atomic::{AtomicBool, Ordering};

	#[test]
	fn ready_protocol_edit_precedes_ready_completion() {
		let (protocol_sender, protocol) = crossbeam_channel::bounded(1);
		let (completed_sender, completed) = crossbeam_channel::bounded(1);
		protocol_sender
			.send(Message::Notification(Notification::new(
				DidChangeTextDocument::METHOD.into(),
				serde_json::Value::Null,
			)))
			.unwrap();
		completed_sender
			.send(CompletedWork {
				id: 1,
				result: TaskResult::Request(Ok(serde_json::Value::Null)),
			})
			.unwrap();
		assert!(matches!(
			receive_owner_event(&protocol, &completed),
			OwnerEvent::Protocol(Ok(Message::Notification(_)))
		));
	}

	#[test]
	fn ready_protocol_cancellation_precedes_ready_completion() {
		let (protocol_sender, protocol) = crossbeam_channel::bounded(1);
		let (completed_sender, completed) = crossbeam_channel::bounded(1);
		protocol_sender
			.send(Message::Notification(Notification::new(
				"$/cancelRequest".into(),
				serde_json::json!({ "id": 7 }),
			)))
			.unwrap();
		completed_sender
			.send(CompletedWork {
				id: 1,
				result: TaskResult::Request(Ok(serde_json::Value::Null)),
			})
			.unwrap();
		assert!(matches!(
			receive_owner_event(&protocol, &completed),
			OwnerEvent::Protocol(Ok(Message::Notification(_)))
		));
	}

	#[test]
	fn captured_protocol_batch_does_not_starve_ready_completion() {
		let (protocol_sender, protocol) = crossbeam_channel::bounded(0);
		let mut inbox = ProtocolInbox::new(protocol);
		let (completed_sender, completed) = crossbeam_channel::bounded(1);
		let (accepted_sender, accepted) = crossbeam_channel::bounded(0);
		let producer = std::thread::spawn(move || {
			for id in 1..=2 {
				protocol_sender
					.send(Message::Notification(Notification::new(
						"$/cancelRequest".into(),
						serde_json::json!({ "id": id }),
					)))
					.unwrap();
			}
			accepted_sender.send(()).unwrap();
		});
		completed_sender
			.send(CompletedWork {
				id: 7,
				result: TaskResult::Request(Ok(serde_json::Value::Null)),
			})
			.unwrap();

		accepted.recv().unwrap();
		let OwnerEvent::Protocol(first) = receive_owner_event(&inbox.receiver, &completed) else {
			panic!("ready protocol batch must linearize before completion");
		};
		let batch = inbox.batch(first);
		assert!(!batch.is_empty());
		assert_eq!(completed.try_recv().unwrap().id, 7);
		producer.join().unwrap();
	}

	#[test]
	fn captured_protocol_batch_stops_at_the_acknowledged_boundary() {
		let (protocol_sender, protocol) = crossbeam_channel::bounded(2);
		for id in 2..=3 {
			protocol_sender
				.send(Message::Notification(Notification::new(
					"$/cancelRequest".into(),
					serde_json::json!({ "id": id }),
				)))
				.unwrap();
		}
		let (flush, flush_requested) = crossbeam_channel::bounded(1);
		let (flushed_sender, flushed) = crossbeam_channel::bounded(1);
		let (stop, _stopped) = crossbeam_channel::bounded(1);
		flushed_sender.send(2).unwrap();
		let mut inbox = ProtocolInbox {
			receiver: protocol,
			flush,
			flushed,
			consumed: 0,
			stop,
			thread: None,
		};
		let first = Message::Notification(Notification::new(
			"$/cancelRequest".into(),
			serde_json::json!({ "id": 1 }),
		));

		let batch = inbox.batch(Ok(first));

		assert_eq!(batch.len(), 2);
		assert!(matches!(
			inbox.receiver.try_recv(),
			Ok(Message::Notification(_))
		));
		assert!(flush_requested.try_recv().is_ok());
	}

	#[test]
	fn batched_cancellation_precedes_capacity_reclaimed_completion() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let request = RequestId::from(41);
		let job = 7;
		let cancellation = CancellationToken::default();
		owner.requests.insert(request.clone(), job);
		owner.jobs.insert(
			job,
			JobMetadata::Request {
				id: request.clone(),
				revision: RequestRevision::Global(DocumentStoreRevision::default()),
				cancellation,
			},
		);
		owner.submitted.insert(job);
		owner.unfinished.insert(job);

		// Capacity reclamation may physically receive this completion while a
		// protocol batch is in progress, but its success effect remains deferred.
		owner.defer_completion_effects = true;
		owner.submitted.remove(&job);
		owner.deferred_completions.push_back(CompletedWork {
			id: job,
			result: TaskResult::Request(Ok(serde_json::json!("stale"))),
		});
		owner.cancel_request(&request).unwrap();
		owner.finish_protocol_batch().unwrap();

		let Message::Response(response) = client.receiver.recv().unwrap() else {
			panic!("cancellation must produce a response");
		};
		assert_eq!(response.id, request);
		assert_eq!(
			response
				.response_result
				.as_ref()
				.err()
				.map(|error| error.code),
			Some(lsp_server::ErrorCode::RequestCanceled as i32)
		);
		assert!(client.receiver.try_recv().is_err());
	}

	#[test]
	fn exact_capacity_replacement_suppresses_buffered_old_diagnostics() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let uri: Uri = "untitled:capacity-generation".parse().unwrap();
		let key = owner.diagnostic_key_for_uri(&uri);
		let old_job = 1;
		let old_cancellation = CancellationToken::default();
		owner.diagnostic_generations.insert(key.clone(), 1);
		owner
			.diagnostic_cancellations
			.insert(key.clone(), old_cancellation.clone());
		owner.jobs.insert(
			old_job,
			JobMetadata::Diagnostics {
				key: key.clone(),
				owner: canonical_owner(&uri),
				generation: 1,
				cancellation: old_cancellation,
				publication: DiagnosticPublication::default(),
			},
		);
		owner.submitted.extend(1..=OUTSTANDING_WORK_CAPACITY as u64);
		assert_eq!(owner.outstanding_work(), OUTSTANDING_WORK_CAPACITY);

		owner
			.schedule_diagnostics(
				key.clone(),
				canonical_owner(&uri),
				DiagnosticPublication::default(),
				Box::new(|_, _| TaskResult::Diagnostics(Ok(Vec::new()))),
			)
			.unwrap();
		assert_eq!(owner.diagnostic_generations.get(&key), Some(&2));
		owner
			.complete(CompletedWork {
				id: old_job,
				result: TaskResult::Diagnostics(Ok(vec![PublishDiagnosticsParams {
					uri,
					diagnostics: Vec::new(),
					version: Some(1),
				}])),
			})
			.unwrap();
		assert!(client.receiver.try_recv().is_err());
		assert!(owner.outstanding_work() <= OUTSTANDING_WORK_CAPACITY);
		owner.finish();
	}

	fn client_state() -> ClientState {
		ClientState {
			watch_registration: crate::WatchRegistration::Unsupported,
		}
	}

	fn blocking_request(
		started: crossbeam_channel::Sender<()>,
		release: crossbeam_channel::Receiver<()>,
	) -> Task {
		Box::new(move |_, cancellation| {
			started.send(()).unwrap();
			release.recv().unwrap();
			TaskResult::Request(
				cancellation
					.checkpoint()
					.map(|()| serde_json::Value::String("completed".into()))
					.map_err(TaskError::from),
			)
		})
	}

	fn cancel(owner: &mut ProtocolOwner<'_>, id: serde_json::Value) {
		handle_notification(
			owner,
			&client_state(),
			Notification::new("$/cancelRequest".into(), serde_json::json!({ "id": id })),
		)
		.unwrap();
	}

	fn recv_response(client: &Connection) -> Response {
		match client.receiver.recv().unwrap() {
			Message::Response(response) => response,
			other => panic!("expected response, got {other:?}"),
		}
	}

	#[test]
	fn numeric_and_string_cancellation_is_immediate_before_and_during_work() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);

		owner
			.schedule_request(
				RequestId::from(7),
				None,
				blocking_request(started_sender, release),
			)
			.unwrap();
		started.recv().unwrap();
		cancel(&mut owner, serde_json::json!(7));
		let during = recv_response(&client);
		assert_eq!(during.id, RequestId::from(7));
		assert_eq!(
			during
				.response_result
				.as_ref()
				.err()
				.map(|error| error.code),
			Some(lsp_server::ErrorCode::RequestCanceled as i32)
		);

		let queued_ran = Arc::new(AtomicBool::new(false));
		let queued_ran_worker = queued_ran.clone();
		owner
			.schedule_request(
				RequestId::from("queued".to_string()),
				None,
				Box::new(move |_, cancellation| {
					queued_ran_worker.store(true, Ordering::SeqCst);
					TaskResult::Request(
						cancellation
							.checkpoint()
							.map(|()| serde_json::Value::Null)
							.map_err(TaskError::from),
					)
				}),
			)
			.unwrap();
		cancel(&mut owner, serde_json::json!("queued"));
		let before = recv_response(&client);
		assert_eq!(before.id, RequestId::from("queued".to_string()));
		assert_eq!(
			before
				.response_result
				.as_ref()
				.err()
				.map(|error| error.code),
			Some(lsp_server::ErrorCode::RequestCanceled as i32)
		);

		owner.add_barrier(RequestId::from(8)).unwrap();
		assert_eq!(recv_response(&client).id, RequestId::from(8));
		release_sender.send(()).unwrap();
		for _ in 0..2 {
			let completed = owner.pool.completed().recv().unwrap();
			owner.complete(completed).unwrap();
		}
		assert!(queued_ran.load(Ordering::SeqCst));
		assert!(client.receiver.try_recv().is_err());
		owner.finish();
	}

	#[test]
	fn stale_success_is_content_modified_but_an_unrelated_edit_is_not() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents.clone(), &CompilerState::new());
		let uri: Uri = "untitled:stale".parse().unwrap();
		let unrelated: Uri = "untitled:unrelated".parse().unwrap();
		documents
			.lock()
			.unwrap()
			.open(uri.clone(), "func old(): int = 1".into(), 1);
		documents
			.lock()
			.unwrap()
			.open(unrelated.clone(), "func other(): int = 1".into(), 1);

		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		owner
			.schedule_request(
				RequestId::from(10),
				Some(uri.clone()),
				blocking_request(started_sender, release),
			)
			.unwrap();
		started.recv().unwrap();
		documents
			.lock()
			.unwrap()
			.change_full(&unrelated, "func other(): int = 2".into(), 2);
		let unrelated_key = owner.diagnostic_key_for_uri(&unrelated);
		owner.supersede_diagnostics(&unrelated_key);
		release_sender.send(()).unwrap();
		let completed = owner.pool.completed().recv().unwrap();
		owner.complete(completed).unwrap();
		let current = recv_response(&client);
		assert_eq!(current.id, RequestId::from(10));
		assert_eq!(
			current.response_result.unwrap(),
			serde_json::json!("completed")
		);

		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		owner
			.schedule_request(
				RequestId::from(11),
				Some(uri.clone()),
				blocking_request(started_sender, release),
			)
			.unwrap();
		started.recv().unwrap();
		documents
			.lock()
			.unwrap()
			.change_full(&uri, "func latest(): int = 2".into(), 2);
		let key = owner.diagnostic_key_for_uri(&uri);
		owner.supersede_diagnostics(&key);
		release_sender.send(()).unwrap();
		let completed = owner.pool.completed().recv().unwrap();
		owner.complete(completed).unwrap();
		let stale = recv_response(&client);
		assert_eq!(stale.id, RequestId::from(11));
		assert_eq!(
			stale.response_result.as_ref().err().map(|error| error.code),
			Some(lsp_server::ErrorCode::ContentModified as i32)
		);
		owner.finish();
	}

	#[test]
	fn equivalent_uri_diagnostics_supersede_and_publish_latest_only() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("same.nym");
		let uri = workspace::path_to_uri(&path).unwrap();
		let alternate: Uri = uri
			.as_str()
			.replace("same.nym", "%73ame.nym")
			.parse()
			.unwrap();
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let first_key = owner.diagnostic_key_for_uri(&uri);
		let second_key = owner.diagnostic_key_for_uri(&alternate);
		assert_eq!(first_key, second_key);

		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		let old_uri = uri.clone();
		owner
			.schedule_diagnostics(
				first_key,
				canonical_owner(&uri),
				DiagnosticPublication::default(),
				Box::new(move |_, _| {
					started_sender.send(()).unwrap();
					release.recv().unwrap();
					TaskResult::Diagnostics(Ok(vec![PublishDiagnosticsParams {
						uri: old_uri,
						diagnostics: Vec::new(),
						version: Some(1),
					}]))
				}),
			)
			.unwrap();
		started.recv().unwrap();
		let latest_uri = alternate.clone();
		owner
			.schedule_diagnostics(
				second_key,
				canonical_owner(&alternate),
				DiagnosticPublication::default(),
				Box::new(move |_, cancellation| {
					TaskResult::Diagnostics(
						cancellation
							.checkpoint()
							.map_err(TaskError::from)
							.map(|()| {
								vec![PublishDiagnosticsParams {
									uri: latest_uri,
									diagnostics: Vec::new(),
									version: Some(2),
								}]
							}),
					)
				}),
			)
			.unwrap();
		release_sender.send(()).unwrap();
		for _ in 0..2 {
			let completed = owner.pool.completed().recv().unwrap();
			owner.complete(completed).unwrap();
		}
		match client.receiver.recv().unwrap() {
			Message::Notification(notification) => {
				let publication: PublishDiagnosticsParams =
					serde_json::from_value(notification.params).unwrap();
				assert_eq!(publication.uri, alternate);
				assert_eq!(publication.version, Some(2));
			}
			other => panic!("expected latest diagnostics, got {other:?}"),
		}
		assert!(client.receiver.try_recv().is_err());
		owner.finish();
	}

	#[test]
	fn sequential_equivalent_uri_diagnostics_clear_the_previous_spelling() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("same.nym");
		let uri = workspace::path_to_uri(&path).unwrap();
		let alternate: Uri = uri
			.as_str()
			.replace("same.nym", "%73ame.nym")
			.parse()
			.unwrap();
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let key = owner.diagnostic_key_for_uri(&uri);

		owner
			.publish_diagnostics(
				key.clone(),
				canonical_owner(&uri),
				vec![PublishDiagnosticsParams {
					uri: uri.clone(),
					diagnostics: Vec::new(),
					version: Some(1),
				}],
				DiagnosticPublication::default(),
			)
			.unwrap();
		let Message::Notification(first) = client.receiver.recv().unwrap() else {
			panic!("expected first diagnostics publication");
		};
		let first: PublishDiagnosticsParams = serde_json::from_value(first.params).unwrap();
		assert_eq!(first.uri, uri);
		assert_eq!(first.version, Some(1));

		owner
			.publish_diagnostics(
				key,
				canonical_owner(&alternate),
				vec![PublishDiagnosticsParams {
					uri: alternate.clone(),
					diagnostics: Vec::new(),
					version: Some(2),
				}],
				DiagnosticPublication::default(),
			)
			.unwrap();
		let publications = (0..2)
			.map(|_| {
				let Message::Notification(notification) = client.receiver.recv().unwrap() else {
					panic!("expected diagnostics publication");
				};
				serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap()
			})
			.collect::<Vec<_>>();
		assert_eq!(publications[0].uri, uri);
		assert!(publications[0].diagnostics.is_empty());
		assert_eq!(publications[1].uri, alternate);
		assert_eq!(publications[1].version, Some(2));
		assert!(client.receiver.try_recv().is_err());
		owner.finish();
	}

	#[test]
	fn saturated_diagnostic_replacement_evicts_pending_work_and_publishes_latest() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let uri: Uri = "untitled:saturated".parse().unwrap();
		let key = owner.diagnostic_key_for_uri(&uri);
		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		let stale_uri = uri.clone();
		owner
			.schedule_diagnostics(
				key.clone(),
				canonical_owner(&uri),
				DiagnosticPublication::default(),
				Box::new(move |_, _| {
					started_sender.send(()).unwrap();
					release.recv().unwrap();
					TaskResult::Diagnostics(Ok(vec![PublishDiagnosticsParams {
						uri: stale_uri,
						diagnostics: Vec::new(),
						version: Some(1),
					}]))
				}),
			)
			.unwrap();
		started.recv().unwrap();

		// The affine worker has one running diagnostic and eight queue slots.
		// Fill those slots, then fill the owner's bounded pending queue.
		for id in 0..(8 + OWNER_PENDING_CAPACITY) {
			owner
				.schedule_request(
					RequestId::from(id as i32),
					Some(uri.clone()),
					Box::new(|_, cancellation| {
						TaskResult::Request(
							cancellation
								.checkpoint()
								.map(|()| serde_json::Value::Null)
								.map_err(TaskError::from),
						)
					}),
				)
				.unwrap();
			assert!(owner.outstanding_work() <= OUTSTANDING_WORK_CAPACITY);
		}
		assert_eq!(owner.pending.len(), OWNER_PENDING_CAPACITY);

		let latest_uri = uri.clone();
		owner
			.schedule_diagnostics(
				key,
				canonical_owner(&uri),
				DiagnosticPublication::default(),
				Box::new(move |_, cancellation| {
					TaskResult::Diagnostics(
						cancellation
							.checkpoint()
							.map_err(TaskError::from)
							.map(|()| {
								vec![PublishDiagnosticsParams {
									uri: latest_uri,
									diagnostics: Vec::new(),
									version: Some(2),
								}]
							}),
					)
				}),
			)
			.unwrap();
		assert_eq!(owner.pending.len(), OWNER_PENDING_CAPACITY);
		assert!(owner.outstanding_work() <= OUTSTANDING_WORK_CAPACITY);

		release_sender.send(()).unwrap();
		for _ in 0..(1 + 8 + OWNER_PENDING_CAPACITY) {
			let completed = owner.pool.completed().recv().unwrap();
			owner.complete(completed).unwrap();
			owner.dispatch_pending().unwrap();
		}
		let messages = client.receiver.try_iter().collect::<Vec<_>>();
		let publications = messages
			.iter()
			.filter_map(|message| match message {
				Message::Notification(notification) => Some(
					serde_json::from_value::<PublishDiagnosticsParams>(notification.params.clone()).unwrap(),
				),
				_ => None,
			})
			.collect::<Vec<_>>();
		assert_eq!(publications.len(), 1);
		assert_eq!(publications[0].uri, uri);
		assert_eq!(publications[0].version, Some(2));
		assert_eq!(
			messages
				.iter()
				.filter(|message| matches!(message, Message::Response(response) if response.response_result.as_ref().err().is_some_and(|error| error.code == lsp_server::ErrorCode::ServerCancelled as i32)))
				.count(),
			1
		);
		owner.finish();
	}

	#[test]
	fn cancelled_submitted_work_remains_in_the_hard_capacity_accounting() {
		let (server, _client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::unbounded();

		for id in 0..OUTSTANDING_WORK_CAPACITY {
			let started_sender = started_sender.clone();
			let release = release.clone();
			let request_id = RequestId::from(id as i32);
			owner
				.schedule_request(
					request_id.clone(),
					None,
					Box::new(move |_, cancellation| {
						started_sender.send(()).unwrap();
						release.recv().unwrap();
						TaskResult::Request(
							cancellation
								.checkpoint()
								.map(|()| serde_json::Value::Null)
								.map_err(TaskError::from),
						)
					}),
				)
				.unwrap();
			started.recv().unwrap();
			owner.cancel_request(&request_id).unwrap();
			release_sender.send(()).unwrap();
			assert!(owner.outstanding_work() <= OUTSTANDING_WORK_CAPACITY);
		}
		assert_eq!(owner.outstanding_work(), OUTSTANDING_WORK_CAPACITY);

		// Admission drains completed tombstones before accepting more work; the
		// physical count never exceeds the bound even though metadata was removed
		// immediately at cancellation time.
		owner
			.schedule_request(
				RequestId::from("after-capacity".to_string()),
				None,
				Box::new(|_, _| TaskResult::Request(Ok(serde_json::Value::Null))),
			)
			.unwrap();
		assert!(owner.outstanding_work() <= OUTSTANDING_WORK_CAPACITY);
		owner.finish();
	}

	#[test]
	fn duplicate_request_id_at_capacity_has_one_terminal_response() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		let id = RequestId::from("duplicate".to_string());
		owner
			.schedule_request(id.clone(), None, blocking_request(started_sender, release))
			.unwrap();
		started.recv().unwrap();

		// Model submitted completions that have not yet been consumed. They are
		// deliberately metadata-free tombstones, just like cancelled work.
		for job in 10_000..10_000 + OUTSTANDING_WORK_CAPACITY as u64 - 1 {
			owner.submitted.insert(job);
		}
		assert_eq!(owner.outstanding_work(), OUTSTANDING_WORK_CAPACITY);
		owner
			.schedule_request(
				id.clone(),
				None,
				Box::new(|_, _| TaskResult::Request(Ok(serde_json::Value::Null))),
			)
			.unwrap();
		let response = recv_response(&client);
		assert_eq!(response.id, id);
		assert_eq!(
			response.response_result.unwrap_err().code,
			lsp_server::ErrorCode::ServerCancelled as i32
		);

		release_sender.send(()).unwrap();
		let completed = owner.pool.completed().recv().unwrap();
		owner.complete(completed).unwrap();
		assert!(client.receiver.try_recv().is_err());
		owner.finish();
	}

	#[test]
	fn repeated_shutdown_is_rejected_and_a_reused_id_has_one_terminal_response() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let id = RequestId::from("shutdown".to_string());
		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		owner
			.schedule_request(id.clone(), None, blocking_request(started_sender, release))
			.unwrap();
		started.recv().unwrap();

		handle_request(
			&mut owner,
			ServerRequest::new(id.clone(), "shutdown".into(), serde_json::Value::Null),
		)
		.unwrap();
		let first = recv_response(&client);
		assert_eq!(first.id, id);
		assert_eq!(first.response_result.unwrap(), serde_json::Value::Null);

		handle_request(
			&mut owner,
			ServerRequest::new(
				RequestId::from(2),
				"shutdown".into(),
				serde_json::Value::Null,
			),
		)
		.unwrap();
		let repeated = recv_response(&client);
		assert_eq!(repeated.id, RequestId::from(2));
		assert_eq!(
			repeated.response_result.unwrap_err().code,
			lsp_server::ErrorCode::InvalidRequest as i32
		);

		release_sender.send(()).unwrap();
		let completed = owner.pool.completed().recv().unwrap();
		owner.complete(completed).unwrap();
		assert!(client.receiver.try_recv().is_err());
		owner.finish();
	}

	#[test]
	fn disconnected_worker_returns_internal_error_once() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		owner.pool = WorkerPool::disconnected();
		let id = RequestId::from(52);

		owner
			.schedule_request(
				id.clone(),
				None,
				Box::new(|_, _| TaskResult::Request(Ok(serde_json::Value::Null))),
			)
			.unwrap();

		let response = recv_response(&client);
		assert_eq!(response.id, id);
		assert_eq!(
			response.response_result.unwrap_err().code,
			lsp_server::ErrorCode::InternalError as i32
		);
		assert!(client.receiver.try_recv().is_err());
		assert!(owner.requests.is_empty());
		assert!(owner.jobs.is_empty());
		owner.finish();
	}

	#[test]
	fn malformed_manifest_keeps_every_current_document_diagnostic_domain() {
		let temp = tempfile::tempdir().unwrap();
		let manifest_path = temp.path().join("nymph.toml");
		std::fs::write(&manifest_path, "[package\ninvalid").unwrap();
		let manifest_uri = workspace::path_to_uri(&manifest_path).unwrap();
		let first = workspace::path_to_uri(&temp.path().join("first.nym")).unwrap();
		let second = workspace::path_to_uri(&temp.path().join("second.nym")).unwrap();
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		{
			let mut documents = documents.lock().unwrap();
			documents.open(first.clone(), "func first(): int = 1".into(), 1);
			documents.open(second.clone(), "func second(): int = 2".into(), 1);
		}
		let mut owner = ProtocolOwner::new(&server, documents.clone(), &CompilerState::new());
		let first_key = owner.diagnostic_key_for_uri(&first);
		let second_key = owner.diagnostic_key_for_uri(&second);
		assert_ne!(first_key, second_key);
		for (key, uri) in [(first_key, first.clone()), (second_key, second.clone())] {
			owner
				.publish_diagnostics(
					key,
					canonical_owner(&uri),
					vec![PublishDiagnosticsParams {
						uri,
						diagnostics: vec![lsp_types::Diagnostic::default()],
						version: Some(1),
					}],
					DiagnosticPublication::default(),
				)
				.unwrap();
		}
		client.receiver.try_iter().for_each(drop);

		documents.lock().unwrap().filesystem_changed();
		schedule_manifest_diagnostics(&mut owner, &manifest_uri).unwrap();
		assert_eq!(owner.jobs.len(), 2);
		for _ in 0..2 {
			let completed = owner.pool.completed().recv().unwrap();
			owner.complete(completed).unwrap();
		}
		let publications = client
			.receiver
			.try_iter()
			.map(|message| {
				let Message::Notification(notification) = message else {
					panic!("expected diagnostics publication");
				};
				serde_json::from_value::<PublishDiagnosticsParams>(notification.params).unwrap()
			})
			.collect::<Vec<_>>();
		assert_eq!(publications.len(), 2);
		assert!(
			publications
				.iter()
				.all(|publication| !publication.diagnostics.is_empty())
		);
		assert_eq!(
			publications
				.iter()
				.map(|publication| publication.uri.clone())
				.collect::<HashSet<_>>(),
			HashSet::from([first, second])
		);
		owner.finish();
	}

	#[test]
	fn manifest_creation_supersedes_loose_work_and_publishes_the_project_revision() {
		let temp = tempfile::tempdir().unwrap();
		let source_root = temp.path().join("src");
		std::fs::create_dir(&source_root).unwrap();
		let source_path = source_root.join("main.nym");
		let source = "func main(): int = true";
		std::fs::write(&source_path, source).unwrap();
		let uri = workspace::path_to_uri(&source_path).unwrap();
		let manifest_uri = workspace::path_to_uri(&temp.path().join("nymph.toml")).unwrap();
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		documents
			.lock()
			.unwrap()
			.open(uri.clone(), source.into(), 1);
		let mut owner = ProtocolOwner::new(&server, documents.clone(), &CompilerState::new());
		let loose_key = owner.diagnostic_key_for_uri(&uri);
		assert!(matches!(loose_key, DiagnosticKey::Document(_)));

		let (started_sender, started) = crossbeam_channel::bounded(0);
		let (release_sender, release) = crossbeam_channel::bounded(0);
		let stale_uri = uri.clone();
		owner
			.schedule_diagnostics(
				loose_key,
				canonical_owner(&uri),
				DiagnosticPublication::default(),
				Box::new(move |_, _| {
					started_sender.send(()).unwrap();
					release.recv().unwrap();
					TaskResult::Diagnostics(Ok(vec![PublishDiagnosticsParams {
						uri: stale_uri,
						diagnostics: Vec::new(),
						version: Some(1),
					}]))
				}),
			)
			.unwrap();
		started.recv().unwrap();

		std::fs::write(
			temp.path().join("nymph.toml"),
			"[package]\nname='transition'\nversion='0.1.0'\n",
		)
		.unwrap();
		documents.lock().unwrap().filesystem_changed();
		schedule_watcher_diagnostics(&mut owner, vec![manifest_uri]).unwrap();
		release_sender.send(()).unwrap();
		for _ in 0..2 {
			let completed = owner.pool.completed().recv().unwrap();
			owner.complete(completed).unwrap();
		}

		match client.receiver.recv().unwrap() {
			Message::Notification(notification) => {
				let publication: PublishDiagnosticsParams =
					serde_json::from_value(notification.params).unwrap();
				assert_eq!(publication.uri, uri);
				assert_eq!(publication.version, Some(1));
				assert!(!publication.diagnostics.is_empty());
			}
			other => panic!("expected current project diagnostics, got {other:?}"),
		}
		assert!(client.receiver.try_recv().is_err());
		owner.finish();
	}

	#[test]
	fn malformed_supported_request_gets_invalid_params_and_server_continues() {
		let (server, client) = Connection::memory();
		let documents = Arc::new(Mutex::new(DocumentStore::default()));
		let mut owner = ProtocolOwner::new(&server, documents, &CompilerState::new());
		let malformed = ServerRequest::new(
			RequestId::from(40),
			HoverRequest::METHOD.into(),
			serde_json::json!({ "textDocument": {} }),
		);
		assert!(!handle_request(&mut owner, malformed).unwrap());
		let response = recv_response(&client);
		assert_eq!(response.id, RequestId::from(40));
		assert_eq!(
			response.response_result.unwrap_err().code,
			lsp_server::ErrorCode::InvalidParams as i32
		);

		assert!(
			!handle_request(
				&mut owner,
				ServerRequest::new(
					RequestId::from(41),
					"test/barrier".into(),
					serde_json::Value::Null,
				),
			)
			.unwrap()
		);
		assert_eq!(recv_response(&client).id, RequestId::from(41));
		owner.finish();
	}
}
