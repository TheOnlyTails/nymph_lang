use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct AddCommand {}

impl NymphCommand for AddCommand {
	fn run(&self) -> i32 {
		not_implemented("add")
	}
}
