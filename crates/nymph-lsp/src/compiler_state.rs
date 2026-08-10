use std::{
	collections::{BTreeMap, BTreeSet, HashMap, HashSet},
	fs,
	path::PathBuf,
	sync::Arc,
};

use lsp_types::Uri;
use nymph_compiler::{
	CompilerSession, ModuleAnalysis, ModulePath, ProjectDiagnostic, ProjectId, SourceVersion,
};
use nymph_sema::EntryMode;

use crate::{
	document_store::{DocumentStore, DocumentStoreRevision},
	workspace,
};

#[derive(Clone)]
struct DocumentIdentity {
	project: ProjectId,
	module: ModulePath,
	entry: ModulePath,
	root: PathBuf,
	without_prelude: bool,
	kind: DocumentKind,
}

impl DocumentIdentity {
	fn module_identity(&self) -> ModuleIdentity {
		ModuleIdentity {
			project: self.project.clone(),
			module: self.module.clone(),
			without_prelude: self.without_prelude,
		}
	}
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ModuleIdentity {
	project: ProjectId,
	module: ModulePath,
	without_prelude: bool,
}

#[derive(Clone)]
enum DocumentKind {
	Project(PathBuf),
	Loose(PathBuf),
	NonFile,
}

pub enum CloseAction {
	PublishProject(Vec<Uri>),
	Clear,
}

pub struct WatchedFileRefresh {
	pub origin: Uri,
	pub affected: Vec<Uri>,
}

pub struct DiagnosticModuleSnapshot {
	pub uri: Uri,
	pub source: Arc<str>,
	pub version: Option<i32>,
	pub diagnostics: Vec<nymph_diagnostics::Diagnostic>,
}

pub struct DiagnosticsSnapshot {
	pub document_revision: DocumentStoreRevision,
	pub modules: Vec<DiagnosticModuleSnapshot>,
	pub owned_targets: Vec<Uri>,
}

pub struct AnalysisSnapshot {
	pub uri: Uri,
	pub project: ProjectId,
	pub module: ModulePath,
	/// Client sequence number used only to suppress responses after a newer
	/// notification. It is protocol metadata, not an analysis-cache key.
	pub document_version: i32,
	/// Identity of the complete open-document overlay state that produced this
	/// analysis, including dependency overlays and document lifecycles.
	pub document_revision: DocumentStoreRevision,
	/// Exact text of the requested open URI. This normally aliases `source`,
	/// but remains distinct when two equivalent URI spellings are open for the
	/// same canonical module and another spelling is the authoritative overlay.
	pub document_source: Arc<str>,
	pub source: Arc<str>,
	pub analysis: Arc<ModuleAnalysis>,
	entry: ModulePath,
	root: PathBuf,
	without_prelude: bool,
}

/// Immutable completion inputs from one authoritative project/overlay revision.
/// Unlike checked-body analysis, these remain available while source syntax is partial.
pub struct CompletionSnapshot {
	pub document_version: i32,
	pub document_revision: DocumentStoreRevision,
	pub source: Arc<str>,
	pub imported_names: Arc<[nymph_sema::query::ImportedName]>,
}

pub struct DefinitionTargetSnapshot {
	pub uri: Uri,
	pub source: Arc<str>,
	pub span: nymph_ast::Span,
	pub requires_disk_validation: bool,
}

pub struct ReferenceModuleSnapshot {
	pub uri: Uri,
	pub source: Arc<str>,
	pub occurrences: Vec<nymph_sema::query::ReferenceOccurrence>,
	pub occurrences_are_uniquely_editable: bool,
	pub document_version: Option<i32>,
	pub requires_disk_validation: bool,
}

/// One module in an immutable, overlay-authoritative workspace-symbol snapshot.
pub struct WorkspaceSymbolModuleSnapshot {
	pub module: ModulePath,
	pub uri: Uri,
	pub source: Arc<str>,
	pub declarations: Arc<[nymph_sema::TopLevelDeclaration]>,
}

pub struct WorkspaceSymbolSnapshot {
	pub document_revision: DocumentStoreRevision,
	pub modules: Vec<WorkspaceSymbolModuleSnapshot>,
}

pub struct CompilerState {
	pub session: CompilerSession,
	stdlib_session: CompilerSession,
	pub workspaces: HashMap<PathBuf, ProjectId>,
	synchronized_roots: HashSet<PathBuf>,
	documents: HashMap<Uri, DocumentIdentity>,
	effective_sources: HashMap<ModuleIdentity, Arc<str>>,
	authoritative_overlays: HashMap<ModuleIdentity, Uri>,
	manifest_errors: HashMap<Uri, String>,
	workspace_symbol_refresh_errors: HashSet<PathBuf>,
	diagnostic_targets: HashMap<String, Vec<String>>,
	diagnostic_owners: HashMap<String, String>,
}

impl Default for CompilerState {
	fn default() -> Self {
		Self::new()
	}
}

impl CompilerState {
	#[must_use]
	pub fn new() -> Self {
		Self::from_session(CompilerSession::new())
	}

	#[doc(hidden)]
	pub fn with_event_callback(callback: impl Fn(&str) + Send + Sync + 'static) -> Self {
		Self::from_session(CompilerSession::with_event_callback_and_tombstone_threshold(callback, 256))
	}

	fn from_session(session: CompilerSession) -> Self {
		Self {
			session,
			stdlib_session: CompilerSession::without_builtin_sources(),
			workspaces: HashMap::new(),
			synchronized_roots: HashSet::new(),
			documents: HashMap::new(),
			effective_sources: HashMap::new(),
			authoritative_overlays: HashMap::new(),
			manifest_errors: HashMap::new(),
			workspace_symbol_refresh_errors: HashSet::new(),
			diagnostic_targets: HashMap::new(),
			diagnostic_owners: HashMap::new(),
		}
	}

	pub fn open(
		&mut self,
		docs: &mut DocumentStore,
		uri: Uri,
		text: String,
		version: i32,
	) -> anyhow::Result<Vec<Uri>> {
		docs.open(uri.clone(), text, version);
		self.synchronize(docs, &uri)?;
		Ok(self.affected_project_documents(docs, &uri))
	}

	pub fn change(
		&mut self,
		docs: &mut DocumentStore,
		uri: &Uri,
		text: String,
		version: i32,
	) -> anyhow::Result<Vec<Uri>> {
		if !docs.change_full(uri, text, version) {
			return Ok(Vec::new());
		}
		self.synchronize(docs, uri)?;
		Ok(self.affected_project_documents(docs, uri))
	}

