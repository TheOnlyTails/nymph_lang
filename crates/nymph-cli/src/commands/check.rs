use std::path::PathBuf;

use crate::NymphCommand;
use crate::project_support::{
	self, TargetIntent, TargetKind, fs_loader, render_project_diagnostics,
};

/// `nymph check [file]` — parse and type-check a Nymph source file. Prints
/// "ok" and exits 0 when there are no diagnostics; otherwise renders every
/// diagnostic (severity, `file:line:col`, message, source excerpt) to stderr
/// and exits nonzero only if at least one diagnostic is an error (warnings
/// alone still exit 0).
///
/// With no file, the nearest project's manifest `build.entry` is used. In a
/// project, that manifest-selected module is checked in entry mode and other
/// explicit files are checked as libraries. Loose files are libraries.
#[derive(clap::Args)]
pub(crate) struct CheckCommand {
	/// Path to the `.nym` source file to check (defaults to project build.entry).
	file: Option<PathBuf>,
}

impl NymphCommand for CheckCommand {
	fn run(&self) -> i32 {
		let target = match project_support::resolve(self.file.as_deref()) {
			Ok(target) => target,
			Err(error) => {
				eprintln!("error: {error}");
				return 1;
			}
		};
		if target.kind == TargetKind::Project {
			let load = fs_loader(target.src_root);
			let diagnostics = match target.intent {
				TargetIntent::Entry => nymph_compiler::check_project(&target.entry_key, &load),
				TargetIntent::Library => nymph_compiler::check_project_library(&target.entry_key, &load),
			};
			if diagnostics.is_empty() {
				println!("ok");
				return 0;
			}
			eprint!("{}", render_project_diagnostics(&diagnostics, &load));
			return i32::from(diagnostics.iter().any(|d| d.diag.is_error()));
		}

		let source = match std::fs::read_to_string(&target.file) {
			Ok(source) => source,
			Err(err) => {
				eprintln!("error: could not read {}: {err}", target.file.display());
				return 1;
			}
		};

		let path = target.file.display().to_string();
		let diagnostics = match target.intent {
			TargetIntent::Entry => nymph_compiler::check_entry(&source, &path),
			TargetIntent::Library => nymph_compiler::check(&source, &path),
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
