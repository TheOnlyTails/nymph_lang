//! Bundle an in-memory ES module graph into one runnable JavaScript string.
//!
//! Bundling never touches disk: `VirtualFsPlugin` resolves and loads
//! canonical module keys straight out of the map the driver already built, so
//! `compile_project` stays exactly as filesystem-agnostic as the rest of the
//! pipeline (see `mod.rs`'s module doc comment). `cwd` is a fixed constant
//! (never the real working directory) and `entry_filenames` defaults to
//! `"[name].js"` (no content hash), so a given set of module sources always
//! produces byte-identical output — required for the golden/e2e tests and for
//! `nymph build` to be reproducible.
#[cfg(all(target_arch = "wasm32", not(feature = "bundler-swc")))]
compile_error!("nymph-compiler requires the `bundler-swc` feature on wasm32");

#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
use std::{borrow::Cow, sync::Arc};

#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
use oxc::{allocator::Allocator, parser::Parser, span::SourceType};
#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
use rolldown::plugin::{
	HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs, HookResolveIdOutput,
	HookResolveIdReturn, HookUsage, Plugin, PluginContext, SharedLoadPluginContext,
};
#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat};
use rustc_hash::FxHashMap;

/// A `rolldown` plugin serving every module source out of an in-memory map
/// keyed by the driver's own canonical module keys (`"main"`,
/// `"geometry/vec"`, ...) — never the filesystem.
#[derive(Debug)]
#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
struct VirtualFsPlugin {
	sources: FxHashMap<String, String>,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
impl Plugin for VirtualFsPlugin {
	fn name(&self) -> Cow<'static, str> {
		"nymph-virtual-fs".into()
	}

	fn register_hook_usage(&self) -> HookUsage {
		HookUsage::ResolveId | HookUsage::Load
	}

	async fn resolve_id(
		&self,
		_ctx: &PluginContext,
		args: &HookResolveIdArgs<'_>,
	) -> HookResolveIdReturn {
		if self.sources.contains_key(args.specifier) {
			Ok(Some(HookResolveIdOutput::from_id(
				args.specifier.to_string(),
			)))
		} else {
			Ok(None)
		}
	}

	async fn load(&self, _ctx: SharedLoadPluginContext, args: &HookLoadArgs<'_>) -> HookLoadReturn {
		match self.sources.get(args.id) {
			Some(source) => Ok(Some(HookLoadOutput {
				code: source.as_str().into(),
				..Default::default()
			})),
			None => Ok(None),
		}
	}
}

/// Bundle `sources` (every processed module's synthesized ES source, keyed by
/// canonical module path) starting from `entry_key`, returning the single
/// output chunk's JS as a string. `sources` must contain `entry_key` and
/// every module transitively `import`ed from it — anything else is simply
/// never visited.
///
/// # Errors
/// Returns a human-readable message on any rolldown build failure. Bundling a
/// graph this driver already resolved, bound, and type-checked is not
/// expected to fail in practice; a failure here means a bug in the
/// import/export synthesis (`mod.rs::wrap_module_js`), not a user-code error.
pub(crate) fn bundle(
	entry_key: &str,
	sources: FxHashMap<String, String>,
) -> Result<String, String> {
	#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
	return bundle_rolldown(entry_key, sources);

	#[cfg(all(
		feature = "bundler-swc",
		any(target_arch = "wasm32", not(feature = "bundler-rolldown"))
	))]
	return swc::bundle(entry_key, sources);

	#[allow(unreachable_code)]
	Err("nymph-compiler requires a bundler backend feature".to_string())
}

#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
fn bundle_rolldown(entry_key: &str, sources: FxHashMap<String, String>) -> Result<String, String> {
	if let Some(entry) = sources.get(entry_key)
		&& !entry.lines().any(|line| line.starts_with("import "))
		&& is_valid_esm(entry)
	{
		return Ok(entry.clone());
	}

	let plugin: Arc<dyn rolldown::plugin::Pluginable> = Arc::new(VirtualFsPlugin { sources });

	let options = BundlerOptions {
		input: Some(vec![InputItem {
			name: Some(entry_key.to_string()),
			import: entry_key.to_string(),
		}]),
		// A fixed, nonexistent virtual root — never the real cwd — so output
		// never depends on where the compiler happens to run from.
		cwd: Some(std::path::PathBuf::from("/nymph/virtual")),
		format: Some(OutputFormat::Esm),
		sourcemap: None,
		..Default::default()
	};

	let mut bundler = Bundler::with_plugins(options, vec![plugin])
		.map_err(|e| format!("rolldown bundler construction failed: {e}"))?;

	let rt = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.map_err(|e| format!("failed to start the bundler's tokio runtime: {e}"))?;

	let output = rt
		.block_on(async { bundler.generate().await })
		.map_err(|e| format!("rolldown bundling failed: {e}"))?;

	let chunk = output
		.assets
		.iter()
		.find(|a| a.filename().ends_with(".js"))
		.or_else(|| output.assets.first())
		.ok_or_else(|| "rolldown produced no output chunk".to_string())?;

	Ok(String::from_utf8_lossy(chunk.content_as_bytes()).into_owned())
}