	pub fn close(&mut self, docs: &mut DocumentStore, uri: &Uri) -> anyhow::Result<CloseAction> {
		let previous = self.affected_project_documents(docs, uri);
		// Remove the overlay before choosing the effective replacement source.
		docs.close(uri);
		self.manifest_errors.remove(uri);
		let Some(identity) = self.documents.get(uri).cloned() else {
			return Ok(CloseAction::Clear);
		};
		if let Some(targets) = self.diagnostic_targets.remove(uri.as_str()) {
			for target in targets {
				if self
					.diagnostic_owners
					.get(&target)
					.is_some_and(|owner| owner == uri.as_str())
				{
					self.diagnostic_owners.remove(&target);
				}
			}
		}
		if !matches!(identity.kind, DocumentKind::Project(_)) {
			self.documents.remove(uri);
		}
		let module_identity = identity.module_identity();
		let remaining_overlay = self.remaining_overlay(docs, &module_identity, Some(uri));
		if let Some((open_uri, source, version)) = remaining_overlay {
			self.set_effective_source(&module_identity, source, SourceVersion(i64::from(version)));
			self
				.authoritative_overlays
				.insert(module_identity.clone(), open_uri);
			if !matches!(identity.kind, DocumentKind::Project(_)) {
				return Ok(CloseAction::Clear);
			}
			let mut affected = previous;
			for current in self.affected_project_documents(docs, uri) {
				if !affected.contains(&current) {
					affected.push(current);
				}
			}
			return Ok(CloseAction::PublishProject(affected));
		}
		self.authoritative_overlays.remove(&module_identity);
		let DocumentKind::Project(path) = identity.kind else {
			self.remove_effective_source(&module_identity);
			return Ok(CloseAction::Clear);
		};
		match fs::read_to_string(path) {
			Ok(source) => {
				self.set_effective_source(&module_identity, source, SourceVersion(0));
			}
			Err(_) => {
				self.remove_effective_source(&module_identity);
			}
		}
		let mut affected = previous;
		for current in self.affected_project_documents(docs, uri) {
			if !affected.contains(&current) {
				affected.push(current);
			}
		}
		let mut insert_at = affected
			.iter()
			.position(|candidate| candidate == uri)
			.map_or(0, |index| index + 1);
		for alias in self.published_closed_aliases(docs, &module_identity, uri) {
			if !affected.contains(&alias) {
				affected.insert(insert_at, alias);
				insert_at += 1;
			}
		}
		Ok(CloseAction::PublishProject(affected))
	}

	/// Refresh disk-backed compiler inputs after one client watcher batch.
	///
	/// Source events update only active project roots and defer to an existing
	/// authoritative open overlay for the same canonical module. Manifest
	/// events re-run discovery for open files below that manifest, allowing
	/// project/loose/error and source-root transitions to use the same identity
	/// retirement and overlay replay path as normal document synchronization.
	pub fn watched_files_changed(
		&mut self,
		docs: &mut DocumentStore,
		uris: &[Uri],
	) -> anyhow::Result<Vec<WatchedFileRefresh>> {
		let mut manifests = BTreeSet::new();
		let mut sources = BTreeSet::new();
		for uri in uris {
			let Some(path) = workspace::uri_to_path(uri) else {
				continue;
			};
			let path = std::path::absolute(path)?;
			if path.file_name().and_then(|name| name.to_str()) == Some("nymph.toml") {
				manifests.insert(path);
			} else if path.extension().and_then(|extension| extension.to_str()) == Some("nym") {
				sources.insert(path);
			}
		}
		let filesystem_changed = !manifests.is_empty() || !sources.is_empty();
		let mut refreshes = Vec::new();
		// Manifest discovery determines final project membership, so source
		// events in the same client batch must be routed only after it settles.
		for manifest in manifests {
			self.refresh_manifest(docs, &manifest, &mut refreshes)?;
		}
		for source in sources {
			self.refresh_source(docs, &source, &mut refreshes)?;
		}
		let mut covered = HashSet::new();
		for refresh in &mut refreshes {
			refresh
				.affected
				.retain(|uri| covered.insert(uri.as_str().to_string()));
		}
		refreshes.retain(|refresh| !refresh.affected.is_empty());
		if filesystem_changed {
			docs.filesystem_changed();
		}
		Ok(refreshes)
	}

	fn refresh_manifest(
		&mut self,
		docs: &DocumentStore,
		manifest: &std::path::Path,
		refreshes: &mut Vec<WatchedFileRefresh>,
	) -> anyhow::Result<()> {
		let Some(root) = manifest.parent() else {
			return Ok(());
		};
		let roots_to_rescan: HashSet<_> = self
			.documents
			.iter()
			.filter(|(uri, identity)| {
				docs.get(uri).is_some()
					&& matches!(identity.kind, DocumentKind::Project(_))
					&& (manifest.starts_with(&identity.root)
						|| workspace::uri_to_path(uri)
							.and_then(|path| std::path::absolute(path).ok())
							.is_some_and(|path| path.starts_with(root)))
			})
			.map(|(_, identity)| identity.root.clone())
			.collect();
		for source_root in &roots_to_rescan {
			self.synchronized_roots.remove(source_root);
		}
		let authoritative_uris: HashSet<_> = self
			.authoritative_overlays
			.values()
			.filter(|uri| docs.get(uri).is_some())
			.map(|uri| uri.as_str().to_string())
			.collect();
		let mut open_uris: Vec<_> = docs
			.iter()
			.filter_map(|(uri, _)| {
				workspace::uri_to_path(uri)
					.and_then(|path| std::path::absolute(path).ok())
					.filter(|path| path.starts_with(root))
					.map(|_| uri.clone())
			})
			.collect();
		// Synchronize non-authoritative aliases first. Replaying the previous
		// authoritative URI last preserves equivalent-URI ownership while still
		// allowing every alias to transition through fresh manifest discovery.
		open_uris.sort_by(|left, right| {
			authoritative_uris
				.contains(left.as_str())
				.cmp(&authoritative_uris.contains(right.as_str()))
				.then_with(|| left.as_str().cmp(right.as_str()))
		});
		for uri in open_uris {
			let mut affected = self.affected_project_documents(docs, &uri);
			self.synchronize(docs, &uri)?;
			for current in self.affected_project_documents(docs, &uri) {
				if !affected.contains(&current) {
					affected.push(current);
				}
			}
			refreshes.push(WatchedFileRefresh {
				origin: uri,
				affected,
			});
		}
		// A nested manifest also changes which sources belong to an enclosing
		// project even when that project's open entry lies outside the nested
		// directory. Reconcile one open representative for every such root.
		let mut representatives: Vec<_> = self
			.documents
			.iter()
			.filter(|(uri, identity)| {
				docs.get(uri).is_some()
					&& matches!(identity.kind, DocumentKind::Project(_))
					&& roots_to_rescan.contains(&identity.root)
					&& !self.synchronized_roots.contains(&identity.root)
			})
			.map(|(uri, identity)| (identity.root.clone(), uri.clone()))
			.collect();
		representatives.sort_by(|(left_root, left_uri), (right_root, right_uri)| {
			left_root
				.cmp(right_root)
				.then_with(|| {
					authoritative_uris
						.contains(right_uri.as_str())
						.cmp(&authoritative_uris.contains(left_uri.as_str()))
				})
				.then_with(|| left_uri.as_str().cmp(right_uri.as_str()))
		});
		representatives.dedup_by(|left, right| left.0 == right.0);
		for (_, uri) in representatives {
			let mut affected = self.affected_project_documents(docs, &uri);
			self.synchronize(docs, &uri)?;
			for current in self.affected_project_documents(docs, &uri) {
				if !affected.contains(&current) {
					affected.push(current);
				}
			}
			refreshes.push(WatchedFileRefresh {
				origin: uri,
				affected,
			});
		}
		Ok(())
	}

