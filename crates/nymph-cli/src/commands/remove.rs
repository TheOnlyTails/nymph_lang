use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct RemoveCommand {}

impl NymphCommand for RemoveCommand {
	fn run(&self) -> i32 {
		not_implemented("remove")
	}
}
