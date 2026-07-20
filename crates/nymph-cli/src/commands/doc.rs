use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct DocCommand {}

impl NymphCommand for DocCommand {
	fn run(&self) -> i32 {
		not_implemented("doc")
	}
}
