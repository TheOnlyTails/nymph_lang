use crate::NymphCommand;
use crate::project_support::{ManifestSelection, ProjectOperation};

/// Print a project's fully expanded module as formatted Nymph source.
#[derive(clap::Args)]
#[command(
	after_long_help = "Examples:\n  nymph expand main\n  nymph expand network/http\n  nymph --manifest ../app/nymph.toml expand generated/routes"
)]
pub(crate) struct ExpandCommand {
	/// Canonical module path relative to package.src, without @/, ./, or .nym.
	#[arg(value_name = "MODULE_PATH")]
	module: String,
}

impl NymphCommand for ExpandCommand {
	fn run(&self, manifest: &ManifestSelection) -> i32 {
		let operation = match ProjectOperation::resolve_module(&self.module, manifest) {
			Some(operation) => operation,
			None => return 1,
		};
		let Some(report) = operation.expand() else {
			return 1;
		};
		if !report.diagnostics.is_empty() {
			eprint!("{}", operation.render(&report.diagnostics));
		}
		let Some(source) = report.source else {
			return 1;
		};
		print!("{source}");
		0
	}
}