#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
fn is_valid_esm(source: &str) -> bool {
	let allocator = Allocator::default();
	let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
	!parsed.panicked && !parsed.diagnostics.has_errors()
}

#[cfg(all(
	feature = "bundler-swc",
	any(target_arch = "wasm32", not(feature = "bundler-rolldown"))
))]
mod swc {
	use std::collections::HashMap;

	use anyhow::{Context, bail};
	use rustc_hash::FxHashMap;
	use swc_bundler::{BundleKind, Bundler, Config, Hook, Load, ModuleData, ModuleRecord};
	use swc_common::{FileName, GLOBALS, Globals, SourceMap, sync::Lrc};
	use swc_ecma_ast::{EsVersion, KeyValueProp};
	use swc_ecma_codegen::{Emitter, text_writer::JsWriter};
	use swc_ecma_loader::resolve::{Resolution, Resolve};
	use swc_ecma_parser::{Syntax, parse_file_as_module};

	struct MemoryModules {
		sources: FxHashMap<String, String>,
		cm: Lrc<SourceMap>,
	}

	impl Resolve for MemoryModules {
		fn resolve(&self, base: &FileName, specifier: &str) -> anyhow::Result<Resolution> {
			if self.sources.contains_key(specifier) {
				return Ok(Resolution {
					filename: FileName::Custom(specifier.into()),
					slug: None,
				});
			}
			bail!("unresolved in-memory module {specifier:?} imported by {base:?}")
		}
	}

	impl Load for MemoryModules {
		fn load(&self, file: &FileName) -> anyhow::Result<ModuleData> {
			let FileName::Custom(key) = file else {
				bail!("non-virtual SWC module: {file}")
			};
			let source = self
				.sources
				.get(key)
				.with_context(|| format!("missing module {key:?}"))?;
			let fm = self
				.cm
				.new_source_file(Lrc::new(file.clone()), source.clone());
			let mut diagnostics = vec![];
			let module = parse_file_as_module(
				&fm,
				Syntax::Es(Default::default()),
				EsVersion::latest(),
				None,
				&mut diagnostics,
			)
			.map_err(|error| anyhow::anyhow!("failed to parse {key:?}: {error:?}"))?;
			if !diagnostics.is_empty() {
				bail!("failed to parse {key:?}: {diagnostics:?}");
			}
			Ok(ModuleData {
				fm,
				module,
				helpers: Default::default(),
			})
		}
	}

	struct NoopHook;
	impl Hook for NoopHook {
		fn get_import_meta_props(
			&self,
			_: swc_common::Span,
			_: &ModuleRecord,
		) -> Result<Vec<KeyValueProp>, anyhow::Error> {
			Ok(vec![])
		}
	}

	pub(super) fn bundle(entry: &str, sources: FxHashMap<String, String>) -> Result<String, String> {
		if !sources.contains_key(entry) {
			return Err(format!("bundle entry {entry:?} is missing"));
		}
		let cm: Lrc<SourceMap> = Default::default();
		let modules = MemoryModules {
			sources,
			cm: cm.clone(),
		};
		let globals = Globals::new();
		GLOBALS.set(&globals, || {
			let resolver = MemoryModules {
				sources: modules.sources.clone(),
				cm: cm.clone(),
			};
			let mut bundler = Bundler::new(
				&globals,
				cm.clone(),
				modules,
				resolver,
				Config {
					require: false,
					..Default::default()
				},
				Box::new(NoopHook),
			);
			let bundles = bundler
				.bundle(HashMap::from([(
					"main".into(),
					FileName::Custom(entry.into()),
				)]))
				.map_err(|error| format!("SWC bundling failed: {error:?}"))?;
			emit(cm, bundles).map_err(|error| format!("SWC emission failed: {error:#}"))
		})
	}

