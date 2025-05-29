mod commands;

use anyhow::Result;
use clap::Parser;
use nu_ansi_term::Color::Green;

fn main() -> Result<()> {
    let app = Xtask::parse();
    app.run()
}

#[derive(Debug, clap::Parser)]
#[structopt(
    name = "xtask",
    about = "Workflows used locally and in CI for developing the Apollo MCP Server"
)]
struct Xtask {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Produce or consume changesets
    #[command(subcommand)]
    Changeset(commands::changeset::Command),
}

impl Xtask {
    pub fn run(&self) -> Result<()> {
        match &self.command {
            Command::Changeset(command) => command.run(),
        }?;
        eprintln!("{}", Green.bold().paint("Success!"));
        Ok(())
    }
}
