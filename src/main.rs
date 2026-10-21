mod cli;
mod context;
mod git;
mod github;
mod gitlab;
mod output;
mod process;
mod task;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};
use cli::{Cli, Operation};
use output::Presentation;
use serde_json::{Value, json};

fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let presentation = Presentation::from_arguments(&arguments);
    let mut parser = Cli::command().color(presentation.clap_color());
    let matches = match parser.try_get_matches_from_mut(&arguments) {
        Ok(matches) => matches,
        Err(error) => {
            let code = error.exit_code();
            match error.kind() {
                clap::error::ErrorKind::DisplayHelp => println!(
                    "{}",
                    presentation.help(
                        &error.render().to_string(),
                        &error.render().ansi().to_string()
                    )
                ),
                clap::error::ErrorKind::DisplayVersion => println!(
                    "{}",
                    presentation.success("version", &json!({"version": env!("CARGO_PKG_VERSION")}))
                ),
                _ => eprintln!("{}", presentation.error(&error.render().to_string())),
            }
            std::process::exit(code);
        }
    };
    let command = match Cli::from_arg_matches(&matches) {
        Ok(parsed) => parsed.command,
        Err(error) => {
            eprintln!("{}", presentation.error(&error.to_string()));
            std::process::exit(2);
        }
    };
    let Some(command) = command else {
        let help = parser.render_help();
        println!(
            "{}",
            presentation.help(&help.to_string(), &help.ansi().to_string())
        );
        return;
    };
    let operation = command.name();
    match run(command) {
        Ok(value) => println!("{}", presentation.success(operation, &value)),
        Err(error) => {
            eprintln!("{}", presentation.error(&format!("{error:#}")));
            std::process::exit(1);
        }
    }
}

fn run(command: Operation) -> Result<Value> {
    if let Operation::Context { limit } = command {
        return context::read(&std::env::current_dir()?, limit);
    }
    let repo = git::Repo::discover()?;
    match command {
        Operation::Start(start) => task::start(&repo, &start.name, start.path),
        Operation::Context { .. } => unreachable!("context is handled before checkout discovery"),
        Operation::Diff { max_bytes, paths } => repo.diff(&paths, max_bytes),
        Operation::Checkpoint { message, paths } => task::checkpoint(&repo, &message, &paths),
        Operation::Verify { timeout, command } => task::verify(&repo, &command, timeout),
        Operation::Finish => task::finish(&repo),
        Operation::Github { limit } => github::overview(&repo, limit),
        Operation::Gitlab { limit } => gitlab::overview(&repo, limit),
    }
}
