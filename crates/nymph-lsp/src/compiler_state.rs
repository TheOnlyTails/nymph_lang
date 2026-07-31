use std::{
	collections::{HashMap, HashSet},
	fs,
	path::PathBuf,
	sync::Arc,
};

use lsp_types::Uri;
use nymph_compiler::{
	CompilerSession, ModuleAnalysis, ModulePath, ProjectDiagnostic, ProjectId, SourceVersion,
};
use nymph_sema::EntryMode;

use crate::{document_store::DocumentStore, workspace};

#[derive(Clone)]
struct DocumentIdentity {
	project: ProjectId,
	module: ModulePath,
	entry: ModulePath,
	root: PathBuf,
	without_prelude: bool,
}

pub struct DiagnosticModuleSnapshot {
	pub uri: Uri,
	pub source: Arc<str>,
	pub version: Option<i32>,
	pub diagnostics: Vec<nymph_diagnostics::Diagnostic>,
}

pub struct DiagnosticsSnapshot {
	pub requested_version: i32,
	pub modules: Vec<DiagnosticModuleSnapshot>,
}

pub struct AnalysisSnapshot {
	pub project: ProjectId,
	pub module: ModulePath,
	pub version: SourceVersion,
	pub source: Arc<str>,
	pub analysis: Arc<ModuleAnalysis>,
}

pub struct CompilerState {
	pub session: CompilerSession,
	stdlib_session: CompilerSession,
	pub workspaces: HashMap<PathBuf, ProjectId>,
	synchronized_roots: HashSet<PathBuf>,
	documents: HashMap<Uri, DocumentIdentity>,
	sources: HashMap<Uri, Arc<str>>,
	diagnostic_targets: HashMap<String, HashSet<String>>,
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
			diagnostic_targets: HashMap::new(),
		}
	}

	pub fn open(
		&mut self,
		docs: &mut DocumentStore,
		uri: Uri,
		text: String,
		version: i32,
	) -> anyhow::Result<()> {
		docs.open(uri.clone(), text, version);
		self.synchronize(docs, &uri)
	}

	pub fn change(
		&mut self,
		docs: &mut DocumentStore,
		uri: &Uri,
		text: String,
		version: i32,
	) -> anyhow::Result<()> {
		docs.change_full(uri, text, version);
		self.synchronize(docs, uri)
	}

	pub fn close(&mut self, docs: &mut DocumentStore, uri: &Uri) -> anyhow::Result<Vec<Uri>> {
		docs.close(uri);
		let Some(identity) = self.documents.remove(uri) else {
			return Ok(Vec::new());
		};
		self.diagnostic_targets.remove(uri.as_str());
		let path = workspace::uri_to_path(uri);
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
				self.sources.remove(uri);
			}
			Err(error) => return Err(error.into()),
		}
		Ok(
			self
				.documents
				.iter()
				.filter(|(open_uri, open_identity)| {
					open_identity.project == identity.project && docs.get(open_uri).is_some()
				})
				.map(|(open_uri, _)| open_uri.clone())
				.collect(),
		)
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
		Some(AnalysisSnapshot {
			project: identity.project.clone(),
			module: identity.module.clone(),
			version: SourceVersion(i64::from(document.version)),
			source: Arc::from(document.text.as_str()),
			analysis,
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

	pub fn diagnostics_snapshot(
		&mut self,
		docs: &DocumentStore,
		uri: &Uri,
	) -> Option<DiagnosticsSnapshot> {
		let requested_version = docs.version(uri)?;
		let identity = self.documents.get(uri)?;
		let session = self.session_for(identity);
		let diagnostics = session.tooling_diagnostics(
			identity.project.clone(),
			identity.entry.clone(),
			!identity.without_prelude,
		);
		let mut grouped: HashMap<String, Vec<nymph_diagnostics::Diagnostic>> = HashMap::new();
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
				keys.push(ModulePath::new(module.clone()).ok()?);
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
		let current_targets: HashSet<String> = modules
			.iter()
			.map(|module| module.uri.as_str().to_string())
			.collect();
		if let Some(previous_targets) = self.diagnostic_targets.get(uri.as_str()) {
			for previous in previous_targets.difference(&current_targets) {
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
		self
			.diagnostic_targets
			.insert(uri.as_str().to_string(), current_targets);
		Some(DiagnosticsSnapshot {
			requested_version,
			modules,
		})
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
		let path = workspace::uri_to_path(uri);
		let (root, module, scan_disk) = match workspace::detect(&path) {
			Some(project) => (
				project.src_root,
				ModulePath::new(project.entry_key).unwrap(),
				true,
			),
			None => {
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
				(root, module, false)
			}
		};
		let root = std::path::absolute(root)?;
		let without_prelude = nymph_compiler::is_stdlib_source_path(&path);
		let project = self
			.workspaces
			.entry(root.clone())
			.or_insert_with(|| ProjectId::new(root.to_string_lossy().into_owned()))
			.clone();
		let identity = DocumentIdentity {
			project: project.clone(),
			module: module.clone(),
			entry: module,
			root: root.clone(),
			without_prelude,
		};
		self.documents.insert(uri.clone(), identity);

		if scan_disk && self.synchronized_roots.insert(root.clone()) {
			for (path, module) in nymph_files(&root)? {
				if let Ok(source) = fs::read_to_string(&path) {
					self.session_mut(without_prelude).set_source(
						project.clone(),
						module,
						source.clone(),
						SourceVersion(0),
					);
					if let Some(disk_uri) = workspace::path_to_uri(&path) {
						self.sources.insert(disk_uri, source.into());
					}
				}
			}
		}
		for (open_uri, document) in docs.iter() {
			let Some(open_identity) = self.documents.get(open_uri).cloned() else {
				continue;
			};
			if open_identity.root == root {
				self.session_mut(open_identity.without_prelude).set_source(
					project.clone(),
					open_identity.module.clone(),
					document.text.clone(),
					SourceVersion(i64::from(document.version)),
				);
				self
					.sources
					.insert(open_uri.clone(), Arc::from(document.text.as_str()));
			}
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
	if docs.version(uri) == i32::try_from(snapshot.version.0).ok() {
		send(value);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::cell::Cell;

	#[test]
	fn stale_snapshot_is_not_published() {
		let uri: Uri = "file:///tmp/stale.nym".parse().unwrap();
		let mut docs = DocumentStore::default();
		let mut state = CompilerState::new();
		state
			.open(&mut docs, uri.clone(), "func f(): int = 1".into(), 1)
			.unwrap();
		let snapshot = state.analysis_for_uri(&docs, &uri).unwrap();
		docs.change_full(&uri, "func f(): int = 2".into(), 2);
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