	fn refresh_source(
		&mut self,
		docs: &DocumentStore,
		path: &std::path::Path,
		refreshes: &mut Vec<WatchedFileRefresh>,
	) -> anyhow::Result<()> {
		let Some(event_root) = workspace::detect(path)
			.ok()
			.flatten()
			.and_then(|project| std::path::absolute(project.src_root).ok())
		else {
			return Ok(());
		};
		let mut roots: Vec<_> = self
			.documents
			.iter()
			.filter(|(uri, identity)| {
				docs.get(uri).is_some()
					&& identity.root == event_root
					&& matches!(identity.kind, DocumentKind::Project(_))
					&& self.synchronized_roots.contains(&identity.root)
			})
			.filter_map(|(_, identity)| {
				let module = nymph_project::module_from_file(&identity.root, path).ok()?;
				Some((
					identity.root.clone(),
					identity.project.clone(),
					identity.without_prelude,
					module,
				))
			})
			.collect();
		roots.sort_by(|left, right| {
			left
				.0
				.cmp(&right.0)
				.then_with(|| left.3.as_str().cmp(right.3.as_str()))
		});
		roots.dedup_by(|left, right| {
			left.0 == right.0 && left.1 == right.1 && left.2 == right.2 && left.3 == right.3
		});
		for (root, project, without_prelude, module) in roots {
			let disk_path = nymph_project::file_for_module(&root, &module);
			let Some(disk_uri) = workspace::path_to_uri(&disk_path) else {
				continue;
			};
			let identity = DocumentIdentity {
				project,
				module: module.clone(),
				entry: module.clone(),
				root,
				without_prelude,
				kind: DocumentKind::Project(disk_path.clone()),
			};
			self
				.documents
				.entry(disk_uri.clone())
				.and_modify(|current| {
					if docs.get(&disk_uri).is_none() {
						*current = identity.clone();
					}
				})
				.or_insert(identity.clone());
			let origin = self
				.publication_uri(docs, &identity, &module)
				.unwrap_or(disk_uri);
			let mut affected = self.affected_project_documents(docs, &origin);
			let module_identity = identity.module_identity();
			if self
				.remaining_overlay(docs, &module_identity, None)
				.is_none()
			{
				match fs::read_to_string(&disk_path) {
					Ok(source) => self.set_effective_source(&module_identity, source, SourceVersion(0)),
					Err(_) => {
						self.remove_effective_source(&module_identity);
					}
				}
			}
			for current in self.affected_project_documents(docs, &origin) {
				if !affected.contains(&current) {
					affected.push(current);
				}
			}
			refreshes.push(WatchedFileRefresh { origin, affected });
		}
		Ok(())
	}

	fn affected_project_documents(&self, docs: &DocumentStore, uri: &Uri) -> Vec<Uri> {
		let Some(identity) = self.documents.get(uri) else {
			return vec![uri.clone()];
		};
		self
			.session_for(identity)
			.reverse_importer_closure(identity.project.clone(), identity.module.clone())
			.into_iter()
			.filter_map(|module| {
				(module == identity.module)
					.then(|| uri.clone())
					.or_else(|| self.publication_uri(docs, identity, &module))
			})
			.collect()
	}

	fn publication_uri(
		&self,
		docs: &DocumentStore,
		identity: &DocumentIdentity,
		module: &ModulePath,
	) -> Option<Uri> {
		let module_identity = ModuleIdentity {
			project: identity.project.clone(),
			module: module.clone(),
			without_prelude: identity.without_prelude,
		};
		self
			.authoritative_overlays
			.get(&module_identity)
			.filter(|uri| docs.get(uri).is_some())
			.cloned()
			.or_else(|| {
				self
					.remaining_overlay(docs, &module_identity, None)
					.map(|(uri, _, _)| uri)
			})
			.or_else(|| workspace::key_to_uri(&identity.root, module.as_str()))
	}

	fn remaining_overlay(
		&self,
		docs: &DocumentStore,
		identity: &ModuleIdentity,
		excluded: Option<&Uri>,
	) -> Option<(Uri, String, i32)> {
		if let Some(uri) = self.authoritative_overlays.get(identity)
			&& excluded != Some(uri)
			&& let Some(document) = docs.get(uri)
		{
			return Some((uri.clone(), document.text.clone(), document.version));
		}
		self
			.documents
			.iter()
			.filter(|(uri, candidate)| excluded != Some(uri) && candidate.module_identity() == *identity)
			.filter_map(|(uri, _)| {
				docs
					.get(uri)
					.map(|document| (uri.clone(), document.text.clone(), document.version))
			})
			.max_by(
				|(left_uri, _, left_version), (right_uri, _, right_version)| {
					left_version
						.cmp(right_version)
						.then_with(|| left_uri.as_str().cmp(right_uri.as_str()))
				},
			)
	}

	fn published_closed_aliases(
		&self,
		docs: &DocumentStore,
		identity: &ModuleIdentity,
		origin: &Uri,
	) -> Vec<Uri> {
		let mut aliases: Vec<_> = self
			.diagnostic_targets
			.keys()
			.filter_map(|raw| raw.parse::<Uri>().ok())
			.filter(|uri| uri != origin && docs.get(uri).is_none())
			.filter(|uri| {
				self
					.documents
					.get(uri)
					.is_some_and(|candidate| candidate.module_identity() == *identity)
			})
			.collect();
		aliases.sort_by(|left, right| left.as_str().cmp(right.as_str()));
		aliases
	}

