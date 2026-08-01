use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::NymphCommand;
use crate::compile_guard::{guarded, unsupported_feature_message};
use crate::project_support::{self, TargetIntent, fs_loader, render_project_diagnostics};

/// compile a Nymph source file to a JavaScript module.
#[derive(clap::Args)]
pub(crate) struct BuildCommand {
	/// Path to the `.nym` source file to build (defaults to project build.entry).
	file: Option<PathBuf>,

	/// Output path for the emitted JavaScript (defaults to `<input>.mjs`)
	#[arg(short, long, value_name = "FILE")]
	output: Option<PathBuf>,
}

impl NymphCommand for BuildCommand {
	fn run(&self) -> i32 {
		let target = match project_support::resolve(self.file.as_deref()) {
			Ok(target) => target,
			Err(error) => {
				eprintln!("error: {error}");
				return 1;
			}
		};
		let output_path = self
			.output
			.clone()
			.unwrap_or_else(|| target.file.with_extension("mjs"));
		let load = fs_loader(target.src_root.clone());
		let result = guarded(|| match target.intent {
			TargetIntent::Entry => nymph_compiler::compile_project_with_std(
				&target.entry_key,
				&load,
				&nymph_compiler::embedded_std_provider,
			),
			TargetIntent::Library => nymph_compiler::compile_project_library_with_std(
				&target.entry_key,
				&load,
				&nymph_compiler::embedded_std_provider,
			),
		});
		match result {
			Ok(Ok(compiled)) => match write_output_atomically(&output_path, &compiled.js) {
				Ok(()) => 0,
				Err(err) => {
					eprintln!("error: could not write {}: {err}", output_path.display());
					1
				}
			},
			Ok(Err(diags)) => {
				eprint!(
					"{}",
					render_project_diagnostics(&diags, &target.src_root, &load)
				);
				1
			}
			Err(payload) => {
				eprintln!("{}", unsupported_feature_message(&payload));
				1
			}
		}
	}
}

/// Write `contents` to `output_path` atomically: write to a fresh, uniquely
/// named temp file in `output_path`'s own directory, then rename it over the
/// target. A rename within one directory is a single filesystem operation —
/// there's no window where `output_path` is truncated or partially written.
/// On any error, the temp file is best-effort cleaned up and `output_path`
/// itself is never touched.
fn write_output_atomically(output_path: &Path, contents: &str) -> std::io::Result<()> {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);

	let dir = output_path
		.parent()
		.filter(|p| !p.as_os_str().is_empty())
		.unwrap_or_else(|| Path::new("."));
	let file_name = output_path
		.file_name()
		.map_or_else(|| "out".to_string(), |n| n.to_string_lossy().into_owned());
	let temp_path = dir.join(format!(
		".{file_name}.nymph-build-tmp-{}-{unique}",
		std::process::id()
	));

	if let Err(err) = std::fs::write(&temp_path, contents) {
		let _ = std::fs::remove_file(&temp_path);
		return Err(err);
	}

	if let Err(err) = std::fs::rename(&temp_path, output_path) {
		let _ = std::fs::remove_file(&temp_path);
		return Err(err);
	}

	Ok(())
}
