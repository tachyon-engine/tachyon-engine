#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::{env, fs, path::PathBuf, process::ExitCode};

use test262_runner::{
    RunOptions, RunReport, TachyonAdapter, Test262Config, compare_reports, run_checkout,
};

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

/// Dispatches explicit runner commands without relying on current-directory discovery.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let command = args.next().ok_or(USAGE)?;
    match command.to_str() {
        Some("run-tachyon") => run_tachyon(args),
        Some("compare") => compare(args),
        _ => Err(USAGE.into()),
    }
}

/// Parses suite selection/scheduling flags and emits a complete versioned Tachyon report.
fn run_tachyon(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let checkout = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut options = RunOptions::default();
    let mut summary_only = false;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--serial") => options.parallel = false,
            Some("--parallel") => options.parallel = true,
            Some("--summary-only") => summary_only = true,
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
    let report = run_checkout(&checkout, &config, &TachyonAdapter, &options)?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if summary_only {
        serde_json::to_writer_pretty(&mut output, &report.summary)?;
    } else {
        serde_json::to_writer_pretty(&mut output, &report)?;
    }
    println!();
    Ok(())
}

/// Compares two reports with identical schema and release-policy fingerprints.
fn compare(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn std::error::Error>> {
    let base_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let new_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let markdown = match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        None => false,
        Some("--markdown") => true,
        _ => return Err(USAGE.into()),
    };
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    let base: RunReport = serde_json::from_slice(&fs::read(base_path)?)?;
    let new: RunReport = serde_json::from_slice(&fs::read(new_path)?)?;
    let diff = compare_reports(&base, &new)?;
    if markdown {
        print!("{}", diff.to_markdown());
    } else {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &diff)?;
        println!();
    }
    Ok(())
}

const USAGE: &str = "usage:\n  test262-runner run-tachyon <config> <checkout> [test/path-or-file] [--filter text] [--seed n] [--serial|--parallel] [--summary-only]\n  test262-runner compare <base.json> <new.json> [--markdown]";