	fn set_effective_source(
		&mut self,
		identity: &ModuleIdentity,
		source: String,
		version: SourceVersion,
	) {
		self.session_mut(identity.without_prelude).set_source(
			identity.project.clone(),
			identity.module.clone(),
			source.clone(),
			version,
		);
		self
			.effective_sources
			.insert(identity.clone(), Arc::from(source));
	}

	fn remove_effective_source(&mut self, identity: &ModuleIdentity) {
		self
			.session_mut(identity.without_prelude)
			.remove_source(identity.project.clone(), identity.module.clone());
		self.effective_sources.remove(identity);
	}

	fn effective_source_for_uri(&self, uri: &Uri) -> Option<Arc<str>> {
		let identity = self.documents.get(uri)?.module_identity();
		self.effective_sources.get(&identity).cloned()
	}

	fn retire_transitioned_identity(
		&mut self,
		docs: &DocumentStore,
		uri: &Uri,
		identity: &DocumentIdentity,
	) {
		let root_still_open = self.documents.iter().any(|(candidate_uri, candidate)| {
			candidate_uri != uri
				&& docs.get(candidate_uri).is_some()
				&& matches!(candidate.kind, DocumentKind::Project(_))
				&& candidate.root == identity.root
		});
		if matches!(identity.kind, DocumentKind::Project(_)) {
			self.synchronized_roots.remove(&identity.root);
			if !root_still_open {
				self.retire_project(docs, uri, identity);
				return;
			}
		}
		let module_identity = identity.module_identity();
		if let Some((open_uri, source, version)) =
			self.remaining_overlay(docs, &module_identity, Some(uri))
		{
			self.set_effective_source(&module_identity, source, SourceVersion(i64::from(version)));
			self
				.authoritative_overlays
				.insert(module_identity, open_uri);
		} else {
			self.authoritative_overlays.remove(&module_identity);
			self.remove_effective_source(&module_identity);
		}
	}

	fn retire_project(&mut self, docs: &DocumentStore, uri: &Uri, identity: &DocumentIdentity) {
		let retired_uris: HashSet<_> = self
			.documents
			.iter()
			.filter(|(_, candidate)| candidate.project == identity.project)
			.map(|(uri, _)| uri.as_str().to_string())
			.collect();
		let origin = uri.as_str().to_string();
		let mut inherited_targets: BTreeSet<_> = self
			.diagnostic_targets
			.remove(&origin)
			.unwrap_or_default()
			.into_iter()
			.collect();
		for retired_uri in &retired_uris {
			if retired_uri == &origin {
				continue;
			}
			for target in self
				.diagnostic_targets
				.remove(retired_uri)
				.unwrap_or_default()
			{
				if self
					.diagnostic_owners
					.get(&target)
					.is_some_and(|owner| owner == retired_uri)
				{
					self
						.diagnostic_owners
						.insert(target.clone(), origin.clone());
					inherited_targets.insert(target);
				}
			}
		}
		if !inherited_targets.is_empty() {
			self
				.diagnostic_targets
				.insert(origin, inherited_targets.into_iter().collect());
		}

		let retired_modules: Vec<_> = self
			.effective_sources
			.keys()
			.filter(|module| module.project == identity.project)
			.cloned()
			.collect();
		for module in retired_modules {
			self.authoritative_overlays.remove(&module);
			self.remove_effective_source(&module);
		}
		self.documents.retain(|candidate_uri, candidate| {
			candidate.project != identity.project || docs.get(candidate_uri).is_some()
		});
	}

	pub fn analysis_for_uri(&self, docs: &DocumentStore, uri: &Uri) -> Option<AnalysisSnapshot> {
		if self.has_manifest_error(uri) {
			return None;
		}
		let document = docs.get(uri)?;
		let identity = self.documents.get(uri)?;
		let analysis = self.session_for(identity).tooling_analyze_module(
			identity.project.clone(),
			identity.entry.clone(),
			identity.module.clone(),
			!identity.without_prelude,
		)?;
		let source = analysis.source.clone();
		let document_source = Arc::from(document.text.as_str());
		Some(AnalysisSnapshot {
			uri: uri.clone(),
			project: identity.project.clone(),
			module: identity.module.clone(),
			document_version: document.version,
			document_revision: docs.revision(),
			document_source,
			source,
			analysis,
			entry: identity.entry.clone(),
			root: identity.root.clone(),
			without_prelude: identity.without_prelude,
		})
	}

	/// Refresh filesystem-backed project membership before taking the snapshot
	/// used by a project-wide references request. Open overlays remain
	/// authoritative for their logical modules.
	pub fn references_analysis_for_uri(
		&mut self,
		docs: &DocumentStore,
		uri: &Uri,
	) -> Option<AnalysisSnapshot> {
		let identity = self.documents.get(uri)?.clone();
		if matches!(identity.kind, DocumentKind::Project(_)) {
			self
				.synchronize_project_files(
					docs,
					&identity.root,
					&identity.project,
					identity.without_prelude,
				)
				.ok()?;
		}
		self.analysis_for_uri(docs, uri)
	}

	pub fn completion_for_uri(&self, docs: &DocumentStore, uri: &Uri) -> Option<CompletionSnapshot> {
		let document = docs.get(uri)?;
		let identity = self.documents.get(uri)?;
		let analysis = self.session_for(identity).tooling_completion_analysis(
			identity.project.clone(),
			identity.entry.clone(),
			identity.module.clone(),
			!identity.without_prelude,
		)?;
		if analysis.source.as_ref() != document.text {
			return None;
		}
		Some(CompletionSnapshot {
			document_version: document.version,
			document_revision: docs.revision(),
			source: analysis.source,
			imported_names: analysis.imported_names,
		})
	}

	/// Refresh disk membership and source facts for workspace-symbol search.
	/// This is deliberately request-scoped: ordinary editor changes preserve
	/// the compiler state's established no-reread behavior for unopened files.
	pub fn refresh_workspace_symbols(&mut self, docs: &DocumentStore) {
		let projects = self
			.synchronized_roots
			.iter()
			.filter_map(|root| {
				let project = self.workspaces.get(root)?.clone();
				let without_prelude = self.documents.values().find_map(|identity| {
					(identity.root == *root && identity.project == project)
						.then_some(identity.without_prelude)
				})?;
				Some((root.clone(), project, without_prelude))
			})
			.collect::<Vec<_>>();
		self.workspace_symbol_refresh_errors.clear();
		for (root, project, without_prelude) in projects {
			if self
				.synchronize_project_files(docs, &root, &project, without_prelude)
				.is_err()
			{
				self.workspace_symbol_refresh_errors.insert(root);
			}
		}
	}

