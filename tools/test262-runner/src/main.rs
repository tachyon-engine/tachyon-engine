#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::{env, fs, path::PathBuf, process::ExitCode};

use test262_runner::{StubAdapter, Test262Config, suite::RunOptions};

/// Reads explicit config/checkout arguments and emits a versioned JSON report to stdout.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("test262-runner: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Implements the initial engine-neutral stub command without hiding path or policy defaults.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let command = args.next().ok_or(USAGE)?;
    if command != "run-stub" {
        return Err(USAGE.into());
    }
    let config_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let checkout = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut options = RunOptions::default();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--serial") => options.parallel = false,
            Some("--parallel") => options.parallel = true,
            Some("--seed") => {
                let value = args.next().ok_or("--seed requires an unsigned integer")?;
                options.seed = Some(
                    value
                        .to_str()
                        .ok_or("--seed must be valid UTF-8")?
                        .parse()?,
                );
            }
            Some("--filter") => {
                let value = args.next().ok_or("--filter requires a substring")?;
                options.filter = Some(value.to_string_lossy().into_owned().into_boxed_str());
            }
            _ if options.selector.is_none() => options.selector = Some(PathBuf::from(argument)),
            _ => return Err(USAGE.into()),
        }
    }
    let config_source = fs::read_to_string(config_path)?;
    let config = Test262Config::parse(&config_source)?;
    let report = test262_runner::suite::run_checkout(
        &checkout,
        &config,
        &StubAdapter::unsupported(),
        &options,
    )?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
    println!();
    Ok(())
}

const USAGE: &str = "usage: test262-runner run-stub <config> <checkout> [test/path-or-file] [--filter text] [--seed n] [--serial|--parallel]";
