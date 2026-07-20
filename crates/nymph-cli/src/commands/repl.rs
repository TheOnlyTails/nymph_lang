use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct ReplCommand {}

impl NymphCommand for ReplCommand {
	fn run(&self) -> i32 {
		not_implemented("repl")
	}
}