	/// Snapshot declarations from every synchronized manifest project. Loose
	/// files and compiler/provider modules are intentionally outside the search
	/// boundary; open project overlays are already installed in each session.
	#[must_use]
	pub fn workspace_symbol_snapshot(&self, docs: &DocumentStore) -> WorkspaceSymbolSnapshot {
		let invalid_roots = self
			.manifest_errors
			.keys()
			.filter_map(|uri| {
				self
					.documents
					.get(uri)
					.map(|identity| identity.root.clone())
			})
			.collect::<HashSet<_>>();
		let mut projects = self
			.synchronized_roots
			.iter()
			.filter(|root| !invalid_roots.contains(*root))
			.filter(|root| !self.workspace_symbol_refresh_errors.contains(*root))
			.filter_map(|root| {
				let project = self.workspaces.get(root)?.clone();
				let without_prelude = self.documents.values().find_map(|identity| {
					(identity.root == *root && identity.project == project)
						.then_some(identity.without_prelude)
				})?;
				Some((root.clone(), project, without_prelude))
			})
			.collect::<Vec<_>>();
		projects.sort();
		projects.dedup();

		let mut modules = Vec::new();
		for (root, project, without_prelude) in projects {
			let session = if without_prelude {
				&self.stdlib_session
			} else {
				&self.session
			};
			for declarations in session.tooling_project_declarations(&project) {
				let Some(uri) = workspace::key_to_uri(&root, declarations.module.as_str()) else {
					continue;
				};
				modules.push(WorkspaceSymbolModuleSnapshot {
					module: declarations.module,
					uri,
					source: declarations.source,
					declarations: declarations.declarations,
				});
			}
		}
		modules.sort_by(|left, right| {
			left
				.uri
				.as_str()
				.cmp(right.uri.as_str())
				.then_with(|| left.module.cmp(&right.module))
		});
		WorkspaceSymbolSnapshot {
			document_revision: docs.revision(),
			modules,
		}
	}

	/// Resolve a checked stable target to an authoritative, reachable project source.
	/// Every validation is fallible so stale or provider-owned identities never become URIs.
	pub fn definition_target(
		&self,
		docs: &DocumentStore,
		snapshot: &AnalysisSnapshot,
		offset: usize,
	) -> Option<DefinitionTargetSnapshot> {
		if docs.revision() != snapshot.document_revision {
			return None;
		}
		let definition = snapshot.analysis.stable_definition_at(offset)?;
		if !matches!(
			definition.key,
			nymph_sema::DeclarationKey::TopLevel {
				category: nymph_sema::DeclarationCategory::Function
					| nymph_sema::DeclarationCategory::Let
					| nymph_sema::DeclarationCategory::Struct
					| nymph_sema::DeclarationCategory::Enum
					| nymph_sema::DeclarationCategory::Interface
					| nymph_sema::DeclarationCategory::TypeAlias,
				..
			}
		) {
			return None;
		}
		let nymph_sema::ModuleOrigin::Project(owner_project) = &definition.module.origin else {
			return None;
		};
		if owner_project.as_str() != snapshot.project.as_str()
			|| definition.module.project.as_str() != snapshot.project.as_str()
		{
			return None;
		}
		let module = ModulePath::new(definition.module.path.as_str()).ok()?;
		if module == snapshot.module {
			return None;
		}
		let session = if snapshot.without_prelude {
			&self.stdlib_session
		} else {
			&self.session
		};
		if !session.has_source(snapshot.project.clone(), module.clone()) {
			return None;
		}
		let analysis = session.tooling_analyze_module(
			snapshot.project.clone(),
			snapshot.entry.clone(),
			module.clone(),
			!snapshot.without_prelude,
		)?;
		let span = analysis.declaration_provenance(&definition)?.name_span;
		let uri = workspace::key_to_uri(&snapshot.root, module.as_str())?;
		let source = analysis.source.clone();
		let target_is_open = self.documents.iter().any(|(open_uri, identity)| {
			identity.project == snapshot.project
				&& identity.module == module
				&& identity.without_prelude == snapshot.without_prelude
				&& docs.get(open_uri).is_some()
		});
		valid_source_span(&source, span).then_some(DefinitionTargetSnapshot {
			uri,
			source,
			span,
			requires_disk_validation: !target_is_open,
		})
	}

	/// Collect semantic occurrences from the complete immutable project analysis
	/// that produced `snapshot`. Local identities stay isolated to their owner
	/// module; stable definitions are compared across every reachable module.
	pub fn reference_modules(
		&self,
		docs: &DocumentStore,
		snapshot: &AnalysisSnapshot,
		symbol: &nymph_sema::query::SymbolIdentity,
	) -> Option<Vec<ReferenceModuleSnapshot>> {
		if docs.revision() != snapshot.document_revision || snapshot.source != snapshot.document_source
		{
			return None;
		}
		let session = if snapshot.without_prelude {
			&self.stdlib_session
		} else {
			&self.session
		};
		let analyses = match symbol {
			nymph_sema::query::SymbolIdentity::Definition(_)
			| nymph_sema::query::SymbolIdentity::Module(_) => session.tooling_project_analyses(
				snapshot.project.clone(),
				snapshot.entry.clone(),
				!snapshot.without_prelude,
			),
			nymph_sema::query::SymbolIdentity::Local(_) => {
				vec![(
					snapshot.module.clone(),
					snapshot.source.clone(),
					Some(snapshot.analysis.clone()),
				)]
			}
		};
		let mut modules = Vec::new();
		for (module, source, analysis) in analyses {
			let occurrences = analysis.as_ref().map_or_else(Vec::new, |analysis| {
				nymph_sema::query::references_to(&analysis.semantic, symbol)
			});
			let occurrences_are_uniquely_editable = analysis.as_ref().is_none_or(|analysis| {
				nymph_sema::query::rename_occurrences(&analysis.semantic, symbol).is_some()
			});
			let overlay_uri = self.documents.iter().find_map(|(uri, identity)| {
				(identity.project == snapshot.project
					&& identity.module == module
					&& identity.without_prelude == snapshot.without_prelude
					&& self
						.authoritative_overlays
						.values()
						.any(|authoritative| authoritative == uri)
					&& docs.get(uri).is_some())
				.then_some(uri)
			});
			let uri = if let Some(uri) = overlay_uri {
				uri.clone()
			} else if module == snapshot.module
				&& !matches!(
					self.documents.get(&snapshot.uri)?.kind,
					DocumentKind::Project(_)
				) {
				snapshot.uri.clone()
			} else {
				workspace::key_to_uri(&snapshot.root, module.as_str())?
			};
			if occurrences
				.iter()
				.any(|occurrence| !valid_source_span(&source, occurrence.span))
			{
				return None;
			}
			let document_version = docs.get(&uri).map(|document| document.version);
			modules.push(ReferenceModuleSnapshot {
				uri,
				source,
				occurrences,
				occurrences_are_uniquely_editable,
				document_version,
				requires_disk_validation: document_version.is_none(),
			});
		}
		Some(modules)
	}

