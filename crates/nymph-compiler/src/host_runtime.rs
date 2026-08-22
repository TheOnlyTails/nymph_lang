//! The `HostRuntimeGraph` for every linked external: the runtime-source
//! counterpart of the embedded `.nym` checker prelude, containing the real
//! `.ts`/JS implementations that emitted calls resolve against at bundle time.
//!
//! `nymph_hir::linkage::REGISTRY` (a leaf-crate table both the sema gate and
//! codegen emit already consult) decides WHICH `external(name)` markers link,
//! and to which module specifier + exported symbol. It does NOT embed any
//! `.ts` SOURCE itself — a leaf crate (deps: `ecow`, `rustc-hash` only) must
//! not `include_str!` the stdlib tree. This module is the other half: it
//! supplies the actual embedded `.ts` source for each distinct module the
//! registry names, and strips + filters it (via `nymph_codegen::strip_ts_to_js`)
//! into virtual sources merged with the stable emitted project before bundling.
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
//! 2. Stable per-definition lowering emits the demanded compiler-owned Option
//!    implementation once under that key. Host modules and source consumers
//!    therefore share both global variant tags and the same method-bearing
//!    prototype.

use std::sync::OnceLock;

use nymph_hir::hir::MarshalKind;
use rustc_hash::{FxHashMap, FxHashSet};

