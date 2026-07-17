#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::{env, fs, path::PathBuf, process::ExitCode};

use benchmark_runner::{BenchmarkConfig, load_corpus};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("benchmark-runner: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Verifies config, provenance, licenses, and every checked-in script hash without taking samples.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref().and_then(std::ffi::OsStr::to_str) != Some("verify") {
        return Err(USAGE.into());
    }
    let config_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let workspace = PathBuf::from(args.next().ok_or(USAGE)?);
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    let config = BenchmarkConfig::parse(&fs::read_to_string(config_path)?)?;
    let corpus = load_corpus(&workspace, &config)?;
    let scripts = corpus
        .iter()
        .map(|script| {
            serde_json::json!({
                "id": script.config.id,
                "sha256": script.config.sha256,
                "license": script.config.license,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_writer_pretty(
        std::io::stdout().lock(),
        &serde_json::json!({ "schema_version": 1, "scripts": scripts }),
    )?;
    println!();
    Ok(())
}

const USAGE: &str = "usage: benchmark-runner verify <benchmark_config.toml> <workspace>";