	pub fn diagnostics_for_uri(
		&self,
		docs: &DocumentStore,
		uri: &Uri,
	) -> Option<Arc<[ProjectDiagnostic]>> {
		if self.has_manifest_error(uri) {
			return None;
		}
		let identity = self.documents.get(uri)?;
		let _ = docs.get(uri)?;
		Some(self.session_for(identity).tooling_diagnostics(
			identity.project.clone(),
			identity.entry.clone(),
			!identity.without_prelude,
		))
	}

	#[must_use]
	pub fn has_manifest_error(&self, uri: &Uri) -> bool {
		self.manifest_errors.contains_key(uri)
	}

	pub fn manifest_error_snapshot(&self, uri: &Uri) -> Option<(String, Vec<Uri>)> {
		let message = self.manifest_errors.get(uri)?.clone();
		let stale_targets = self
			.diagnostic_targets
			.get(uri.as_str())
			.into_iter()
			.flatten()
			.filter(|target| target.as_str() != uri.as_str())
			.filter(|target| {
				self
					.diagnostic_owners
					.get(target.as_str())
					.is_some_and(|owner| owner == uri.as_str())
			})
			.filter_map(|target| target.parse::<Uri>().ok())
			.collect();
		Some((message, stale_targets))
	}

	pub fn diagnostics_snapshot(
		&self,
		docs: &DocumentStore,
		uri: &Uri,
	) -> Option<DiagnosticsSnapshot> {
		let _ = docs.version(uri)?;
		let document_revision = docs.revision();
		let identity = self.documents.get(uri)?;
		let session = self.session_for(identity);
		let diagnostics = session.tooling_diagnostics(
			identity.project.clone(),
			identity.entry.clone(),
			!identity.without_prelude,
		);
		let mut grouped: BTreeMap<String, Vec<nymph_diagnostics::Diagnostic>> = BTreeMap::new();
		for diagnostic in diagnostics.iter() {
			grouped
				.entry(diagnostic.module.clone())
				.or_default()
				.push(diagnostic.diag.clone());
		}
		// The requested module is always republished, including when clean, so
		// stale editor marks are cleared. Project dependencies are included in
		// graph order; diagnostics outside that order are retained afterwards.
		grouped.entry(identity.module.to_string()).or_default();
		let mut keys = session.graph_order(
			identity.project.clone(),
			identity.entry.clone(),
			EntryMode::Library,
		);
		keys.retain(|module| grouped.contains_key(module.as_str()));
		for module in grouped.keys() {
			if !keys.iter().any(|key| key.as_str() == module) {
				// Compiler-owned module identities (for example `std::io`) are
				// intentionally not valid project paths and have no workspace URI.
				// Skip them without suppressing publication for real project modules.
				if let Ok(module) = ModulePath::new(module.clone()) {
					keys.push(module);
				}
			}
		}
		let mut modules: Vec<_> = keys
			.into_iter()
			.filter_map(|module| {
				let module_uri = if module == identity.module {
					uri.clone()
				} else {
					self.publication_uri(docs, identity, &module)?
				};
				let open = docs.get(&module_uri);
				let source = self.effective_source_for_uri(&module_uri)?;
				Some(DiagnosticModuleSnapshot {
					uri: module_uri,
					source,
					version: open.map(|document| document.version),
					diagnostics: grouped.remove(module.as_str()).unwrap_or_default(),
				})
			})
			.collect();
		let owned_targets: Vec<Uri> = modules.iter().map(|module| module.uri.clone()).collect();
		if let Some(previous_targets) = self.diagnostic_targets.get(uri.as_str()) {
			for previous in previous_targets {
				if owned_targets
					.iter()
					.any(|current| current.as_str() == previous)
				{
					continue;
				}
				if !self
					.diagnostic_owners
					.get(previous)
					.is_some_and(|owner| owner == uri.as_str())
				{
					continue;
				}
				let Ok(previous_uri) = previous.parse::<Uri>() else {
					continue;
				};
				let open = docs.get(&previous_uri);
				modules.push(DiagnosticModuleSnapshot {
					uri: previous_uri.clone(),
					source: open
						.map(|document| Arc::from(document.text.as_str()))
						.or_else(|| self.effective_source_for_uri(&previous_uri))
						.unwrap_or_else(|| Arc::from("")),
					version: open.map(|document| document.version),
					diagnostics: Vec::new(),
				});
			}
		}
		Some(DiagnosticsSnapshot {
			document_revision,
			modules,
			owned_targets,
		})
	}

	pub fn affected_diagnostics_snapshot(
		&self,
		docs: &DocumentStore,
		origin: &Uri,
		uris: &[Uri],
	) -> DiagnosticsSnapshot {
		let mut modules: Vec<_> = uris
			.iter()
			.filter_map(|uri| {
				let identity = self.documents.get(uri)?;
				let open = docs.get(uri);
				let source = self
					.effective_sources
					.get(&identity.module_identity())
					.cloned()
					.unwrap_or_else(|| Arc::from(""));
				let diagnostics = if source.is_empty()
					&& !self
						.session_for(identity)
						.has_source(identity.project.clone(), identity.module.clone())
				{
					Vec::new()
				} else {
					self
						.session_for(identity)
						.tooling_diagnostics(
							identity.project.clone(),
							identity.entry.clone(),
							!identity.without_prelude,
						)
						.iter()
						.filter(|diagnostic| diagnostic.module == identity.module.as_str())
						.map(|diagnostic| diagnostic.diag.clone())
						.collect()
				};
				Some(DiagnosticModuleSnapshot {
					uri: uri.clone(),
					source,
					version: open.map(|document| document.version),
					diagnostics,
				})
			})
			.collect();
		let origin_identity = self
			.documents
			.get(origin)
			.map(DocumentIdentity::module_identity);
		let owned_targets: Vec<Uri> = modules
			.iter()
			.filter(|module| {
				module.uri == *origin
					|| !origin_identity.as_ref().is_some_and(|origin_identity| {
						docs.get(&module.uri).is_none()
							&& self
								.documents
								.get(&module.uri)
								.is_some_and(|candidate| candidate.module_identity() == *origin_identity)
					})
			})
			.map(|module| module.uri.clone())
			.collect();
		if let Some(previous_targets) = self.diagnostic_targets.get(origin.as_str()) {
			for previous in previous_targets {
				if owned_targets
					.iter()
					.any(|current| current.as_str() == previous)
				{
					continue;
				}
				if !self
					.diagnostic_owners
					.get(previous)
					.is_some_and(|owner| owner == origin.as_str())
				{
					continue;
				}
				let Ok(previous_uri) = previous.parse::<Uri>() else {
					continue;
				};
				let open = docs.get(&previous_uri);
				modules.push(DiagnosticModuleSnapshot {
					uri: previous_uri.clone(),
					source: open
						.map(|document| Arc::from(document.text.as_str()))
						.or_else(|| self.effective_source_for_uri(&previous_uri))
						.unwrap_or_else(|| Arc::from("")),
					version: open.map(|document| document.version),
					diagnostics: Vec::new(),
				});
			}
		}
		DiagnosticsSnapshot {
			document_revision: docs.revision(),
			modules,
			owned_targets,
		}
	}

