use std::{
	collections::{BTreeMap, HashMap, HashSet},
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
	pub project: ProjectId,
	pub module: ModulePath,
	/// Client sequence number used only to suppress responses after a newer
	/// notification. It is protocol metadata, not an analysis-cache key.
	pub document_version: i32,
	/// Identity of the complete open-document overlay state that produced this
	/// analysis, including dependency overlays and document lifecycles.
	pub document_revision: DocumentStoreRevision,
	pub source: Arc<str>,
	pub analysis: Arc<ModuleAnalysis>,
	entry: ModulePath,
	root: PathBuf,
	without_prelude: bool,
}

pub struct DefinitionTargetSnapshot {
	pub uri: Uri,
	pub source: Arc<str>,
	pub span: nymph_ast::Span,
	pub requires_disk_validation: bool,
}

pub struct CompilerState {
	pub session: CompilerSession,
	stdlib_session: CompilerSession,
	pub workspaces: HashMap<PathBuf, ProjectId>,
	synchronized_roots: HashSet<PathBuf>,
	documents: HashMap<Uri, DocumentIdentity>,
	sources: HashMap<Uri, Arc<str>>,
	manifest_errors: HashMap<Uri, String>,
	diagnostic_targets: HashMap<String, Vec<String>>,
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
			sources: HashMap::new(),
			manifest_errors: HashMap::new(),
			diagnostic_targets: HashMap::new(),
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
		Ok(self.affected_project_documents(&uri))
	}

	pub fn change(
		&mut self,
		docs: &mut DocumentStore,
		uri: &Uri,
		text: String,
		version: i32,
	) -> anyhow::Result<Vec<Uri>> {
		docs.change_full(uri, text, version);
		self.synchronize(docs, uri)?;
		Ok(self.affected_project_documents(uri))
	}

	pub fn close(&mut self, docs: &mut DocumentStore, uri: &Uri) -> anyhow::Result<CloseAction> {
		let previous = self.affected_project_documents(uri);
		// Remove the overlay before choosing the effective replacement source.
		docs.close(uri);
		self.manifest_errors.remove(uri);
		let Some(identity) = self.documents.get(uri).cloned() else {
			return Ok(CloseAction::Clear);
		};
		self.diagnostic_targets.remove(uri.as_str());
		self.sources.remove(uri);
		if !matches!(identity.kind, DocumentKind::Project(_)) {
			self.documents.remove(uri);
		}
		let remaining_overlay = self
			.documents
			.iter()
			.filter(|(_, candidate)| {
				candidate.project == identity.project
					&& candidate.module == identity.module
					&& candidate.without_prelude == identity.without_prelude
			})
			.filter_map(|(open_uri, _)| {
				docs
					.get(open_uri)
					.map(|document| (open_uri.clone(), document.text.clone(), document.version))
			})
			.max_by(
				|(left_uri, _, left_version), (right_uri, _, right_version)| {
					left_version
						.cmp(right_version)
						.then_with(|| left_uri.as_str().cmp(right_uri.as_str()))
				},
			);
		if let Some((open_uri, source, version)) = remaining_overlay {
			self.session_mut(identity.without_prelude).set_source(
				identity.project.clone(),
				identity.module.clone(),
				source.clone(),
				SourceVersion(i64::from(version)),
			);
			self.sources.insert(open_uri.clone(), source.clone().into());
			if !matches!(identity.kind, DocumentKind::Project(_)) {
				return Ok(CloseAction::Clear);
			}
			// The closed spelling remains a valid project publication target even
			// when an equivalent URI spelling still supplies the live overlay.
			self.sources.insert(uri.clone(), source.into());
			let mut affected = previous;
			for current in self.affected_project_documents(uri) {
				if !affected.contains(&current) {
					affected.push(current);
				}
			}
			return Ok(CloseAction::PublishProject(affected));
		}
		let DocumentKind::Project(path) = identity.kind else {
			self
				.session_mut(identity.without_prelude)
				.remove_source(identity.project, identity.module);
			return Ok(CloseAction::Clear);
		};
		match fs::read_to_string(path) {
			Ok(source) => {
				self.session_mut(identity.without_prelude).set_source(
					identity.project.clone(),
					identity.module.clone(),
					source.clone(),
					SourceVersion(0),
				);
				self.sources.insert(uri.clone(), source.into());
			}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
				self
					.session_mut(identity.without_prelude)
					.remove_source(identity.project.clone(), identity.module.clone());
			}
			Err(error) => return Err(error.into()),
		}
		let mut affected = previous;
		for current in self.affected_project_documents(uri) {
			if !affected.contains(&current) {
				affected.push(current);
			}
		}
		Ok(CloseAction::PublishProject(affected))
	}

	fn affected_project_documents(&self, uri: &Uri) -> Vec<Uri> {
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
					.or_else(|| workspace::key_to_uri(&identity.root, module.as_str()))
			})
			.collect()
	}

	pub fn analysis_for_uri(&self, docs: &DocumentStore, uri: &Uri) -> Option<AnalysisSnapshot> {
		let document = docs.get(uri)?;
		let identity = self.documents.get(uri)?;
		let analysis = self.session_for(identity).tooling_analyze_module(
			identity.project.clone(),
			identity.entry.clone(),
			identity.module.clone(),
			!identity.without_prelude,
		)?;
		let source = analysis.source.clone();
		Some(AnalysisSnapshot {
			project: identity.project.clone(),
			module: identity.module.clone(),
			document_version: document.version,
			document_revision: docs.revision(),
			source,
			analysis,
			entry: identity.entry.clone(),
			root: identity.root.clone(),
			without_prelude: identity.without_prelude,
		})
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

	pub fn diagnostics_for_uri(
		&self,
		docs: &DocumentStore,
		uri: &Uri,
	) -> Option<Arc<[ProjectDiagnostic]>> {
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
					workspace::key_to_uri(&identity.root, module.as_str())?
				};
				let open = docs.get(&module_uri);
				let source = open
					.map(|document| Arc::from(document.text.as_str()))
					.or_else(|| self.sources.get(&module_uri).cloned())?;
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
				let Ok(previous_uri) = previous.parse::<Uri>() else {
					continue;
				};
				let open = docs.get(&previous_uri);
				modules.push(DiagnosticModuleSnapshot {
					uri: previous_uri.clone(),
					source: open
						.map(|document| Arc::from(document.text.as_str()))
						.or_else(|| self.sources.get(&previous_uri).cloned())
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
				let source = open
					.map(|document| Arc::from(document.text.as_str()))
					.or_else(|| self.sources.get(uri).cloned())
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
		let owned_targets: Vec<Uri> = modules.iter().map(|module| module.uri.clone()).collect();
		if let Some(previous_targets) = self.diagnostic_targets.get(origin.as_str()) {
			for previous in previous_targets {
				if owned_targets
					.iter()
					.any(|current| current.as_str() == previous)
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
						.or_else(|| self.sources.get(&previous_uri).cloned())
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
		self.diagnostic_targets.insert(
			origin.as_str().to_string(),
			targets
				.iter()
				.map(|target| target.as_str().to_string())
				.collect(),
		);
	}

	#[doc(hidden)]
	#[must_use]
	pub fn source_for_uri(&self, uri: &Uri) -> Option<Arc<str>> {
		self.sources.get(uri).cloned()
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
		// Filesystem discovery is intentionally tied to the first project open.
		// Without a watcher, later external adds/deletes of unopened files are
		// not events; opening a file still overlays or adds its live source.
		let class = match workspace::classify_uri(uri) {
			Err(error) => {
				self.documents.remove(uri);
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
		self.documents.insert(uri.clone(), identity.clone());

		if matches!(kind, DocumentKind::Project(_)) && self.synchronized_roots.insert(root.clone()) {
			for (path, module) in nymph_files(&root)? {
				if let Ok(source) = fs::read_to_string(&path) {
					self.session_mut(without_prelude).set_source(
						project.clone(),
						module.clone(),
						source.clone(),
						SourceVersion(0),
					);
					if let Some(disk_uri) = workspace::path_to_uri(&path) {
						self.sources.insert(disk_uri.clone(), source.into());
						self
							.documents
							.entry(disk_uri)
							.or_insert_with(|| DocumentIdentity {
								project: project.clone(),
								module: module.clone(),
								entry: module,
								root: root.clone(),
								without_prelude,
								kind: DocumentKind::Project(path.clone()),
							});
					}
				}
			}
		}
		for (open_uri, document) in docs.iter() {
			let Some(open_identity) = self.documents.get(open_uri).cloned() else {
				continue;
			};
			if open_identity.project == project {
				self.session_mut(open_identity.without_prelude).set_source(
					open_identity.project,
					open_identity.module.clone(),
					document.text.clone(),
					SourceVersion(i64::from(document.version)),
				);
				self
					.sources
					.insert(open_uri.clone(), Arc::from(document.text.as_str()));
			}
		}
		// Equivalent URI spellings can name the same module. The notification
		// currently being synchronized is authoritative regardless of hash-map
		// iteration order above; closing it later restores another live overlay.
		if let Some(document) = docs.get(uri) {
			self.session_mut(identity.without_prelude).set_source(
				project,
				identity.module,
				document.text.clone(),
				SourceVersion(i64::from(document.version)),
			);
			self
				.sources
				.insert(uri.clone(), Arc::from(document.text.as_str()));
		}
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

fn nymph_files(root: &std::path::Path) -> anyhow::Result<Vec<(PathBuf, ModulePath)>> {
	fn visit(
		root: &std::path::Path,
		dir: &std::path::Path,
		out: &mut Vec<(PathBuf, ModulePath)>,
	) -> anyhow::Result<()> {
		if !dir.is_dir() {
			return Ok(());
		}
		for entry in fs::read_dir(dir)? {
			let path = entry?.path();
			if path.is_dir() {
				visit(root, &path, out)?;
			} else if path.extension().and_then(|ext| ext.to_str()) == Some("nym")
				&& let Ok(module) = nymph_project::module_from_file(root, &path)
			{
				out.push((path, module));
			}
		}
		Ok(())
	}
	let mut files = Vec::new();
	visit(root, root, &mut files)?;
	Ok(files)
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