/// One registry MODULE specifier's `include_str!`-embedded `.ts` source —
/// mirrors `prelude.rs`'s `CORE_SOURCES` table one level down (runtime JS,
/// not checker-facing Nymph source). Add an entry here whenever
/// `nymph_hir::linkage::REGISTRY` gains a module this table doesn't cover yet
/// — graph construction fails loudly (never silently skips) if one is missing.
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
const HOST_MODULES: &[HostModuleDescriptor] = &[
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
		module: "std/collections/set",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/collections/set.ts")),
		dependencies: &[BOX],
	},
	HostModuleDescriptor {
		module: "std/io",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/io.ts")),
		dependencies: &[BOX],
	},
	HostModuleDescriptor {
		module: "std/math/intrinsics",
		provider: SourceProvider::EmbeddedTs(include_str!("../../../stdlib/src/math/mod.ts")),
		dependencies: &[BOX],
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
	Callable(nymph_sema::ExternalCallMode),
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
	MissingAdapter {
		module: String,
		symbol: String,
	},
	MismatchedAdapter {
		module: String,
		symbol: String,
		expected: Delivery,
		actual: Delivery,
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
/// tree-shaking ever gets a chance to drop it). Stable project emission
/// separately supplies the canonical `std/option` module referenced by
/// rewritten imports.
///
/// Keyed by the SAME module specifier the registry names (e.g.
/// `"std/collections/list"`) — the specifier an emitted `import { .. } from
/// ".."` line names, and what `bundle::VirtualFsPlugin` resolves module
/// sources against. Callers merge this into the stable emitted project's
/// module sources before bundling. `VirtualFsPlugin` only loads a source
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

	pub(crate) fn validate_abi(
		&self,
		abi: &nymph_sema::ExternalAbi,
	) -> Result<(), HostRuntimeGraphError> {
		let Some(adapter) = abi.adapter() else {
			return Ok(());
		};
		let expected = nymph_hir::linkage::lookup_value(&abi.marker)
			.ok()
			.filter(|value| {
				value.linked.module == adapter.module && value.linked.symbol == adapter.symbol
			})
			.map_or(Delivery::Callable(abi.call_mode), |value| {
				Delivery::Value(value.marshal)
			});
		let Some(actual) = self
			.exports
			.get(&(adapter.module.as_str(), adapter.symbol.as_str()))
			.copied()
		else {
			return Err(HostRuntimeGraphError::MissingAdapter {
				module: adapter.module.to_string(),
				symbol: adapter.symbol.to_string(),
			});
		};
		if actual != expected {
			return Err(HostRuntimeGraphError::MismatchedAdapter {
				module: adapter.module.to_string(),
				symbol: adapter.symbol.to_string(),
				expected,
				actual,
			});
		}
		Ok(())
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

	pub(crate) fn module_sources(
		&self,
		option_enum_name: &str,
		option_module: &str,
		echo: bool,
	) -> FxHashMap<String, String> {
		let option_import = if option_enum_name == "Option" {
			format!("import {{ Option }} from \"{option_module}\";")
		} else {
			format!("import {{ {option_enum_name} as Option }} from \"{option_module}\";")
		};
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
					let source = nymph_codegen::strip_ts_to_js(source, &keep, &rewrites)
						.replace("import { Option } from \"std/option\";", &option_import);
					sources.insert(descriptor.module.to_string(), source);
				}
				SourceProvider::GeneratedBox => {
					sources.insert(
						descriptor.module.to_string(),
						if echo {
							nymph_codegen::box_module_source_with_option_enum(option_enum_name)
						} else {
							nymph_codegen::box_module_source_with_option_enum_release(option_enum_name)
						},
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
			delivery: Delivery::Callable(nymph_sema::ExternalCallMode::Ordinary),
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
			delivery: Delivery::Callable(nymph_sema::ExternalCallMode::Ordinary),
		}
	}

	#[test]
	fn graph_joins_every_linkage_export_with_typed_delivery() {
		let graph = HostRuntimeGraph::compiler_facts();
		for (_, linked) in nymph_hir::linkage::REGISTRY {
			assert_eq!(
				graph.delivery(linked.module, linked.symbol),
				Some(Delivery::Callable(nymph_sema::ExternalCallMode::Ordinary))
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

	fn linked_abi(
		module: &str,
		symbol: &str,
		call_mode: nymph_sema::ExternalCallMode,
	) -> nymph_sema::ExternalAbi {
		nymph_sema::ExternalAbi {
			marker: symbol.into(),
			callable: nymph_sema::ExternalCallable::Linked {
				adapter: nymph_sema::ExternalAdapterId {
					module: module.into(),
					symbol: symbol.into(),
				},
			},
			effects: nymph_sema::EffectRow::pure(),
			audit: nymph_sema::ExternalAudit::default(),
			call_mode,
			marshal: nymph_sema::ExternalMarshalPlan::default(),
		}
	}

	#[test]
	fn adapter_registry_validates_ordinary_cancellable_missing_and_mismatched_plans() {
		let descriptors = [HostModuleDescriptor {
			module: "std/test",
			provider: SourceProvider::EmbeddedTs(
				"export const ordinary = () => {}; export const cancellable = () => {};",
			),
			dependencies: &[],
		}];
		let mut ordinary = test_export("std/test", "ordinary");
		let mut cancellable = test_export("std/test", "cancellable");
		cancellable.delivery = Delivery::Callable(nymph_sema::ExternalCallMode::Cancellable);
		let graph = HostRuntimeGraph::build(&descriptors, &[ordinary, cancellable]).unwrap();
		assert!(
			graph
				.validate_abi(&linked_abi(
					"std/test",
					"ordinary",
					nymph_sema::ExternalCallMode::Ordinary,
				))
				.is_ok()
		);
		assert!(
			graph
				.validate_abi(&linked_abi(
					"std/test",
					"cancellable",
					nymph_sema::ExternalCallMode::Cancellable,
				))
				.is_ok()
		);
		assert!(matches!(
			graph.validate_abi(&linked_abi(
				"std/test",
				"missing",
				nymph_sema::ExternalCallMode::Ordinary,
			)),
			Err(HostRuntimeGraphError::MissingAdapter { .. })
		));
		assert!(matches!(
			graph.validate_abi(&linked_abi(
				"std/test",
				"ordinary",
				nymph_sema::ExternalCallMode::Cancellable,
			)),
			Err(HostRuntimeGraphError::MismatchedAdapter { .. })
		));
		ordinary.delivery = Delivery::Value(MarshalKind::Float);
		let graph = HostRuntimeGraph::build(&descriptors, &[ordinary]).unwrap();
		assert!(matches!(
			graph.validate_abi(&linked_abi(
				"std/test",
				"ordinary",
				nymph_sema::ExternalCallMode::Ordinary,
			)),
			Err(HostRuntimeGraphError::MismatchedAdapter { .. })
		));
	}

	#[test]
	fn generated_io_source_uses_the_box_protocol_boundary() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources(
			"Option",
			"@nymph/runtime/std/option",
			false,
		);
		let io_js = sources
			.get("std/io")
			.expect("expected the linked I/O runtime module to be injected");
		assert!(io_js.contains("from \"std/box\""), "{io_js}");
		assert!(io_js.contains("nymphProtocolDisplay"), "{io_js}");
	}

	#[test]
	fn math_source_exports_every_registry_symbol_through_the_canonical_box_runtime() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources(
			"Option",
			"@nymph/runtime/std/option",
			false,
		);
		let math_js = sources
			.get("std/math/intrinsics")
			.expect("expected the linked math runtime module to be injected");
		assert!(math_js.contains("from \"std/box\""), "{math_js}");
		for (_, symbols) in nymph_hir::linkage::modules()
			.into_iter()
			.filter(|(module, _)| *module == "std/math/intrinsics")
		{
			for symbol in symbols {
				assert!(
					math_js.contains(&format!("const {symbol} =")),
					"missing `{symbol}` in {math_js}"
				);
			}
		}
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
	fn injects_a_list_primitive_module_with_a_resolvable_option_import() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources(
			"Option",
			"@nymph/runtime/std/option",
			false,
		);
		let list_js = sources
			.get("std/collections/list")
			.expect("expected the linked-symbol registry module to be injected");
		for symbol in ["length", "get", "appended", "replaced", "slice"] {
			assert!(
				list_js.contains(symbol),
				"expected the linked `{symbol}` export to survive stripping, got:\n{list_js}"
			);
		}
		// `get` constructs `Option.Some`/`Option.None`; import the exact owner.
		assert!(
			list_js.contains("from \"@nymph/runtime/std/option\";"),
			"expected the `Option` import to target its exact owner, got:\n{list_js}"
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
		let sources = HostRuntimeGraph::compiler_facts().module_sources(
			"Option",
			"@nymph/runtime/std/option",
			false,
		);
		let map_js = sources
			.get("std/collections/map")
			.expect("expected the linked-symbol registry module to be injected");
		for symbol in [
			"size", "get", "inserted", "removed", "keys", "values", "entries",
		] {
			assert!(
				map_js.contains(symbol),
				"expected the linked `{symbol}` export to survive stripping, got:\n{map_js}"
			);
		}
		// `get` constructs `Option.Some`/`Option.None`; import the exact owner.
		assert!(
			map_js.contains("from \"@nymph/runtime/std/option\";"),
			"expected the `Option` import to target its exact owner, got:\n{map_js}"
		);
		assert!(
			!map_js.contains("\"../option\""),
			"expected the original, unresolvable `../option` specifier to be gone, got:\n{map_js}"
		);
		assert!(
			map_js.contains("from \"std/box\""),
			"map results must use the canonical box module: {map_js}"
		);
		// `get` must build `Option.Some({ value: .. })`
		// — a named-field object, not a bare positional value — to
		// interoperate with the checker's generated `Some(value)` pattern
		// binding (see `map.ts`'s own doc comment / L1's `list.ts` template).
		assert!(
			map_js.contains("Option.Some({ value:") || map_js.contains("Option.Some({ value }"),
			"expected `get`/`remove` to build a named-field `Option.Some`, got:\n{map_js}"
		);
	}

	#[test]
	fn does_not_fabricate_the_canonical_option_module() {
		let sources = HostRuntimeGraph::compiler_facts().module_sources(
			"Option",
			"@nymph/runtime/std/option",
			false,
		);
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
		for module in ["std/io", "std/math/intrinsics"] {
			assert_eq!(
				HostRuntimeGraph::compiler_facts()
					.semantic_dependencies(module)
					.count(),
				0
			);
		}
	}
}