	pub fn record_diagnostic_targets(&mut self, origin: &Uri, targets: &[Uri]) {
		let origin = origin.as_str().to_string();
		let targets: Vec<_> = targets
			.iter()
			.map(|target| target.as_str().to_string())
			.collect();
		if let Some(previous_targets) = self
			.diagnostic_targets
			.insert(origin.clone(), targets.clone())
		{
			for previous in previous_targets {
				if !targets.contains(&previous)
					&& self
						.diagnostic_owners
						.get(&previous)
						.is_some_and(|owner| owner == &origin)
				{
					self.diagnostic_owners.remove(&previous);
				}
			}
		}
		for target in targets {
			self.diagnostic_owners.insert(target, origin.clone());
		}
	}

	#[doc(hidden)]
	#[must_use]
	pub fn source_for_uri(&self, uri: &Uri) -> Option<Arc<str>> {
		self.effective_source_for_uri(uri)
	}

	#[cfg(test)]
	pub fn synchronize_open_document(
		&mut self,
		docs: &DocumentStore,
		uri: &Uri,
	) -> anyhow::Result<()> {
		if docs.get(uri).is_some() {
			self.synchronize(docs, uri)?;
		}
		Ok(())
	}

	fn synchronize(&mut self, docs: &DocumentStore, uri: &Uri) -> anyhow::Result<()> {
		// Notification-driven discovery is tied to the first project open;
		// project-wide reference requests explicitly refresh filesystem
		// membership, while opening a file always overlays or adds its live source.
		let previous_identity = self.documents.get(uri).cloned();
		let class = match workspace::classify_uri(uri) {
			Err(error) => {
				// Retain the last valid identity as lifecycle metadata. The manifest
				// error suppresses analysis, but close still needs the project path to
				// retire the overlay and restore disk state.
				self.manifest_errors.insert(uri.clone(), error.to_string());
				return Ok(());
			}
			Ok(class) => class,
		};
		let (root, module, kind) = match class {
			workspace::UriClass::ProjectFile { path, project } => (
				project.src_root,
				ModulePath::new(project.entry_key).unwrap(),
				DocumentKind::Project(path),
			),
			workspace::UriClass::LooseFile { path } => {
				let root = path
					.parent()
					.unwrap_or_else(|| std::path::Path::new("/"))
					.to_path_buf();
				let module = path
					.file_stem()
					.and_then(|name| name.to_str())
					.unwrap_or("loose");
				let module = ModulePath::new(module).map_err(|reason| {
					anyhow::anyhow!(
						"cannot use loose source file `{}`: {reason}",
						path.display()
					)
				})?;
				(root, module, DocumentKind::Loose(path))
			}
			workspace::UriClass::NonFile => (
				PathBuf::new(),
				ModulePath::new("document").unwrap(),
				DocumentKind::NonFile,
			),
		};
		self.manifest_errors.remove(uri);
		let root = if matches!(kind, DocumentKind::NonFile) {
			root
		} else {
			std::path::absolute(root)?
		};
		// A manifest project shares one source graph at its source root. A
		// loose file is deliberately its own one-module project, even when
		// sibling loose files in the same directory are open.
		let project_key = match &kind {
			DocumentKind::Project(_) => root.clone(),
			DocumentKind::Loose(path) => path.clone(),
			DocumentKind::NonFile => PathBuf::new(),
		};
		let without_prelude = match &kind {
			DocumentKind::Project(path) => nymph_compiler::is_stdlib_source_path(path),
			DocumentKind::Loose(path) => nymph_compiler::is_stdlib_source_path(path),
			DocumentKind::NonFile => false,
		};
		let project = if matches!(kind, DocumentKind::NonFile) {
			ProjectId::new(uri.as_str().to_string())
		} else {
			self
				.workspaces
				.entry(std::path::absolute(project_key)?)
				.or_insert_with_key(|key| ProjectId::new(key.to_string_lossy().into_owned()))
				.clone()
		};
		let identity = DocumentIdentity {
			project: project.clone(),
			module: module.clone(),
			entry: module,
			root: root.clone(),
			without_prelude,
			kind: kind.clone(),
		};
		if let Some(previous) = previous_identity
			&& previous.module_identity() != identity.module_identity()
		{
			self.retire_transitioned_identity(docs, uri, &previous);
		}
		self.documents.insert(uri.clone(), identity.clone());

		if matches!(kind, DocumentKind::Project(_)) && self.synchronized_roots.insert(root.clone()) {
			self.synchronize_project_files(docs, &root, &project, without_prelude)?;
		}
		if docs.get(uri).is_some() {
			self
				.authoritative_overlays
				.insert(identity.module_identity(), uri.clone());
		}
		// Replay one explicitly authoritative overlay per logical module. The
		// current notification wins for its module; other aliases are not chosen
		// by hash-map iteration order.
		let overlays: Vec<_> = self
			.authoritative_overlays
			.iter()
			.filter(|(module, _)| module.project == project)
			.filter_map(|(module, open_uri)| {
				let document = docs.get(open_uri)?;
				(self
					.documents
					.get(open_uri)
					.is_some_and(|candidate| candidate.module_identity() == *module))
				.then(|| {
					(
						module.clone(),
						document.text.clone(),
						SourceVersion(i64::from(document.version)),
					)
				})
			})
			.collect();
		for (module, source, version) in overlays {
			self.set_effective_source(&module, source, version);
		}
		Ok(())
	}

