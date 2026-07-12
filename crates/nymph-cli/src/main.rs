use clap::Parser;

use crate::commands::{
	add::AddCommand, build::BuildCommand, check::CheckCommand, doc::DocCommand,
	format::FormatCommand, install::InstallCommand, new::NewCommand, remove::RemoveCommand,
	repl::ReplCommand, run::RunCommand, search::SearchCommand,
};

mod commands;
pub mod config;

pub(crate) trait NymphCommand {
	fn run(&self);
}

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct NymphCli {
	#[command(subcommand)]
	command: Option<NymphCommands>,
}

#[derive(clap::Subcommand)]
enum NymphCommands {
	Add(AddCommand),
	Build(BuildCommand),
	Check(CheckCommand),
	Doc(DocCommand),
	Format(FormatCommand),
	Install(InstallCommand),
	New(NewCommand),
	Remove(RemoveCommand),
	Repl(ReplCommand),
	Run(RunCommand),
	Search(SearchCommand),
}

fn main() {
	let cli = NymphCli::parse();
}
