use std::path::PathBuf;

use crate::NymphCommand;
use crate::project_support::{ManifestSelection, ProjectOperation};

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

	/// Check with the release compiler profile.
	#[arg(long)]
	release: bool,
}

impl NymphCommand for CheckCommand {
	fn run(&self, manifest: &ManifestSelection) -> i32 {
		let profile = if self.release {
			nymph_compiler::BuildProfile::Release
		} else {
			nymph_compiler::BuildProfile::Development
		};
		let operation = match ProjectOperation::resolve(self.file.as_deref(), manifest, profile) {
			Some(operation) => operation,
			None => return 1,
		};
		let diagnostics = operation.check_selected_mode();
		if diagnostics.is_empty() {
			println!("ok");
			return 0;
		}
		eprint!("{}", operation.render(&diagnostics));
		i32::from(diagnostics.iter().any(|d| d.diag.is_error()))
	}
}
