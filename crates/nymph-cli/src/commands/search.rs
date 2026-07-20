use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct SearchCommand {}

impl NymphCommand for SearchCommand {
	fn run(&self) -> i32 {
		not_implemented("search")
	}
}
