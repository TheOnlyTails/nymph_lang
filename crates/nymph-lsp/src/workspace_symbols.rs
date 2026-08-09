//! `workspace/symbol`: deterministic search over one immutable compiler-state
//! snapshot of synchronized manifest projects.
//!
//! Visible top-level project declarations are ranked in three case-sensitive
//! tiers: exact, prefix, then fuzzy. Fuzzy ranking uses Jaro-Winkler similarity
//! (descending, minimum 0.70). Every tie is resolved by URI, start/end range,
//! then name. Non-empty searches return at most 100 results; an empty query is
//! a deterministic project overview bounded to 50 results.

use std::cmp::Ordering;

use lsp_types::{Location, SymbolInformation, WorkspaceSymbolParams, WorkspaceSymbolResponse};
use nymph_sema::NamespaceVisibility;

use crate::{
	compiler_state::WorkspaceSymbolSnapshot, document_symbols::symbol_kind, line_index::LineIndex,
};

pub const MAX_RESULTS: usize = 100;
pub const MAX_OVERVIEW_RESULTS: usize = 50;
const MIN_FUZZY_SCORE: f64 = 0.70;

#[derive(Clone, Copy, Debug)]
enum MatchRank {
	Overview,
	Exact,
	Prefix,
	Fuzzy(f64),
}

struct RankedSymbol {
	rank: MatchRank,
	symbol: SymbolInformation,
}

/// Search declarations from exactly one project/overlay revision.
#[must_use]
pub fn workspace_symbols(
	snapshot: &WorkspaceSymbolSnapshot,
	params: &WorkspaceSymbolParams,
) -> Option<WorkspaceSymbolResponse> {
	let query = params.query.as_str();
	let mut ranked = Vec::new();
	for module in &snapshot.modules {
		let index = LineIndex::new(&module.source);
		for declaration in module
			.declarations
			.iter()
			.filter(|declaration| declaration.visibility == NamespaceVisibility::Importable)
		{
			let name = declaration.name.as_str();
			let rank = if query.is_empty() {
				MatchRank::Overview
			} else if name == query {
				MatchRank::Exact
			} else if name.starts_with(query) {
				MatchRank::Prefix
			} else {
				let score = strsim::jaro_winkler(name, query);
				if score < MIN_FUZZY_SCORE {
					continue;
				}
				MatchRank::Fuzzy(score)
			};
			if !valid_span(&module.source, declaration.name_span) {
				continue;
			}
			let location = Location {
				uri: module.uri.clone(),
				range: index.range(&module.source, declaration.name_span),
			};
			#[allow(deprecated)]
			let symbol = SymbolInformation {
				name: name.to_string(),
				kind: symbol_kind(declaration.category, declaration.mutable),
				tags: None,
				deprecated: None,
				location,
				container_name: Some(module.module.as_str().to_string()),
			};
			ranked.push(RankedSymbol { rank, symbol });
		}
	}

	ranked.sort_by(compare_ranked);
	let limit = if query.is_empty() {
		MAX_OVERVIEW_RESULTS
	} else {
		MAX_RESULTS
	};
	Some(WorkspaceSymbolResponse::Flat(
		ranked
			.into_iter()
			.take(limit)
			.map(|candidate| candidate.symbol)
			.collect(),
	))
}

fn compare_ranked(left: &RankedSymbol, right: &RankedSymbol) -> Ordering {
	match (left.rank, right.rank) {
		(MatchRank::Exact, MatchRank::Exact)
		| (MatchRank::Prefix, MatchRank::Prefix)
		| (MatchRank::Overview, MatchRank::Overview) => tie_break(&left.symbol, &right.symbol),
		(MatchRank::Fuzzy(left_score), MatchRank::Fuzzy(right_score)) => right_score
			.total_cmp(&left_score)
			.then_with(|| tie_break(&left.symbol, &right.symbol)),
		(left, right) => tier(left).cmp(&tier(right)),
	}
}

fn tier(rank: MatchRank) -> u8 {
	match rank {
		MatchRank::Overview | MatchRank::Exact => 0,
		MatchRank::Prefix => 1,
		MatchRank::Fuzzy(_) => 2,
	}
}

fn tie_break(left: &SymbolInformation, right: &SymbolInformation) -> Ordering {
	left
		.location
		.uri
		.as_str()
		.cmp(right.location.uri.as_str())
		.then_with(|| {
			left
				.location
				.range
				.start
				.line
				.cmp(&right.location.range.start.line)
		})
		.then_with(|| {
			left
				.location
				.range
				.start
				.character
				.cmp(&right.location.range.start.character)
		})
		.then_with(|| {
			left
				.location
				.range
				.end
				.line
				.cmp(&right.location.range.end.line)
		})
		.then_with(|| {
			left
				.location
				.range
				.end
				.character
				.cmp(&right.location.range.end.character)
		})
		.then_with(|| left.name.cmp(&right.name))
}

