use crate::NymphCommand;
use crate::commands::not_implemented;
use crate::project_support::ManifestSelection;

#[derive(clap::Args)]
pub(crate) struct FormatCommand {}

impl NymphCommand for FormatCommand {
	fn run(&self, _manifest: &ManifestSelection) -> i32 {
		not_implemented("format")
	}
}
