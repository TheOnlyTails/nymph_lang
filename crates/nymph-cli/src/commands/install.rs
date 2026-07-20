use crate::NymphCommand;
use crate::commands::not_implemented;

#[derive(clap::Args)]
pub(crate) struct InstallCommand {}

impl NymphCommand for InstallCommand {
	fn run(&self) -> i32 {
		not_implemented("install")
	}
}