	fn emit(cm: Lrc<SourceMap>, mut bundles: Vec<swc_bundler::Bundle>) -> anyhow::Result<String> {
		if bundles.len() != 1 {
			bail!(
				"SWC produced {} output bundles instead of one",
				bundles.len()
			);
		}
		let bundle = bundles.pop().expect("one SWC bundle was checked above");
		if bundle.kind
			!= (BundleKind::Named {
				name: "main".into(),
			}) {
			bail!(
				"SWC produced unexpected output bundle kind: {:?}",
				bundle.kind
			);
		}
		let mut output = vec![];
		let mut emitter = Emitter {
			cfg: Default::default(),
			cm: cm.clone(),
			comments: None,
			wr: JsWriter::new(cm, "\n", &mut output, None),
		};
		emitter.emit_module(&bundle.module)?;
		String::from_utf8(output).context("SWC emitted non-UTF-8 JavaScript")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::process::Command;

	#[test]
	fn compiler_host_graph_bundles_and_runs_the_structured_task_kernel() {
		let mut sources = crate::host_runtime::HostRuntimeGraph::compiler_facts().module_sources(
			"Option",
			"@nymph/runtime/std/option",
			false,
		);
		sources.insert(
			"task-test".to_string(),
			r#"import { nymphCallable, nymphReturn, nymphRunTask, nymphTaskRecipe } from "std/box";
const task = nymphTaskRecipe(nymphCallable(() => nymphReturn(42)), true);
console.log(await nymphRunTask(task));
"#
			.to_string(),
		);
		let bundled = bundle("task-test", sources).expect("bundle task runtime");
		let path = std::env::temp_dir().join(format!(
			"nymph_task_host_runtime_{}.mjs",
			std::process::id()
		));
		std::fs::write(&path, &bundled).expect("write task runtime test module");
		let output = Command::new("node")
			.arg(&path)
			.output()
			.expect("run task runtime under Node");
		let _ = std::fs::remove_file(path);
		assert!(
			output.status.success(),
			"node failed:\n{}\n--- js ---\n{bundled}",
			String::from_utf8_lossy(&output.stderr)
		);
		assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
	}

	#[test]
	fn node_adapter_boundary_preserves_modes_bigints_opaque_aliases_and_defects() {
		let mut sources = crate::host_runtime::HostRuntimeGraph::compiler_facts().module_sources(
			"Option",
			"@nymph/runtime/std/option",
			false,
		);
		sources.insert(
			"std/test-adapters".to_string(),
			r#"const resource = { closed: false };
export function open() { return resource; }
export function ordinary(value) {
  if (arguments.length !== 1) throw new TypeError("ordinary adapter received hidden state");
  return value;
}
export function cancellable(value, signal) {
  if (arguments.length !== 2 || !(signal instanceof AbortSignal)) throw new TypeError("missing execution AbortSignal");
  return value;
}
export function close(value) {
  if (arguments.length !== 1) throw new TypeError("cleanup adapter received hidden state");
  value.closed = true;
}
export function defect() { throw new TypeError("bad trusted ABI"); }
"#
			.to_string(),
		);
		sources.insert(
			"adapter-test".to_string(),
			r#"import { nymphBoxOpaque, nymphCallable, nymphCurrentExecutionSignal, nymphReturn, nymphRunTask, nymphTaskRecipe, nymphUnboxOpaque } from "std/box";
import { cancellable, close, defect, open, ordinary } from "std/test-adapters";
const file = nymphBoxOpaque(117n, open());
const alias = file;
const exact = ordinary(9007199254740993n);
const direct = (() => { try { defect(); return "repaired"; } catch (error) { return `${error.name}:${error.message}`; } })();
const step = nymphCallable(() => {
  const host = nymphUnboxOpaque(117n, alias);
  const value = cancellable(host, nymphCurrentExecutionSignal());
  close(value);
  close(nymphUnboxOpaque(117n, file));
  return nymphReturn(value.closed);
});
const spawned = await nymphRunTask(nymphTaskRecipe(step, false));
const defectStep = nymphCallable(() => defect());
const taskDefect = await (async () => { try { await nymphRunTask(nymphTaskRecipe(defectStep, false)); return "repaired"; } catch (error) { return `${error.name}:${error.message}`; } })();
console.log([typeof exact, exact, nymphUnboxOpaque(117n, file) === nymphUnboxOpaque(117n, alias), spawned, direct, taskDefect].join("|"));
"#
			.to_string(),
		);
		let bundled = bundle("adapter-test", sources).expect("bundle adapter runtime");
		let path = std::env::temp_dir().join(format!(
			"nymph_adapter_host_runtime_{}.mjs",
			std::process::id()
		));
		std::fs::write(&path, &bundled).expect("write adapter runtime test module");
		let output = Command::new("node")
			.arg(&path)
			.output()
			.expect("run adapter runtime under Node");
		let _ = std::fs::remove_file(path);
		assert!(
			output.status.success(),
			"node failed:\n{}\n--- js ---\n{bundled}",
			String::from_utf8_lossy(&output.stderr)
		);
		assert_eq!(
			String::from_utf8_lossy(&output.stdout).trim(),
			"bigint|9007199254740993|true|true|TypeError:bad trusted ABI|TypeError:bad trusted ABI"
		);
	}

	#[test]
	fn self_contained_entry_is_returned_without_rebundling() {
		let source = "function main() {}\nexport { main };\n";
		let sources = FxHashMap::from_iter([("main".to_string(), source.to_string())]);

		let js = bundle("main", sources).expect("self-contained entry should compile");

		#[cfg(all(not(target_arch = "wasm32"), feature = "bundler-rolldown"))]
		assert_eq!(js, source);
		#[cfg(any(target_arch = "wasm32", not(feature = "bundler-rolldown")))]
		assert!(js.contains("function main()"));
	}

	#[test]
	fn invalid_self_contained_entry_still_reports_a_bundle_error() {
		let source = "function default() {}\nexport { default };\n";
		let sources = FxHashMap::from_iter([("main".to_string(), source.to_string())]);

		let result = bundle("main", sources);

		assert!(result.is_err(), "invalid ESM must not bypass validation");
	}

	// Rolldown's tree-shaking does not drop a dependency's observable top-level
	// code when the importer never references what it exports (e.g. a
	// bare `import @/dep;` with no `with`-list): rolldown/oxc
	// distinguish PROVABLY PURE unused code (safe, correct to drop — e.g. a
	// literal assigned to an unreferenced binding) from code whose evaluation
	// has an observable effect the analyzer can't rule out (e.g. a call to an
	// opaque global like `console.log`), and only ever drops the former. This
	// test pins the latter: `wrap_module_js` imports a dependency's full
	// non-private surface regardless of use (see its doc comment) — so a
	// genuinely side-effecting export that the importer never references must
	// still survive bundling.
	#[test]
	fn genuinely_side_effecting_unreferenced_export_survives_bundling() {
		let sources = FxHashMap::from_iter([
			(
				"main".to_string(),
				"import { sideEffectMarker } from \"helper\";\nfunction main() {}\nexport { main };\n"
					.to_string(),
			),
			(
				"helper".to_string(),
				"const sideEffectMarker = console.log(\"helper loaded\");\nexport { sideEffectMarker };\n"
					.to_string(),
			),
		]);
		let js = bundle("main", sources).expect("bundle should succeed");
		assert!(
			js.contains("console.log(\"helper loaded\")"),
			"expected the genuinely side-effecting (unreferenced) export to survive bundling, got:\n{js}"
		);
	}

	#[test]
	fn aliases_and_reexports_bundle_deterministically() {
		let sources = || {
			FxHashMap::from_iter([
				(
					"main".into(),
					"import { renamed } from \"api\";\nexport { renamed as result };\n".into(),
				),
				(
					"api".into(),
					"export { value as renamed } from \"dep\";\n".into(),
				),
				(
					"dep".into(),
					"const value = 42;\nexport { value };\n".into(),
				),
			])
		};
		let first = bundle("main", sources()).expect("alias graph should bundle");
		let second = bundle("main", sources()).expect("alias graph should bundle repeatedly");
		assert_eq!(first, second);
		assert!(first.contains("42"));
		assert!(!first.contains("from \"api\""));
		assert!(!first.contains("from \"dep\""));
	}

	#[cfg(all(
		feature = "bundler-swc",
		any(target_arch = "wasm32", not(feature = "bundler-rolldown"))
	))]
	#[test]
	fn swc_rejects_noncanonical_relative_imports() {
		let sources = FxHashMap::from_iter([
			(
				"main".into(),
				"import { value } from \"./dep\";\nexport { value };\n".into(),
			),
			("dep".into(), "export const value = 42;\n".into()),
		]);
		assert!(bundle("main", sources).is_err());
	}
}
