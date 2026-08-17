//! In-process TypeScript-to-JavaScript type stripping for a stdlib intrinsic
//! module's `.ts` source using oxc stripping.
//!
//! The `.ts` files under `stdlib/src/**` (e.g.
//! `stdlib/src/collections/list.ts`) are the single source of truth for a
//! LINKED external's real JS implementation — this module never rewrites
//! them, it only strips their TS-only syntax down to plain JS via oxc's own
//! parser + transformer + codegen (already a workspace dependency of this
//! crate), so no separate `pnpm build`/Node step is ever needed to produce
//! runnable JS from them.
//!
//! [`strip_ts_to_js`] additionally FILTERS the stripped module down to only
//! the `export const <name> = ..` declarations named in `keep`: injecting the
//! FULL stripped `list.ts` into the bundle graph fails, because rolldown
//! resolves every `import` at graph-build time (before tree-shaking can drop
//! anything) — `list.ts`'s own `import { Option } from "../option"` would be
//! a fatal unresolved specifier the moment ANY symbol from the module is
//! used, even one that never touches `Option`. This is avoided by
//! filtering to only symbols that never reference `Option` (`length`),
//! dropping the import unconditionally along with everything else.
//!
//! An Option-returning intrinsic (e.g.
//! `list.ts`'s `get`) genuinely needs its `import { Option } from
//! "../option"` to survive — but only when a KEPT export still references it
//! (an unrelated kept export, like plain `length`, must still drop it, as
//! before). `import_rewrites` names which source specifiers are resolvable
//! virtual modules at all (e.g. `"../option"` → `"std/option"`, the bare key
//! `bundle::VirtualFsPlugin` serves an injected `std/option` module under —
//! see `nymph-compiler`'s `HostRuntimeGraph`); an import whose specifier
//! isn't in this map is dropped unconditionally, since
//! nothing would resolve it in the bundle graph anyway. An import whose
//! specifier IS in the map is kept — with its specifier rewritten to the
//! resolvable key — only if at least one of its imported local names still
//! appears (as a whole JS identifier, not a substring hit) in the exports
//! [`keep`] retained.
use oxc::allocator::Allocator;
use oxc::ast::ast::{Declaration, ImportDeclarationSpecifier, Statement};
use oxc::codegen::Codegen;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::SourceType;
use oxc::transformer::{TransformOptions, Transformer};
use std::path::Path;

/// The narrow top-level source facts supported by host runtime modules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedModuleInspection {
	pub imports: Vec<String>,
	pub unsupported_imports: Vec<String>,
	pub exported_bindings: Vec<String>,
}

/// Inspect actual named imports and `export const` bindings in embedded TS.
/// Duplicate entries are intentionally retained for consistency validation.
#[must_use]
pub fn inspect_embedded_module(source: &str) -> EmbeddedModuleInspection {
	let allocator = Allocator::default();
	let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
	assert!(
		!parsed.panicked && !parsed.diagnostics.has_errors(),
		"inspect_embedded_module: failed to parse embedded stdlib TS source: {:?}",
		parsed.diagnostics
	);
	let mut imports = Vec::new();
	let mut unsupported_imports = Vec::new();
	let mut exported_bindings = Vec::new();
	for statement in &parsed.program.body {
		match statement {
			Statement::ImportDeclaration(import) => {
				// The stripping contract reconstructs ordinary named imports. Reject
				// every other shape rather than silently dropping or changing it.
				let supported = import.specifiers.as_ref().is_some_and(|specifiers| {
					!specifiers.is_empty()
						&& specifiers.iter().all(|specifier| {
							matches!(
								specifier,
								ImportDeclarationSpecifier::ImportSpecifier(named)
									if named.imported.name().as_ref() == named.local.name.as_str()
							)
						})
				});
				if supported {
					imports.push(import.source.value.to_string());
				} else {
					unsupported_imports.push(import.source.value.to_string());
				}
			}
			Statement::ExportNamedDeclaration(export) => {
				if let Some(Declaration::VariableDeclaration(variable)) = &export.declaration {
					for declaration in &variable.declarations {
						if let Some(binding) = declaration.id.get_binding_identifier() {
							exported_bindings.push(binding.name.to_string());
						}
					}
				}
			}
			_ => {}
		}
	}
	EmbeddedModuleInspection {
		imports,
		unsupported_imports,
		exported_bindings,
	}
}

