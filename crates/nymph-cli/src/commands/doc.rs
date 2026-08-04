use crate::NymphCommand;
use crate::commands::not_implemented;
use crate::project_support::ManifestSelection;

#[derive(clap::Args)]
pub(crate) struct DocCommand {}

impl NymphCommand for DocCommand {
	fn run(&self, _manifest: &ManifestSelection) -> i32 {
		not_implemented("doc")
	}
}
