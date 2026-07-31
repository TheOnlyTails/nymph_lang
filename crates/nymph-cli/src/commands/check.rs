use std::path::PathBuf;

use crate::NymphCommand;
use crate::project_support::{self, fs_loader, render_project_diagnostics};

/// `nymph check <file>` — parse and type-check a Nymph source file. Prints
/// "ok" and exits 0 when there are no diagnostics; otherwise renders every
/// diagnostic (severity, `file:line:col`, message, source excerpt) to stderr
/// and exits nonzero only if at least one diagnostic is an error (warnings
/// alone still exit 0).
///
/// Uses entry mode (requiring a valid top-level `main`) iff `file`'s stem is
/// literally `main` — the project's entry module today; every other file
/// stem is checked as a plain library module.
// TODO: the entry module becomes manifest-configurable later (see the
// checker-phase main() validation plan, GG4); for now it's hardcoded to the
// `main` file stem.
#[derive(clap::Args)]
pub(crate) struct CheckCommand {
	/// Path to the `.nym` source file to check.
	file: PathBuf,
}

impl NymphCommand for CheckCommand {
	fn run(&self) -> i32 {
		let is_entry = self.file.file_stem() == Some(std::ffi::OsStr::new("main"));

		let project = match project_support::detect(&self.file) {
			Ok(project) => project,
			Err(error) => {
				eprintln!("error: {error}");
				return 1;
			}
		};
		if let Some(project) = project {
			let load = fs_loader(project.src_root);
			let diagnostics = if is_entry {
				nymph_compiler::check_project(&project.entry_key, &load)
			} else {
				nymph_compiler::check_project_library(&project.entry_key, &load)
			};
			if diagnostics.is_empty() {
				println!("ok");
				return 0;
			}
			eprint!("{}", render_project_diagnostics(&diagnostics, &load));
			return i32::from(diagnostics.iter().any(|d| d.diag.is_error()));
		}

		let source = match std::fs::read_to_string(&self.file) {
			Ok(source) => source,
			Err(err) => {
				eprintln!("error: could not read {}: {err}", self.file.display());
				return 1;
			}
		};

		let path = self.file.display().to_string();
		let diagnostics = if is_entry {
			nymph_compiler::check_entry(&source, &path)
		} else {
			nymph_compiler::check(&source, &path)
		};

		if diagnostics.is_empty() {
			println!("ok");
			return 0;
		}

		eprint!(
			"{}",
			nymph_diagnostics::render(&path, &source, &diagnostics)
		);

		i32::from(diagnostics.iter().any(nymph_compiler::Diagnostic::is_error))
	}
}