/// Strip `source` (TypeScript) down to plain JavaScript, then filter its
/// top-level body to only the `export const <name> = ..` declarations whose
/// binding identifier appears in `keep`, keeping (and rewriting, per
/// `import_rewrites`) only the imports the surviving exports still need — see
/// this module's own doc comment. Panics on a parse error — `source` is
/// always a fixed, `include_str!`-embedded stdlib file, never user input, so
/// a parse failure here is a compiler bug, not a user-facing condition
/// (mirrors this codebase's "loud panic over silent wrong-JS" convention).
#[must_use]
pub fn strip_ts_to_js(source: &str, keep: &[&str], import_rewrites: &[(&str, &str)]) -> String {
	let allocator = Allocator::default();
	let parser_ret = Parser::new(&allocator, source, SourceType::ts()).parse();
	assert!(
		!parser_ret.panicked && !parser_ret.diagnostics.has_errors(),
		"strip_ts_to_js: failed to parse embedded stdlib TS source: {:?}",
		parser_ret.diagnostics
	);
	let mut program = parser_ret.program;

	let scoping = SemanticBuilder::new()
		.build(&program)
		.semantic
		.into_scoping();
	Transformer::new(
		&allocator,
		Path::new("stdlib.ts"),
		&TransformOptions::default(),
	)
	.build_with_scoping(scoping, &mut program);

	// Snapshot every top-level import (specifier + its imported local names)
	// BEFORE filtering — `retain` below drops every `Statement::ImportDeclaration`
	// unconditionally (it only keeps `ExportNamedDeclaration`s matching `keep`),
	// so this is the only chance to see what a surviving export might still need.
	let imports: Vec<(String, Vec<String>)> = program
		.body
		.iter()
		.filter_map(|stmt| match stmt {
			Statement::ImportDeclaration(import) => {
				let names = import
					.specifiers
					.iter()
					.flatten()
					.filter_map(|spec| match spec {
						ImportDeclarationSpecifier::ImportSpecifier(named) => {
							Some(named.local.name.to_string())
						}
						// A default/namespace import never appears in the stdlib
						// runtime sources today (every cross-module reference is a
						// named import, e.g. `{ Option }`) — skipped because keeping
						// one requires a different rewritten-import shape.
						_ => None,
					})
					.collect();
				Some((import.source.value.to_string(), names))
			}
			_ => None,
		})
		.collect();

	program.body.retain(|stmt| {
		matches!(stmt, Statement::ExportNamedDeclaration(export)
		if matches!(&export.declaration, Some(Declaration::VariableDeclaration(var))
			if var.declarations.iter().any(|decl| {
				decl.id.get_binding_identifier()
					.is_some_and(|binding| keep.contains(&binding.name.as_str()))
			})))
	});

	let body = Codegen::new().build(&program).code;

	let mut prelude = String::new();
	for (specifier, names) in &imports {
		let Some((_, rewritten)) = import_rewrites.iter().find(|(from, _)| from == specifier) else {
			continue;
		};
		let used: Vec<&str> = names
			.iter()
			.map(String::as_str)
			.filter(|name| contains_identifier(&body, name))
			.collect();
		if !used.is_empty() {
			prelude.push_str(&format!(
				"import {{ {} }} from \"{rewritten}\";\n",
				used.join(", ")
			));
		}
	}

	format!("{prelude}{body}")
}

/// Whether `name` appears in `haystack` as a whole JS identifier (not merely
/// a substring) — e.g. `"Option"` matches `Option.Some(x)` but must not match
/// `MyOption` or `Option2`.
fn contains_identifier(haystack: &str, name: &str) -> bool {
	if name.is_empty() {
		return false;
	}
	let bytes = haystack.as_bytes();
	let mut start = 0;
	while let Some(pos) = haystack[start..].find(name) {
		let idx = start + pos;
		let before_ok = idx == 0 || !is_ident_char(bytes[idx - 1]);
		let after = idx + name.len();
		let after_ok = after >= bytes.len() || !is_ident_char(bytes[after]);
		if before_ok && after_ok {
			return true;
		}
		start = idx + 1;
	}
	false
}

fn is_ident_char(b: u8) -> bool {
	b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn strips_types_and_keeps_only_the_requested_export() {
		let source = "import { Option } from \"../option\";\n\
			export const length = ($_this: any[]) => $_this.length;\n\
			export const get = <T>($_this: T[], i: number) =>\n\
			\ti < $_this.length ? Option.Some($_this[i]) : Option.None;\n";
		let js = strip_ts_to_js(source, &["length"], &[("../option", "std/option")]);
		assert!(
			!js.contains("Option"),
			"expected the unused `Option` import/export to be dropped, got:\n{js}"
		);
		assert!(
			!js.contains(": any") && !js.contains(": number"),
			"expected TS type annotations to be stripped, got:\n{js}"
		);
		assert!(
			js.contains("length"),
			"expected the kept `length` export to survive, got:\n{js}"
		);
		assert!(
			!js.contains("get ="),
			"expected the un-kept `get` export to be dropped, got:\n{js}"
		);
	}

	// The un-kept `Option` import above is dropped only
	// because nothing SURVIVING the `keep` filter still references it. When
	// `get` (which DOES reference `Option`) is kept instead, the import must
	// survive too — rewritten to the resolvable virtual specifier
	// `import_rewrites` names — so the injected module's own `import { Option
	// } from "../option"` resolves in the bundle graph instead of dangling.
	#[test]
	fn keeps_and_rewrites_the_option_import_when_a_kept_export_still_needs_it() {
		let source = "import { Option } from \"../option\";\n\
			export const length = ($_this: any[]) => $_this.length;\n\
			export const get = <T>($_this: T[], i: number) =>\n\
			\ti < $_this.length ? Option.Some($_this[i]) : Option.None;\n";
		let js = strip_ts_to_js(source, &["get"], &[("../option", "std/option")]);
		assert!(
			js.contains("import { Option } from \"std/option\";"),
			"expected the `Option` import to survive, rewritten to the virtual \
			 `std/option` specifier, got:\n{js}"
		);
		assert!(
			js.contains("get ="),
			"expected the kept `get` export to survive, got:\n{js}"
		);
		assert!(
			!js.contains("length ="),
			"expected the un-kept `length` export to be dropped, got:\n{js}"
		);
	}

	// A specifier with no entry in `import_rewrites` at all must still be
	// dropped unconditionally — nothing in the bundle graph
	// would resolve it even if kept (mirrors `strips_types_and_keeps_only_the_
	// requested_export`, but pins the "no rewrite configured" branch
	// specifically, independent of whether the kept export references it).
	#[test]
	fn drops_an_import_with_no_configured_rewrite_even_if_referenced() {
		let source = "import { Option } from \"../option\";\n\
			export const get = <T>($_this: T[], i: number) =>\n\
			\ti < $_this.length ? Option.Some($_this[i]) : Option.None;\n";
		let js = strip_ts_to_js(source, &["get"], &[]);
		assert!(
			!js.contains("import"),
			"expected the import to be dropped with no rewrite configured for its \
			 specifier, got:\n{js}"
		);
	}
}