fn valid_span(source: &str, span: nymph_ast::Span) -> bool {
	span.start < span.end
		&& span.end <= source.len()
		&& source.is_char_boundary(span.start)
		&& source.is_char_boundary(span.end)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		compiler_state::CompilerState, document_store::DocumentStore, workspace::path_to_uri,
	};
	use lsp_types::{PartialResultParams, WorkDoneProgressParams};

	fn params(query: &str) -> WorkspaceSymbolParams {
		WorkspaceSymbolParams {
			query: query.to_string(),
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: PartialResultParams::default(),
		}
	}

	fn flat(response: Option<WorkspaceSymbolResponse>) -> Vec<SymbolInformation> {
		match response.unwrap() {
			WorkspaceSymbolResponse::Flat(symbols) => symbols,
			WorkspaceSymbolResponse::Nested(_) => panic!("expected flat workspace symbols"),
		}
	}

	struct Fixture {
		_temp: tempfile::TempDir,
		main_path: std::path::PathBuf,
		main_uri: lsp_types::Uri,
		docs: DocumentStore,
		state: CompilerState,
	}

	impl Fixture {
		fn new(files: &[(&str, &str)]) -> Self {
			let temp = tempfile::tempdir().unwrap();
			std::fs::write(
				temp.path().join("nymph.toml"),
				"[package]\nname='workspace-symbols'\nversion='0.1.0'\n",
			)
			.unwrap();
			let src = temp.path().join("src");
			std::fs::create_dir(&src).unwrap();
			for (module, source) in files {
				let path = src.join(format!("{module}.nym"));
				if let Some(parent) = path.parent() {
					std::fs::create_dir_all(parent).unwrap();
				}
				std::fs::write(path, source).unwrap();
			}
			let main_path = src.join("main.nym");
			if !main_path.exists() {
				std::fs::write(&main_path, "public func main(): void = {}").unwrap();
			}
			let main_uri = path_to_uri(&main_path).unwrap();
			let text = std::fs::read_to_string(&main_path).unwrap();
			let mut docs = DocumentStore::default();
			let mut state = CompilerState::new();
			state.open(&mut docs, main_uri.clone(), text, 1).unwrap();
			Self {
				_temp: temp,
				main_path,
				main_uri,
				docs,
				state,
			}
		}

		fn search(&mut self, query: &str) -> Vec<SymbolInformation> {
			self.state.refresh_workspace_symbols(&self.docs);
			flat(workspace_symbols(
				&self.state.workspace_symbol_snapshot(&self.docs),
				&params(query),
			))
		}
	}

	#[test]
	fn ranks_exact_then_prefix_then_fuzzy_with_stable_location_ties() {
		let mut fixture = Fixture::new(&[
			(
				"main",
				"public func map(): void = {}\npublic func mapper(): void = {}",
			),
			(
				"a",
				"public func map(): void = {}\npublic func mop(): void = {}",
			),
		]);
		let symbols = fixture.search("map");
		assert_eq!(
			symbols
				.iter()
				.map(|symbol| symbol.name.as_str())
				.collect::<Vec<_>>(),
			["map", "map", "mapper", "mop"]
		);
		assert!(symbols[0].location.uri.as_str() < symbols[1].location.uri.as_str());
	}

	#[test]
	fn includes_unopened_modules_and_semantic_categories_but_filters_private() {
		let mut fixture = Fixture::new(&[
			("main", "private func hidden(): void = {}"),
			(
				"nested/types",
				"public struct Point()\ninternal let mut counter = 0\ntype Alias = int",
			),
		]);
		let symbols = fixture.search("");
		assert!(!symbols.iter().any(|symbol| symbol.name == "hidden"));
		let point = symbols
			.iter()
			.find(|symbol| symbol.name == "Point")
			.unwrap();
		assert_eq!(point.kind, lsp_types::SymbolKind::STRUCT);
		assert_eq!(point.container_name.as_deref(), Some("nested/types"));
		assert!(point.location.uri.as_str().ends_with("/nested/types.nym"));
		assert_eq!(
			symbols
				.iter()
				.find(|symbol| symbol.name == "counter")
				.unwrap()
				.kind,
			lsp_types::SymbolKind::VARIABLE
		);
		assert_eq!(
			symbols
				.iter()
				.find(|symbol| symbol.name == "Alias")
				.unwrap()
				.kind,
			lsp_types::SymbolKind::CLASS
		);
	}

	#[test]
	fn semantic_duplicate_winner_controls_visibility_kind_and_range() {
		let mut fixture = Fixture::new(&[(
			"main",
			"public func shadowed(): void = {}\nprivate struct shadowed()\npublic func winner(): void = {}\npublic struct winner()",
		)]);

		assert!(fixture.search("shadowed").is_empty());
		let symbols = fixture.search("winner");
		assert_eq!(symbols.len(), 1);
		assert_eq!(symbols[0].name, "winner");
		assert_eq!(symbols[0].kind, lsp_types::SymbolKind::STRUCT);
		assert_eq!(symbols[0].location.range.start.line, 3);
		assert_eq!(symbols[0].location.range.start.character, 14);
		assert_eq!(symbols[0].location.range.end.character, 20);
	}

	#[test]
	fn authoritative_overlay_replaces_disk_and_close_restores_it() {
		let mut fixture = Fixture::new(&[
			("main", "public func main(): void = {}"),
			("dep", "public func disk_name(): void = {}"),
		]);
		let dep_uri = path_to_uri(&fixture.main_path.with_file_name("dep.nym")).unwrap();
		fixture
			.state
			.open(
				&mut fixture.docs,
				dep_uri.clone(),
				"public func overlay_name(): void = {}".to_string(),
				2,
			)
			.unwrap();
		assert!(
			fixture
				.search("overlay_name")
				.iter()
				.any(|s| s.name == "overlay_name")
		);
		assert!(fixture.search("disk_name").is_empty());
		fixture.state.close(&mut fixture.docs, &dep_uri).unwrap();
		assert!(
			fixture
				.search("disk_name")
				.iter()
				.any(|s| s.name == "disk_name")
		);
		assert!(fixture.search("overlay_name").is_empty());
	}

	#[test]
	fn deleted_unopened_module_disappears_after_analysis_refresh() {
		let mut fixture = Fixture::new(&[
			("main", "public func main(): void = {}"),
			("stale", "public func removed_name(): void = {}"),
		]);
		assert!(
			fixture
				.search("removed_name")
				.iter()
				.any(|s| s.name == "removed_name")
		);
		std::fs::remove_file(fixture.main_path.with_file_name("stale.nym")).unwrap();
		fixture
			.state
			.change(
				&mut fixture.docs,
				&fixture.main_uri,
				"public func main(): void = {}".to_string(),
				2,
			)
			.unwrap();
		assert!(fixture.search("removed_name").is_empty());
	}

	#[test]
	fn added_unopened_module_appears_after_analysis_refresh() {
		let mut fixture = Fixture::new(&[("main", "public func main(): void = {}")]);
		std::fs::write(
			fixture.main_path.with_file_name("added.nym"),
			"public func added_name(): void = {}",
		)
		.unwrap();
		fixture
			.state
			.change(
				&mut fixture.docs,
				&fixture.main_uri,
				"public func main(): void = {}".to_string(),
				2,
			)
			.unwrap();
		assert!(
			fixture
				.search("added_name")
				.iter()
				.any(|s| s.name == "added_name")
		);
	}

	#[test]
	fn unreadable_unopened_source_retires_stale_symbols_and_recovers() {
		let mut fixture = Fixture::new(&[
			("main", "public func main(): void = {}"),
			("dep", "public func stale_name(): void = {}"),
		]);
		let dep_path = fixture.main_path.with_file_name("dep.nym");
		assert_eq!(fixture.search("stale_name").len(), 1);

		std::fs::write(&dep_path, [0xff]).unwrap();
		assert!(fixture.search("stale_name").is_empty());

		std::fs::write(&dep_path, "public func fresh_name(): void = {}").unwrap();
		assert!(
			fixture
				.search("stale_name")
				.iter()
				.all(|symbol| symbol.name != "stale_name")
		);
		assert_eq!(fixture.search("fresh_name")[0].name, "fresh_name");
	}

	#[test]
	fn closing_an_overlay_over_unreadable_disk_retires_the_overlay() {
		let mut fixture = Fixture::new(&[
			("main", "public func main(): void = {}"),
			("dep", "public func disk_name(): void = {}"),
		]);
		let dep_path = fixture.main_path.with_file_name("dep.nym");
		let dep_uri = path_to_uri(&dep_path).unwrap();
		fixture
			.state
			.open(
				&mut fixture.docs,
				dep_uri.clone(),
				"public func overlay_name(): void = {}".to_string(),
				2,
			)
			.unwrap();
		std::fs::write(dep_path, [0xff]).unwrap();

		fixture.state.close(&mut fixture.docs, &dep_uri).unwrap();
		let snapshot = fixture.state.workspace_symbol_snapshot(&fixture.docs);
		assert!(flat(workspace_symbols(&snapshot, &params("overlay_name"))).is_empty());
		assert!(flat(workspace_symbols(&snapshot, &params("disk_name"))).is_empty());
	}

	#[test]
	fn manifest_errors_suppress_stale_project_symbols() {
		let mut fixture = Fixture::new(&[("main", "public func before_error(): void = {}")]);
		std::fs::write(fixture._temp.path().join("nymph.toml"), "not = [valid").unwrap();
		fixture
			.state
			.change(
				&mut fixture.docs,
				&fixture.main_uri,
				"public func after_error(): void = {}".to_string(),
				2,
			)
			.unwrap();
		assert!(fixture.search("").is_empty());
	}

	#[test]
	fn source_root_transition_replaces_historical_module_identities() {
		let mut fixture = Fixture::new(&[
			("main", "public func root_symbol(): void = {}"),
			("dep", "public func dep_symbol(): void = {}"),
		]);
		std::fs::write(
			fixture._temp.path().join("nymph.toml"),
			"[package]\nname='workspace-symbols'\nversion='0.1.0'\nsrc='.'\n",
		)
		.unwrap();
		fixture
			.state
			.change(
				&mut fixture.docs,
				&fixture.main_uri,
				"public func root_symbol(): void = {}".to_string(),
				2,
			)
			.unwrap();
		let symbols = fixture.search("");
		assert_eq!(symbols.len(), 2);
		assert_eq!(
			symbols
				.iter()
				.map(|symbol| symbol.container_name.as_deref().unwrap())
				.collect::<Vec<_>>(),
			["src/dep", "src/main"]
		);
	}

	#[test]
	fn source_root_transitions_rekey_every_open_overlay_without_history_leaks() {
		let mut fixture = Fixture::new(&[
			("main", "public func disk_main(): void = {}"),
			("dep", "public func disk_dep(): void = {}"),
		]);
		let dep_uri = path_to_uri(&fixture.main_path.with_file_name("dep.nym")).unwrap();
		let dep_alias: lsp_types::Uri = dep_uri
			.as_str()
			.replace("dep.nym", "%64ep.nym")
			.parse()
			.unwrap();
		assert_ne!(dep_alias, dep_uri);
		fixture
			.state
			.open(
				&mut fixture.docs,
				dep_alias,
				"public func overlay_dep(): void = {}".to_string(),
				2,
			)
			.unwrap();

		std::fs::write(
			fixture._temp.path().join("nymph.toml"),
			"[package]\nname='workspace-symbols'\nversion='0.1.0'\nsrc='.'\n",
		)
		.unwrap();
		fixture
			.state
			.change(
				&mut fixture.docs,
				&fixture.main_uri,
				"public func overlay_main(): void = {}".to_string(),
				2,
			)
			.unwrap();
		let symbols = fixture.search("");
		assert_eq!(
			symbols
				.iter()
				.map(|symbol| (
					symbol.name.as_str(),
					symbol.container_name.as_deref().unwrap(),
				))
				.collect::<Vec<_>>(),
			[("overlay_dep", "src/dep"), ("overlay_main", "src/main")]
		);

		std::fs::write(
			fixture._temp.path().join("nymph.toml"),
			"[package]\nname='workspace-symbols'\nversion='0.1.0'\n",
		)
		.unwrap();
		fixture
			.state
			.change(
				&mut fixture.docs,
				&fixture.main_uri,
				"public func overlay_main(): void = {}".to_string(),
				3,
			)
			.unwrap();
		let symbols = fixture.search("");
		assert_eq!(
			symbols
				.iter()
				.map(|symbol| (
					symbol.name.as_str(),
					symbol.container_name.as_deref().unwrap(),
				))
				.collect::<Vec<_>>(),
			[("overlay_dep", "dep"), ("overlay_main", "main")]
		);
	}

	#[test]
	fn missing_recovered_names_never_become_symbols() {
		let mut fixture = Fixture::new(&[("main", "public func (\npublic struct (")]);
		assert!(fixture.search("").is_empty());
	}

	#[test]
	fn malformed_modules_are_safe_and_empty_overview_is_bounded() {
		let mut files = vec![(
			"main".to_string(),
			"public func main(): void = {}".to_string(),
		)];
		for index in 0..(MAX_RESULTS + 10) {
			files.push((
				format!("module_{index:03}"),
				format!("public func item_{index:03}(): void = {{}}"),
			));
		}
		files.push((
			"broken".to_string(),
			"public func recovered(): void = {}\npublic func broken(".to_string(),
		));
		let refs = files
			.iter()
			.map(|(module, source)| (module.as_str(), source.as_str()))
			.collect::<Vec<_>>();
		let mut fixture = Fixture::new(&refs);
		assert_eq!(fixture.search("").len(), MAX_OVERVIEW_RESULTS);
		assert_eq!(fixture.search("item_").len(), MAX_RESULTS);
		assert!(
			fixture
				.search("recovered")
				.iter()
				.any(|s| s.name == "recovered")
		);
	}
}
