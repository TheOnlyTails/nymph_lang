use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct FormatCommand {}

impl NymphCommand for FormatCommand {
	fn run(&self) -> i32 {
		not_implemented("format")
	}
}