	/// Reconcile the session's disk-backed project inputs on every analysis
	/// refresh. Open overlays remain authoritative; newly added unopened files
	/// appear and deleted unopened files are tombstoned deterministically.
	fn synchronize_project_files(
		&mut self,
		docs: &DocumentStore,
		root: &std::path::Path,
		project: &ProjectId,
		without_prelude: bool,
	) -> anyhow::Result<()> {
		let mut present = HashSet::new();
		for (path, module) in nymph_files(root) {
			let module_identity = ModuleIdentity {
				project: project.clone(),
				module: module.clone(),
				without_prelude,
			};
			present.insert(module_identity.clone());
			let disk_identity = DocumentIdentity {
				project: project.clone(),
				module: module.clone(),
				entry: module,
				root: root.to_path_buf(),
				without_prelude,
				kind: DocumentKind::Project(path.clone()),
			};
			let mut open_overlays = self
				.documents
				.iter()
				.filter_map(|(uri, identity)| {
					let DocumentKind::Project(candidate_path) = &identity.kind else {
						return None;
					};
					if candidate_path != &path {
						return None;
					}
					let document = docs.get(uri)?;
					Some((
						uri.clone(),
						document.text.clone(),
						document.version,
						identity.clone(),
					))
				})
				.collect::<Vec<_>>();
			open_overlays.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
			for (uri, _, _, previous) in &open_overlays {
				if previous.module_identity() != module_identity {
					self.retire_transitioned_identity(docs, uri, previous);
				}
				self.documents.insert(uri.clone(), disk_identity.clone());
			}
			let authoritative = self
				.authoritative_overlays
				.get(&module_identity)
				.and_then(|authoritative| {
					open_overlays
						.iter()
						.find(|(uri, _, _, _)| uri == authoritative)
				})
				.or_else(|| {
					open_overlays.iter().max_by(
						|(left_uri, _, left_version, _), (right_uri, _, right_version, _)| {
							left_version
								.cmp(right_version)
								.then_with(|| left_uri.as_str().cmp(right_uri.as_str()))
						},
					)
				})
				.map(|(uri, source, version, _)| (uri.clone(), source.clone(), *version));
			if let Some((uri, source, version)) = authoritative {
				self
					.authoritative_overlays
					.insert(module_identity.clone(), uri);
				self.set_effective_source(&module_identity, source, SourceVersion(i64::from(version)));
			} else {
				match fs::read_to_string(&path) {
					Ok(source) => {
						self.set_effective_source(&module_identity, source, SourceVersion(0));
					}
					Err(_) => self.remove_effective_source(&module_identity),
				}
			}
			if let Some(disk_uri) = workspace::path_to_uri(&path) {
				self.documents.insert(disk_uri, disk_identity);
			}
		}

		let stale = self
			.effective_sources
			.keys()
			.filter(|identity| {
				identity.project == *project
					&& identity.without_prelude == without_prelude
					&& !present.contains(*identity)
					&& !self
						.authoritative_overlays
						.get(*identity)
						.is_some_and(|uri| docs.get(uri).is_some())
			})
			.cloned()
			.collect::<Vec<_>>();
		for identity in &stale {
			self.remove_effective_source(identity);
		}
		self.documents.retain(|uri, identity| {
			docs.get(uri).is_some() || !stale.contains(&identity.module_identity())
		});
		Ok(())
	}

	fn session_for(&self, identity: &DocumentIdentity) -> &CompilerSession {
		if identity.without_prelude {
			&self.stdlib_session
		} else {
			&self.session
		}
	}

	fn session_mut(&mut self, without_prelude: bool) -> &mut CompilerSession {
		if without_prelude {
			&mut self.stdlib_session
		} else {
			&mut self.session
		}
	}
}

fn valid_source_span(source: &str, span: nymph_ast::Span) -> bool {
	span.start < span.end
		&& span.end <= source.len()
		&& source.is_char_boundary(span.start)
		&& source.is_char_boundary(span.end)
}

fn nymph_files(root: &std::path::Path) -> Vec<(PathBuf, ModulePath)> {
	fn visit(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(PathBuf, ModulePath)>) {
		if !dir.is_dir() {
			return;
		}
		if dir != root && !matches!(dir.join("nymph.toml").try_exists(), Ok(false)) {
			return;
		}
		let Ok(entries) = fs::read_dir(dir) else {
			return;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				visit(root, &path, out);
			} else if path.extension().and_then(|ext| ext.to_str()) == Some("nym")
				&& let Ok(module) = nymph_project::module_from_file(root, &path)
			{
				out.push((path, module));
			}
		}
	}
	let mut files = Vec::new();
	visit(root, root, &mut files);
	files.sort_by(|left, right| left.0.cmp(&right.0));
	files
}

pub fn publish_if_current<T>(
	docs: &DocumentStore,
	uri: &Uri,
	snapshot: &AnalysisSnapshot,
	value: T,
	send: impl FnOnce(T),
) {
	if docs.revision() == snapshot.document_revision
		&& docs.version(uri) == Some(snapshot.document_version)
	{
		send(value);
	}
}

pub fn publish_completion_if_current<T>(
	docs: &DocumentStore,
	uri: &Uri,
	snapshot: &CompletionSnapshot,
	value: T,
	send: impl FnOnce(T),
) {
	if docs.revision() == snapshot.document_revision
		&& docs.version(uri) == Some(snapshot.document_version)
	{
		send(value);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::cell::Cell;

	#[test]
	fn malformed_or_non_boundary_definition_spans_are_rejected() {
		let source = "a😀z";
		assert!(valid_source_span(source, nymph_ast::Span::new(1, 5)));
		for span in [
			nymph_ast::Span::new(5, 1),
			nymph_ast::Span::new(1, 1),
			nymph_ast::Span::new(0, source.len() + 1),
			nymph_ast::Span::new(2, 5),
			nymph_ast::Span::new(1, 4),
		] {
			assert!(!valid_source_span(source, span), "span {span:?}");
		}
	}

	#[test]
	fn snapshot_from_a_previous_same_version_lifecycle_is_not_published() {
		let uri: Uri = "file:///tmp/stale.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(&mut docs, uri.clone(), "func f(): int = 1".into(), 1)
			.unwrap();
		let snapshot = state.analysis_for_uri(&docs, &uri).unwrap();
		state.close(&mut docs, &uri).unwrap();
		state
			.open(&mut docs, uri.clone(), "func f(): boolean = true".into(), 1)
			.unwrap();
		let sent = Cell::new(false);
		publish_if_current(&docs, &uri, &snapshot, (), |_| sent.set(true));
		assert!(!sent.get());
	}

	#[cfg(unix)]
	#[test]
	fn noncanonical_loose_source_name_returns_an_error() {
		let uri: Uri = "file:///tmp/loose%3Afile.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		let error = state
			.open(&mut docs, uri, "let value = 1".into(), 1)
			.expect_err("a noncanonical loose source name must not panic or enter the session");
		assert!(error.to_string().contains("cannot use loose source file"));
	}
}
