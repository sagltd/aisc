mod cli;
mod context;
mod git;
mod github;
mod gitlab;
mod history;
mod output;
mod process;
mod runner;
mod signals;
mod store;
mod task;
mod worktree;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};
use cli::{Cli, Operation};
use history::{HistoryError, Outcome};
use output::Presentation;
use serde_json::Value;
use std::io::Write;

/// Prints a line to stdout; a closed pipe (`aisc log | head -1`) ends the process quietly.
fn emit(line: &str) {
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{line}").and_then(|()| stdout.flush()) {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        std::process::exit(1);
    }
}

fn emit_error(line: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
}

fn main() {
    signals::install();
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let presentation = Presentation::from_arguments(&arguments);
    let mut parser = Cli::command().color(presentation.clap_color());
    let matches = match parser.try_get_matches_from_mut(&arguments) {
        Ok(matches) => matches,
        Err(error) => {
            let code = error.exit_code();
            match error.kind() {
                clap::error::ErrorKind::DisplayHelp => emit(&presentation.help(
                    &error.render().to_string(),
                    &error.render().ansi().to_string(),
                )),
                // Help and version are for people (and agents reading them): always plain text.
                clap::error::ErrorKind::DisplayVersion => {
                    emit(&format!("aisc {}", env!("CARGO_PKG_VERSION")))
                }
                _ => emit_error(&presentation.error(&error.render().to_string())),
            }
            std::process::exit(code);
        }
    };
    let command = match Cli::from_arg_matches(&matches) {
        Ok(parsed) => parsed.command.map(|mut command| {
            command.attach_groups(&matches);
            command
        }),
        Err(error) => {
            emit_error(&presentation.error(&error.to_string()));
            std::process::exit(2);
        }
    };
    let Some(command) = command else {
        let help = parser.render_help();
        emit(&presentation.help(&help.to_string(), &help.ansi().to_string()));
        return;
    };
    let operation = command.name();
    let presentation = presentation.for_operation(&command);
    let history = command.prefers_text();
    let exit;
    match run(&command) {
        Ok(outcome) => {
            exit = outcome.exit;
            emit(&presentation.outcome(operation, &outcome));
        }
        Err(error) => {
            if let Some(interrupted) = error.downcast_ref::<signals::Interrupted>() {
                std::process::exit(signals::exit_code(interrupted.0));
            }
            let (code, hint) = match error.downcast_ref::<HistoryError>() {
                Some(failure) => (Some(failure.code), failure.hint.clone()),
                None => (history.then_some(history::Code::Git), None),
            };
            emit_error(&presentation.error_with(&format!("{error:#}"), code, hint.as_deref()));
            exit = code.map_or(1, history::Code::exit);
        }
    }
    // A signal received during a critical section is honored once the section has completed.
    if let Some(signal) = signals::pending() {
        std::process::exit(signals::exit_code(signal));
    }
    if exit != 0 {
        std::process::exit(exit);
    }
}

fn run(command: &Operation) -> Result<Outcome> {
    if let Some(result) = history::run(command) {
        return result;
    }
    let value = run_task(command)?;
    Ok(Outcome::plain(value))
}

fn run_task(command: &Operation) -> Result<Value> {
    if let Operation::Context { limit } = command {
        return context::read(&std::env::current_dir()?, *limit);
    }
    let repo = git::Repo::discover()?;
    match command {
        Operation::Start(start) => {
            task::start(&repo, &start.name, start.path.clone(), &start.sparse)
        }
        Operation::Context { .. } => unreachable!("context is handled before checkout discovery"),
        Operation::Diff { max_bytes, paths } => repo.diff(paths, *max_bytes),
        Operation::Checkpoint { message, paths } => task::checkpoint(&repo, message, paths),
        Operation::Verify { timeout, command } => task::verify(&repo, command, *timeout),
        Operation::Finish {
            keep,
            delete_ignored,
        } => task::finish(&repo, *keep, *delete_ignored),
        Operation::Github { limit } => github::overview(&repo, *limit),
        Operation::Gitlab { limit } => gitlab::overview(&repo, *limit),
        _ => unreachable!("history verbs are dispatched before task verbs"),
    }
}
