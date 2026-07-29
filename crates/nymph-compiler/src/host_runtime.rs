//! The `HostRuntimeGraph` for every LINKED external (Gap 3, L0/L1) —
//! the runtime-source counterpart of `prelude.rs`'s embedded `.nym` checker
//! prelude, but for the real `.ts`/JS implementation a linked external's
//! emitted call actually resolves against at bundle time.
//!
//! `nymph_hir::linkage::REGISTRY` (a leaf-crate table both the sema gate and
//! codegen emit already consult) decides WHICH `external(name)` markers link,
//! and to which module specifier + exported symbol. It does NOT embed any
//! `.ts` SOURCE itself — a leaf crate (deps: `ecow`, `rustc-hash` only) must
//! not `include_str!` the stdlib tree. This module is the other half: it
//! supplies the actual embedded `.ts` source for each distinct module the
//! registry names, and strips + filters it (via `nymph_codegen::strip_ts_to_js`)
//! into the virtual sources [`Driver::compile_all`] injects into the bundle
//! graph alongside every real project/std module.
//!
//! L1 extension (the Option ABI seam): `list.ts`'s `get`/`first`/`last`/`pop`
//! all return `Option<T>`, built by calling the SAME `Option` their `.ts`
//! source imports (`import { Option } from "../option"`). Two things make
//! that import resolve to the compiler's canonical, source-derived Option:
//! 1. `IMPORT_REWRITES` tells `strip_ts_to_js` to keep that import (when a
//!    kept export still references it) and rewrite its specifier to the bare
//!    virtual key `"std/option"` — a real sources-map key `bundle::
//!    VirtualFsPlugin` can resolve (unlike the raw relative `"../option"`,
//!    which it can't — `resolve_id` only matches exact specifier strings).
//! 2. `Driver::compile_all` emits the demanded `option.nym` implementation
//!    once under that key. Intrinsics and source consumers therefore share
//!    both global variant tags and the same method-bearing prototype.

use std::sync::OnceLock;

use nymph_hir::hir::MarshalKind;
use rustc_hash::{FxHashMap, FxHashSet};

