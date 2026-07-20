use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct NewCommand {}

impl NymphCommand for NewCommand {
	fn run(&self) -> i32 {
		not_implemented("new")
	}
}
