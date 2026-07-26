//! Bundling the driver's per-module ES sources into one runnable JS string
//! (Slice IB2).
//!
//! [`super::mod`]'s `compile_all` synthesizes a real ES module (`import` /
//! `export`) for every processed Nymph module, over the SAME `$m{tag}$`
//! mangled names IB1 already renders every declaration and reference to (see
//! `rewrite.rs`) — so every reference site and its declaration share one
//! globally-unique identifier and rolldown never has to rename anything, only
//! link the graph. This module feeds that in-memory source map into
//! `rolldown` and returns the single bundled chunk's code.
//!
//! Bundling never touches disk: `VirtualFsPlugin` resolves and loads
//! canonical module keys straight out of the map the driver already built, so
//! `compile_project` stays exactly as filesystem-agnostic as the rest of the
//! pipeline (see `mod.rs`'s module doc comment). `cwd` is a fixed constant
//! (never the real working directory) and `entry_filenames` defaults to
//! `"[name].js"` (no content hash), so a given set of module sources always
//! produces byte-identical output — required for the golden/e2e tests and for
//! `nymph build` to be reproducible.
use std::borrow::Cow;
use std::sync::Arc;

use oxc::{allocator::Allocator, parser::Parser, span::SourceType};
use rolldown::plugin::{
	HookLoadArgs, HookLoadOutput, HookLoadReturn, HookResolveIdArgs, HookResolveIdOutput,
	HookResolveIdReturn, HookUsage, Plugin, PluginContext, SharedLoadPluginContext,
};
use rolldown::{Bundler, BundlerOptions, InputItem, OutputFormat};
use rustc_hash::FxHashMap;

/// A `rolldown` plugin serving every module source out of an in-memory map
/// keyed by the driver's own canonical module keys (`"main"`,
/// `"geometry/vec"`, ...) — never the filesystem.
#[derive(Debug)]
struct VirtualFsPlugin {
	sources: FxHashMap<String, String>,
}

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

fn is_valid_esm(source: &str) -> bool {
	let allocator = Allocator::default();
	let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
	!parsed.panicked && !parsed.diagnostics.has_errors()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn self_contained_entry_is_returned_without_rebundling() {
		let source = "function main() {}\nexport { main };\n";
		let sources = FxHashMap::from_iter([("main".to_string(), source.to_string())]);

		let js = bundle("main", sources).expect("self-contained entry should compile");

		assert_eq!(js, source);
	}

	#[test]
	fn invalid_self_contained_entry_still_reports_a_bundle_error() {
		let source = "function default() {}\nexport { default };\n";
		let sources = FxHashMap::from_iter([("main".to_string(), source.to_string())]);

		let result = bundle("main", sources);

		assert!(result.is_err(), "invalid ESM must not bypass validation");
	}

	// Regression pin (not a bug): a wave of review findings flagged rolldown's
	// tree-shaking as a "silent wrong-JS" risk — a dependency's top-level code
	// dropped whenever the importer never references what it exports (e.g. a
	// bare `import @/dep;` with no `with`-list). Investigation (see the IB2
	// fix-agent notes) showed that risk does NOT materialize: rolldown/oxc
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
}