/// One registry MODULE specifier's `include_str!`-embedded `.ts` source —
/// mirrors `prelude.rs`'s `CORE_SOURCES` table one level down (runtime JS,
/// not checker-facing Nymph source). Add an entry here whenever
/// `nymph_hir::linkage::REGISTRY` gains a module this table doesn't cover yet
/// — `intrinsic_module_sources` panics loudly (never silently skips) if one
/// is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceProvider {
	EmbeddedTs(&'static str),
	GeneratedBox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CompilerRuntimeRole {
	Option,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DependencyTarget {
	HostModule(&'static str),
	CompilerRuntimeRole(CompilerRuntimeRole),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportDependency {
	source: &'static str,
	destination: &'static str,
	target: DependencyTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostModuleDescriptor {
	module: &'static str,
	provider: SourceProvider,
	dependencies: &'static [ImportDependency],
}

const BOX: ImportDependency = ImportDependency {
	source: "std/box",
	destination: "std/box",
	target: DependencyTarget::HostModule("std/box"),
};
const OPTION: ImportDependency = ImportDependency {
	source: "std/option",
	destination: "std/option",
	target: DependencyTarget::CompilerRuntimeRole(CompilerRuntimeRole::Option),
};
const DISPLAY: ImportDependency = ImportDependency {
	source: "./display",
	destination: "std/display",
	target: DependencyTarget::HostModule("std/display"),
};

const HOST_MODULES: &[HostModuleDescriptor] = &[
	HostModuleDescriptor {
		module: "std/display",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/display.ts")),
		dependencies: &[BOX],
	},
	HostModuleDescriptor {
		module: "std/equality",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/ops/equality.ts")),
		dependencies: &[BOX],
	},
	HostModuleDescriptor {
		module: "std/comparison",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/ops/comparison.ts")),
		dependencies: &[BOX],
	},
	HostModuleDescriptor {
		module: "std/hash",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/hash.ts")),
		dependencies: &[BOX],
	},
	HostModuleDescriptor {
		module: "std/collections/list",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/collections/list.ts")),
		dependencies: &[BOX, OPTION],
	},
	HostModuleDescriptor {
		module: "std/collections/map",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/collections/map.ts")),
		dependencies: &[BOX, OPTION],
	},
	HostModuleDescriptor {
		module: "std/io",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/io.ts")),
		dependencies: &[DISPLAY],
	},
	HostModuleDescriptor {
		module: "std/math/intrinsics",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/math/mod.ts")),
		dependencies: &[],
	},
	HostModuleDescriptor {
		module: "std/string",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/string.ts")),
		dependencies: &[BOX, OPTION],
	},
	HostModuleDescriptor {
		module: "std/box",
		provider: SourceProvider::GeneratedBox,
		dependencies: &[],
	},
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
	Callable,
	Value(MarshalKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostExport {
	marker: &'static str,
	module: &'static str,
	symbol: &'static str,
	receiver_tag: Option<&'static str>,
	delivery: Delivery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostRuntimeGraphError {
	DuplicateModuleDescriptor {
		module: &'static str,
	},
	DuplicateAbiSelector {
		marker: &'static str,
		receiver_tag: Option<&'static str>,
	},
	ConflictingAbiSelector {
		marker: &'static str,
	},
	ConflictingAbiTarget {
		module: &'static str,
		symbol: &'static str,
	},
	DuplicateDependencySource {
		module: &'static str,
		source: &'static str,
	},
	MissingDependencySourceSpelling {
		module: &'static str,
		source: &'static str,
	},
	DuplicateSourceImport {
		module: &'static str,
		source: String,
	},
	UnsupportedSourceImport {
		module: &'static str,
		source: String,
	},
	UndeclaredSourceImport {
		module: &'static str,
		source: String,
	},
	MissingSourceExport {
		module: &'static str,
		symbol: &'static str,
	},
	DuplicateSourceExport {
		module: &'static str,
		symbol: &'static str,
	},
	MissingSourceProvider {
		module: &'static str,
	},
	DependencyTargetMismatch {
		module: &'static str,
		destination: &'static str,
	},
	UnownedLinkageModule {
		module: &'static str,
	},
}

pub(crate) struct HostRuntimeGraph {
	descriptors: FxHashMap<&'static str, HostModuleDescriptor>,
	exports: FxHashMap<(&'static str, &'static str), Delivery>,
}

static HOST_RUNTIME_GRAPH: OnceLock<HostRuntimeGraph> = OnceLock::new();

/// Build the virtual module sources every LINKED external's registry module
/// needs: for each distinct module `nymph_hir::linkage::modules()` names, its
/// embedded `.ts` source stripped of TypeScript syntax and FILTERED down to
/// only the symbols that module actually links (never the full file — see
/// `nymph_codegen::strip_ts_to_js`'s doc comment for why injecting the whole
/// file is fatal to bundling: an unrelated, still-unlinked `import` inside it
/// would be a dangling specifier rolldown resolves eagerly, before
/// tree-shaking ever gets a chance to drop it). The project driver separately
/// supplies the canonical `std/option` module referenced by rewritten imports.
///
/// Keyed by the SAME module specifier the registry names (e.g.
/// `"std/collections/list"`) — the specifier an emitted `import { .. } from
/// ".."` line names, and what `bundle::VirtualFsPlugin` resolves module
/// sources against. Callers merge this into the driver's own
/// `module_sources` map before bundling. `VirtualFsPlugin` only loads a source
/// when something imports it, and rolldown tree-shakes unreferenced entries.
impl HostRuntimeGraph {
	pub(crate) fn compiler_facts() -> &'static Self {
		HOST_RUNTIME_GRAPH.get_or_init(|| {
			let exports = linkage_exports();
			Self::build(HOST_MODULES, &exports)
				.unwrap_or_else(|error| panic!("invalid compiler host runtime graph: {error:?}"))
		})
	}

	fn build(
		descriptors: &[HostModuleDescriptor],
		exports: &[HostExport],
	) -> Result<Self, HostRuntimeGraphError> {
		let mut by_module = FxHashMap::default();
		for descriptor in descriptors {
			if by_module.insert(descriptor.module, *descriptor).is_some() {
				return Err(HostRuntimeGraphError::DuplicateModuleDescriptor {
					module: descriptor.module,
				});
			}
			let mut sources = FxHashSet::default();
			for dependency in descriptor.dependencies {
				if !sources.insert(dependency.source) {
					return Err(HostRuntimeGraphError::DuplicateDependencySource {
						module: descriptor.module,
						source: dependency.source,
					});
				}
			}
			if let SourceProvider::EmbeddedTs(source) = descriptor.provider {
				let inspection = nymph_codegen::inspect_embedded_module(source);
				if let Some(source) = inspection.unsupported_imports.into_iter().next() {
					return Err(HostRuntimeGraphError::UnsupportedSourceImport {
						module: descriptor.module,
						source,
					});
				}
				let mut actual = FxHashSet::default();
				for import in inspection.imports {
					if !actual.insert(import.clone()) {
						return Err(HostRuntimeGraphError::DuplicateSourceImport {
							module: descriptor.module,
							source: import,
						});
					}
					if !descriptor
						.dependencies
						.iter()
						.any(|dependency| dependency.source == import)
					{
						return Err(HostRuntimeGraphError::UndeclaredSourceImport {
							module: descriptor.module,
							source: import,
						});
					}
				}
				for dependency in descriptor.dependencies {
					if !actual.contains(dependency.source) {
						return Err(HostRuntimeGraphError::MissingDependencySourceSpelling {
							module: descriptor.module,
							source: dependency.source,
						});
					}
				}
			}
		}
		for descriptor in descriptors {
			for dependency in descriptor.dependencies {
				match dependency.target {
					DependencyTarget::HostModule(target)
						if !by_module.contains_key(target) || target != dependency.destination =>
					{
						return Err(HostRuntimeGraphError::MissingSourceProvider { module: target });
					}
					DependencyTarget::CompilerRuntimeRole(role)
						if dependency.destination != Self::role_import_specifier(role) =>
					{
						return Err(HostRuntimeGraphError::DependencyTargetMismatch {
							module: descriptor.module,
							destination: dependency.destination,
						});
					}
					_ => {}
				}
			}
		}
		let mut selectors = FxHashMap::default();
		let mut by_export = FxHashMap::default();
		for export in exports {
			if !by_module.contains_key(export.module) {
				return Err(HostRuntimeGraphError::UnownedLinkageModule {
					module: export.module,
				});
			}
			let selector = (export.marker, export.receiver_tag);
			let target = (export.module, export.symbol, export.delivery);
			if let Some(old) = selectors.insert(selector, target) {
				return Err(if old == target {
					HostRuntimeGraphError::DuplicateAbiSelector {
						marker: export.marker,
						receiver_tag: export.receiver_tag,
					}
				} else {
					HostRuntimeGraphError::ConflictingAbiSelector {
						marker: export.marker,
					}
				});
			}
			if by_export
				.insert(
					(export.module, export.symbol),
					(export.marker, export.delivery),
				)
				.is_some_and(|old| old != (export.marker, export.delivery))
			{
				return Err(HostRuntimeGraphError::ConflictingAbiTarget {
					module: export.module,
					symbol: export.symbol,
				});
			}
		}
		for descriptor in descriptors {
			if let SourceProvider::EmbeddedTs(source) = descriptor.provider {
				let bindings = nymph_codegen::inspect_embedded_module(source).exported_bindings;
				for &(module, symbol) in by_export
					.keys()
					.filter(|(module, _)| *module == descriptor.module)
				{
					let count = bindings
						.iter()
						.filter(|binding| binding.as_str() == symbol)
						.count();
					match count {
						0 => return Err(HostRuntimeGraphError::MissingSourceExport { module, symbol }),
						1 => {}
						_ => return Err(HostRuntimeGraphError::DuplicateSourceExport { module, symbol }),
					}
				}
			}
		}
		Ok(Self {
			descriptors: by_module,
			exports: by_export
				.into_iter()
				.map(|(target, (_, delivery))| (target, delivery))
				.collect(),
		})
	}

	#[cfg(test)]
	fn delivery(&self, module: &str, symbol: &str) -> Option<Delivery> {
		self.exports.get(&(module, symbol)).copied()
	}

	pub(crate) fn semantic_dependencies(
		&self,
		module: &str,
	) -> impl Iterator<Item = CompilerRuntimeRole> + '_ {
		self
			.descriptors
			.get(module)
			.into_iter()
			.flat_map(|descriptor| descriptor.dependencies)
			.filter_map(|dependency| match dependency.target {
				DependencyTarget::CompilerRuntimeRole(role) => Some(role),
				DependencyTarget::HostModule(_) => None,
			})
	}

	pub(crate) const fn role_import_specifier(role: CompilerRuntimeRole) -> &'static str {
		match role {
			CompilerRuntimeRole::Option => "std/option",
		}
	}

	#[cfg(test)]
	fn rewrite(&self, module: &str, source: &str) -> Option<&'static str> {
		self
			.descriptors
			.get(module)?
			.dependencies
			.iter()
			.find(|dependency| dependency.source == source)
			.map(|dependency| dependency.destination)
	}

	#[cfg(test)]
	fn runtime_type_imports<'a>(
		&self,
		modules: impl IntoIterator<Item = &'a String>,
	) -> FxHashSet<ecow::EcoString> {
		modules
			.into_iter()
			.flat_map(|module| self.semantic_dependencies(module))
			.map(|role| match role {
				CompilerRuntimeRole::Option => ecow::EcoString::from("Option"),
			})
			.collect()
	}

	pub(crate) fn module_sources(&self, option_enum_name: &str) -> FxHashMap<String, String> {
		let mut symbols: FxHashMap<&str, Vec<&str>> = FxHashMap::default();
		for &(module, symbol) in self.exports.keys() {
			symbols.entry(module).or_default().push(symbol);
		}
		let mut sources = FxHashMap::default();
		for descriptor in self.descriptors.values() {
			match descriptor.provider {
				SourceProvider::EmbeddedTs(source) => {
					let mut keep = symbols.remove(descriptor.module).unwrap_or_default();
					keep.sort_unstable();
					let rewrites = descriptor
						.dependencies
						.iter()
						.map(|dependency| (dependency.source, dependency.destination))
						.collect::<Vec<_>>();
					sources.insert(
						descriptor.module.to_string(),
						nymph_codegen::strip_ts_to_js(source, &keep, &rewrites),
					);
				}
				SourceProvider::GeneratedBox => {
					sources.insert(
						descriptor.module.to_string(),
						nymph_codegen::box_module_source_with_option_enum(option_enum_name),
					);
				}
			}
		}
		sources
	}
}

fn linkage_exports() -> Vec<HostExport> {
	let mut exports = Vec::new();
	for (marker, linked) in nymph_hir::linkage::REGISTRY {
		exports.push(HostExport {
			marker,
			module: linked.module,
			symbol: linked.symbol,
			receiver_tag: linked.receiver_tag,
			delivery: Delivery::Callable,
		});
	}
	for (marker, value) in nymph_hir::linkage::VALUE_REGISTRY {
		exports.push(HostExport {
			marker,
			module: value.linked.module,
			symbol: value.linked.symbol,
			receiver_tag: value.linked.receiver_tag,
			delivery: Delivery::Value(value.marshal),
		});
	}
	exports
}

#[cfg(test)]
mod tests {
	use super::*;
	use nymph_hir::hir::MarshalKind;

	fn test_descriptor(module: &'static str) -> HostModuleDescriptor {
		HostModuleDescriptor {
			module,
			provider: SourceProvider::EmbeddedTs(""),
			dependencies: &[],
		}
	}

	fn test_export(module: &'static str, symbol: &'static str) -> HostExport {
		HostExport {
			marker: symbol,
			module,
			symbol,
			receiver_tag: None,
			delivery: Delivery::Callable,
		}
	}

	#[test]
	fn graph_joins_every_linkage_export_with_typed_delivery() {
		let graph = HostRuntimeGraph::compiler_facts();
		for (_, linked) in nymph_hir::linkage::REGISTRY {
			assert_eq!(
				graph.delivery(linked.module, linked.symbol),
				Some(Delivery::Callable)
			);
		}
		for (_, value) in nymph_hir::linkage::VALUE_REGISTRY {
			assert_eq!(
				graph.delivery(value.linked.module, value.linked.symbol),
				Some(Delivery::Value(value.marshal))
			);
		}
		assert_eq!(
			graph.delivery("std/math/intrinsics", "max_float"),
			Some(Delivery::Value(MarshalKind::Float))
		);
	}

	#[test]
	fn graph_dependencies_and_rewrites_are_module_local_and_exact() {
		let graph = HostRuntimeGraph::compiler_facts();
		assert_eq!(
			graph
				.semantic_dependencies("std/collections/list")
				.collect::<Vec<_>>(),
			vec![CompilerRuntimeRole::Option]
		);
		assert_eq!(
			graph.rewrite("std/collections/list", "std/option"),
			Some("std/option")
		);
		assert_eq!(
			graph.rewrite("std/string", "std/option"),
			Some("std/option")
		);
		assert_eq!(graph.rewrite("std/io", "./display"), Some("std/display"));
		assert_eq!(graph.rewrite("std/hash", "./display"), None);
	}

	#[test]
	fn malformed_graphs_report_typed_consistency_errors() {
		let duplicate = [test_descriptor("std/a"), test_descriptor("std/a")];
		assert!(matches!(
			HostRuntimeGraph::build(&duplicate, &[]),
			Err(HostRuntimeGraphError::DuplicateModuleDescriptor { module: "std/a" })
		));
		let descriptors = [HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs("export const x = 1;"),
			dependencies: &[],
		}];
		let exports = [test_export("std/a", "x"), test_export("std/a", "x")];
		assert!(matches!(
			HostRuntimeGraph::build(&descriptors, &exports),
			Err(HostRuntimeGraphError::DuplicateAbiSelector {
				marker: "x",
				receiver_tag: None
			})
		));
		let missing = [test_export("std/missing", "x")];
		assert!(matches!(
			HostRuntimeGraph::build(&descriptors, &missing),
			Err(HostRuntimeGraphError::UnownedLinkageModule {
				module: "std/missing"
			})
		));
		let bad_dependency = [HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs(""),
			dependencies: &[ImportDependency {
				source: "./missing",
				destination: "std/missing",
				target: DependencyTarget::HostModule("std/missing"),
			}],
		}];
		assert!(matches!(
			HostRuntimeGraph::build(&bad_dependency, &[]),
			Err(HostRuntimeGraphError::MissingDependencySourceSpelling {
				module: "std/a",
				source: "./missing"
			})
		));
	}

	#[test]
	fn receiver_variants_are_the_only_selector_deduplication() {
		let descriptors = [HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs("export const x = 1;"),
			dependencies: &[],
		}];
		let mut list = test_export("std/a", "x");
		list.receiver_tag = Some("list");
		let mut mutable = list;
		mutable.receiver_tag = Some("mut_list");
		assert!(HostRuntimeGraph::build(&descriptors, &[list, mutable]).is_ok());

		let mut wrong_delivery = mutable;
		wrong_delivery.receiver_tag = Some("list");
		wrong_delivery.delivery = Delivery::Value(MarshalKind::Float);
		assert!(matches!(
			HostRuntimeGraph::build(&descriptors, &[list, wrong_delivery]),
			Err(HostRuntimeGraphError::ConflictingAbiSelector { marker: "x" })
		));
		let mut alias = list;
		alias.marker = "alias";
		assert!(matches!(
			HostRuntimeGraph::build(&descriptors, &[list, alias]),
			Err(HostRuntimeGraphError::ConflictingAbiTarget {
				module: "std/a",
				symbol: "x"
			})
		));
	}

	#[test]
	fn malformed_embedded_sources_report_ast_based_errors() {
		const DEPENDENCY: ImportDependency = ImportDependency {
			source: "std/option",
			destination: "std/option",
			target: DependencyTarget::CompilerRuntimeRole(CompilerRuntimeRole::Option),
		};
		let descriptor = |source| HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs(source),
			dependencies: &[DEPENDENCY],
		};
		assert!(matches!(
			HostRuntimeGraph::build(&[descriptor("export const x = 1;")], &[]),
			Err(HostRuntimeGraphError::MissingDependencySourceSpelling { .. })
		));
		assert!(matches!(
			HostRuntimeGraph::build(
				&[descriptor(
					"import { Option } from \"std/option\"; import { Some } from \"std/option\";"
				)],
				&[]
			),
			Err(HostRuntimeGraphError::DuplicateSourceImport { .. })
		));
		let undeclared = [HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs("import { X } from \"other\";"),
			dependencies: &[],
		}];
		assert!(matches!(
			HostRuntimeGraph::build(&undeclared, &[]),
			Err(HostRuntimeGraphError::UndeclaredSourceImport { .. })
		));
		for source in [
			"import Default from \"std/option\";",
			"import * as Option from \"std/option\";",
			"import \"std/option\";",
			"import Default, { Option } from \"std/option\";",
			"import { Option as Maybe } from \"std/option\";",
		] {
			assert!(matches!(
				HostRuntimeGraph::build(&[descriptor(source)], &[]),
				Err(HostRuntimeGraphError::UnsupportedSourceImport { .. })
			));
		}
		let wrong_destination = [HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs("import { Option } from \"std/option\";"),
			dependencies: &[ImportDependency {
				source: "std/option",
				destination: "wrong/option",
				target: DependencyTarget::CompilerRuntimeRole(CompilerRuntimeRole::Option),
			}],
		}];
		assert!(matches!(
			HostRuntimeGraph::build(&wrong_destination, &[]),
			Err(HostRuntimeGraphError::DependencyTargetMismatch { .. })
		));
		let missing_export = [HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs("export const y = 1;"),
			dependencies: &[],
		}];
		assert!(matches!(
			HostRuntimeGraph::build(&missing_export, &[test_export("std/a", "x")]),
			Err(HostRuntimeGraphError::MissingSourceExport { .. })
		));
		let duplicate_export = [HostModuleDescriptor {
			module: "std/a",
			provider: SourceProvider::EmbeddedTs("export const x = 1; export const x = 2;"),
			dependencies: &[],
		}];
		assert!(matches!(
			HostRuntimeGraph::build(&duplicate_export, &[test_export("std/a", "x")]),
			Err(HostRuntimeGraphError::DuplicateSourceExport { .. })
		));
	}

	#[test]
	fn runtime_type_imports_are_projected_from_module_dependencies() {
		let modules = vec!["std/hash".to_string(), "std/string".to_string()];
		assert_eq!(
			HostRuntimeGraph::compiler_facts().runtime_type_imports(&modules),
			FxHashSet::from_iter([ecow::EcoString::from("Option")])
		);
	}

	#[test]
	fn injects_a_list_module_with_every_linked_symbol_and_a_resolvable_option_import() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources("Option");
		let list_js = sources
			.get("std/collections/list")
			.expect("expected the linked-symbol registry module to be injected");
		for symbol in ["length", "get", "first", "last", "pop"] {
			assert!(
				list_js.contains(symbol),
				"expected the linked `{symbol}` export to survive stripping, got:\n{list_js}"
			);
		}
		// `get`/`first`/`last`/`pop` all construct `Option.Some`/`Option.None`
		// — the import must survive, rewritten to the injected virtual
		// `std/option` key (never the original, unresolvable `"../option"`).
		assert!(
			list_js.contains("import { Option } from \"std/option\";"),
			"expected the `Option` import to survive, rewritten to `std/option`, got:\n{list_js}"
		);
		assert!(
			!list_js.contains("\"../option\""),
			"expected the original, unresolvable `../option` specifier to be gone, got:\n{list_js}"
		);
		assert!(
			list_js.contains("from \"std/box\""),
			"list results must use the canonical box module: {list_js}"
		);
	}

	#[test]
	fn injects_a_map_module_with_every_linked_symbol_and_a_resolvable_option_import() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources("Option");
		let map_js = sources
			.get("std/collections/map")
			.expect("expected the linked-symbol registry module to be injected");
		for symbol in [
			"size",
			"get",
			"insert",
			"remove",
			"clear",
			"get_or_insert",
			"contains_key",
			"keys",
			"values",
			"entries",
			"merge",
			"to_string",
		] {
			assert!(
				map_js.contains(symbol),
				"expected the linked `{symbol}` export to survive stripping, got:\n{map_js}"
			);
		}
		// `get`/`remove` both construct `Option.Some`/`Option.None` — the
		// import must survive, rewritten to the canonical `std/option`
		// key (never the original, unresolvable `"../option"`).
		assert!(
			map_js.contains("import { Option } from \"std/option\";"),
			"expected the `Option` import to survive, rewritten to `std/option`, got:\n{map_js}"
		);
		assert!(
			!map_js.contains("\"../option\""),
			"expected the original, unresolvable `../option` specifier to be gone, got:\n{map_js}"
		);
		assert!(
			map_js.contains("from \"std/box\""),
			"map results must use the canonical box module: {map_js}"
		);
		// L3's ABI fix: `get`/`remove` must build `Option.Some({ value: .. })`
		// — a named-field object, not a bare positional value — to
		// interoperate with the checker's generated `Some(value)` pattern
		// binding (see `map.ts`'s own doc comment / L1's `list.ts` template).
		assert!(
			map_js.contains("Option.Some({ value:") || map_js.contains("Option.Some({ value }"),
			"expected `get`/`remove` to build a named-field `Option.Some`, got:\n{map_js}"
		);
	}

	#[test]
	fn injects_the_hash_intrinsic() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources("Option");
		let hash_js = sources
			.get("std/hash")
			.expect("expected the hash runtime module to be injected");
		assert!(hash_js.contains("export const hash"), "{hash_js}");
		assert!(
			hash_js.contains("from \"std/box\""),
			"hash must share the box runtime's structural implementation: {hash_js}"
		);
	}

	#[test]
	fn does_not_fabricate_the_canonical_option_module() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources("Option");
		assert!(
			!sources.contains_key("std/option"),
			"the project compiler must be the sole owner of canonical std/option"
		);
	}

	#[test]
	fn option_dependency_is_structured_and_module_specific() {
		for module in ["std/collections/list", "std/collections/map", "std/string"] {
			assert_eq!(
				HostRuntimeGraph::compiler_facts()
					.semantic_dependencies(module)
					.collect::<Vec<_>>(),
				vec![CompilerRuntimeRole::Option]
			);
		}
		for module in ["std/hash", "std/display", "std/io", "std/math/intrinsics"] {
			assert_eq!(
				HostRuntimeGraph::compiler_facts()
					.semantic_dependencies(module)
					.count(),
				0
			);
		}
	}
}
