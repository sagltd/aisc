mod git;
mod github;
mod process;
mod task;

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::PathBuf;

const HELP: &str = "aisc 0.1.0 — task-oriented Git operations\n\nstart NAME (--path PATH | --in-place)\ncontext [--limit N]\ndiff [--max-bytes N] -- [PATH...]\ncheckpoint -m MESSAGE -- PATH...\nverify [--timeout SECONDS] -- PROGRAM ARG...\nfinish\ngithub [--limit N]\n\nRun checkpoint → verify → finish in the task checkout. All results are JSON.\nPaths are literal files relative to the repository root. No automatic push, merge, or cleanup.\nVerification fingerprints tracked files, index, Git config, environment and executable; ignored files and external services are not hermetic. Configured Git hooks/signing run normally.\nGitHub is an explicit read-only gh pr status request; local context stays offline.";

fn main() {
    match run() {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("{}", json!({"error": format!("{error:#}")}));
            std::process::exit(1);
        }
    }
}

fn run() -> Result<serde_json::Value> {
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF-8 arguments are unsupported"))
        })
        .collect::<Result<Vec<_>>>()?;
    if args.is_empty() || args == ["--help"] || args == ["help"] || args == ["-h"] {
        return Ok(json!({"help": HELP}));
    }
    if args == ["--version"] {
        return Ok(json!({"version": env!("CARGO_PKG_VERSION")}));
    }
    let repo = git::Repo::discover()?;
    let tail = &args[1..];
    match args[0].as_str() {
        "start" => match tail {
            [name, option] if option == "--in-place" => task::start(&repo, name, None),
            [name, option, path] if option == "--path" => {
                task::start(&repo, name, Some(PathBuf::from(path)))
            }
            _ => bail!("usage: start NAME (--path PATH | --in-place)"),
        },
        "context" => repo.context(number_option(tail, "--limit", 30, 1_000)?),
        "github" => github::overview(&repo, number_option(tail, "--limit", 10, 100)?),
        "diff" => {
            let separator = tail
                .iter()
                .position(|arg| arg == "--")
                .context("usage: diff [--max-bytes N] -- [PATH...]")?;
            repo.diff(
                &tail[separator + 1..],
                number_option(&tail[..separator], "--max-bytes", 8_192, 1_048_576)?,
            )
        }
        "checkpoint" => match tail {
            [option, message, separator, paths @ ..]
                if option == "-m" && separator == "--" && !paths.is_empty() =>
            {
                task::checkpoint(&repo, message, paths)
            }
            _ => bail!("usage: checkpoint -m MESSAGE -- PATH..."),
        },
        "verify" => {
            let separator = tail
                .iter()
                .position(|arg| arg == "--")
                .context("usage: verify [--timeout SECONDS] -- PROGRAM ARG...")?;
            let timeout = number_option(&tail[..separator], "--timeout", 300, 3_600)?;
            let command = &tail[separator + 1..];
            if command.is_empty() {
                bail!("verification requires a program after --");
            }
            task::verify(&repo, command, timeout)
        }
        "finish" if tail.is_empty() => task::finish(&repo),
        _ => bail!("unknown command or arguments; run aisc --help"),
    }
}

fn number_option(args: &[String], option: &str, default: usize, maximum: usize) -> Result<usize> {
    let value = match args {
        [] => return Ok(default),
        [name, value] if name == option => {
            value.parse::<usize>().context("invalid positive integer")?
        }
        _ => bail!("expected optional {option} N"),
    };
    if value == 0 || value > maximum {
        bail!("{option} must be between 1 and {maximum}");
    }
    Ok(value)
}
