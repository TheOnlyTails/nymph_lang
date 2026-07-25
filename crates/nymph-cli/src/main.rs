#![warn(clippy::all)]

use std::path::PathBuf;

use clap::Parser;

use crate::commands::{
	build::BuildCommand, check::CheckCommand, doc::DocCommand, format::FormatCommand,
	new::NewCommand, repl::ReplCommand, run::RunCommand,
};

mod commands;
mod compile_guard;
pub mod config;
mod project_support;

pub(crate) trait NymphCommand {
	/// Run the command and return the process exit code.
	fn run(&self) -> i32;
}

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(arg_required_else_help = true)]
pub(crate) struct NymphCli {
	#[arg(short, long, value_name = "FILE")]
	config: Option<PathBuf>,

	#[command(subcommand)]
	command: Option<NymphCommands>,
}

#[derive(clap::Subcommand)]
enum NymphCommands {
	Build(BuildCommand),
	Check(CheckCommand),
	Doc(DocCommand),
	Format(FormatCommand),
	New(NewCommand),
	Repl(ReplCommand),
	Run(RunCommand),
}

impl NymphCommands {
	fn run(&self) -> i32 {
		match self {
			NymphCommands::Build(cmd) => cmd.run(),
			NymphCommands::Check(cmd) => cmd.run(),
			NymphCommands::Doc(cmd) => cmd.run(),
			NymphCommands::Format(cmd) => cmd.run(),
			NymphCommands::New(cmd) => cmd.run(),
			NymphCommands::Repl(cmd) => cmd.run(),
			NymphCommands::Run(cmd) => cmd.run(),
		}
	}
}

fn main() -> anyhow::Result<()> {
	let cli = NymphCli::parse();

	let code = cli.command.map_or(2, |command| command.run());
	std::process::exit(code);
}
