#![warn(clippy::all)]

use std::path::PathBuf;

use clap::Parser;

use crate::commands::{
	build::BuildCommand, check::CheckCommand, doc::DocCommand, format::FormatCommand,
	new::NewCommand, repl::ReplCommand, run::RunCommand,
};

mod commands;
mod compile_guard;
mod project_support;

pub(crate) trait NymphCommand {
	/// Run the command and return the process exit code.
	fn run(&self, manifest: &project_support::ManifestSelection) -> i32;
}

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(arg_required_else_help = true)]
pub(crate) struct NymphCli {
	/// Use exactly this project manifest instead of discovering nymph.toml.
	#[arg(long, global = true, value_name = "PATH")]
	manifest: Option<PathBuf>,

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
	fn run(&self, manifest: &project_support::ManifestSelection) -> i32 {
		match self {
			NymphCommands::Build(cmd) => cmd.run(manifest),
			NymphCommands::Check(cmd) => cmd.run(manifest),
			NymphCommands::Doc(cmd) => cmd.run(manifest),
			NymphCommands::Format(cmd) => cmd.run(manifest),
			NymphCommands::New(cmd) => cmd.run(manifest),
			NymphCommands::Repl(cmd) => cmd.run(manifest),
			NymphCommands::Run(cmd) => cmd.run(manifest),
		}
	}
}

fn main() -> anyhow::Result<()> {
	let cli = NymphCli::parse();
	let manifest = project_support::ManifestSelection::from(cli.manifest);

	let code = cli.command.map_or(2, |command| command.run(&manifest));
	std::process::exit(code);
}
