#![allow(clippy::disallowed_methods, clippy::disallowed_types)]
//! Repository maintenance commands. Host I/O is deliberately confined to this tool crate.

use std::{env, fs, path::Path, process::Command};

use benchmark_runner::{
    BenchmarkConfig, BenchmarkReport, compare_reports as compare_benchmarks, load_corpus,
};
use test262_runner::{
    RunOptions, RunReport, StubAdapter, Test262Config, compare_reports, run_checkout,
};

const ENGINE_CRATES: [&str; 6] = [
    "tachyon-value",
    "tachyon-bytecode",
    "tachyon-gc",
    "tachyon-compiler",
    "tachyon-vm",
    "tachyon",
];

const FORBIDDEN_SOURCE_PATTERNS: [&str; 18] = [
    "std::fs",
    "std::io",
    "std::net",
    "std::process",
    "std::env",
    "std::thread",
    "TcpListener",
    "TcpStream",
    "UdpSocket",
    "Command",
    "OpenOptions",
    "println!",
    "eprintln!",
    "print!(",
    "eprint!(",
    "File::",
    "read_to_string",
    "read_dir",
];

/// Dispatches repository maintenance commands while keeping their host I/O outside engine crates.
fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [command, subcommand] if command == "architecture" && subcommand == "check" => {
            check_architecture()
        }
        [command, subcommand] if command == "test262" && subcommand == "fetch" => fetch_test262(),
        [command, subcommand, rest @ ..] if command == "test262" && subcommand == "run" => {
            run_test262(rest)
        }
        [command, subcommand, base, new] if command == "test262" && subcommand == "compare" => {
            compare_test262(base, new, false)
        }
        [command, subcommand, base, new, flag]
            if command == "test262" && subcommand == "compare" && flag == "--markdown" =>
        {
            compare_test262(base, new, true)
        }
        [command, subcommand] if command == "bench" && subcommand == "verify" => {
            verify_benchmarks()
        }
        [command, subcommand, base, candidate] if command == "bench" && subcommand == "compare" => {
            compare_benchmark_reports(base, candidate, false)
        }
        [command, subcommand, base, candidate, flag]
            if command == "bench" && subcommand == "compare" && flag == "--markdown" =>
        {
            compare_benchmark_reports(base, candidate, true)
        }
        _ => Err(USAGE.to_owned()),
    };

    if let Err(message) = result {
        eprintln!("xtask: {message}");
        std::process::exit(1);
    }
}

const USAGE: &str = "usage:\n  cargo xtask architecture check\n  cargo xtask test262 fetch\n  cargo xtask test262 run [test/path-or-file] [--filter text] [--seed n] [--serial|--parallel]\n  cargo xtask test262 compare <base.json> <new.json> [--markdown]\n  cargo xtask bench verify\n  cargo xtask bench compare <base.json> <candidate.json> [--markdown]";

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn test262_config() -> Result<Test262Config, String> {
    let path = workspace_root().join("test262_config.toml");
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let config = Test262Config::parse(&source).map_err(|error| error.to_string())?;
    config.validate().map_err(str::to_owned)?;
    Ok(config)
}

/// Fetches exactly the configured commit while preserving dirty or differently sourced checkouts.
fn fetch_test262() -> Result<(), String> {
    let workspace = workspace_root();
    let checkout = workspace.join("test262");
    let config = test262_config()?;
    if !checkout.exists() {
        run_command(Command::new("git").arg("init").arg(&checkout))?;
        run_command(
            Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(["remote", "add", "origin"])
                .arg(&*config.repository),
        )?;
    }
    let remote = command_output(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["remote", "get-url", "origin"]),
    )?;
    if remote.trim() != &*config.repository {
        return Err(format!(
            "test262 origin mismatch: expected {}, got {}",
            config.repository,
            remote.trim()
        ));
    }
    if checkout.join(".git").exists() {
        let output = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["diff-index", "--quiet", "HEAD", "--"])
            .output()
            .map_err(|error| format!("failed to inspect test262 checkout: {error}"))?;
        if !matches!(output.status.code(), Some(0 | 128)) {
            return Err("test262 checkout has tracked modifications".to_owned());
        }
    }
    run_command(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["fetch", "--depth", "1", "origin"])
            .arg(&*config.commit),
    )?;
    run_command(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["checkout", "--detach"])
            .arg(&*config.commit),
    )?;
    let actual = command_output(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["rev-parse", "HEAD"]),
    )?;
    if actual.trim() == &*config.commit {
        Ok(())
    } else {
        Err(format!(
            "test262 checkout revision mismatch after fetch: expected {}, got {}",
            config.commit,
            actual.trim()
        ))
    }
}

/// Runs the engine-neutral adapter with deterministic selection flags and writes JSON to stdout.
fn run_test262(arguments: &[String]) -> Result<(), String> {
    let mut options = RunOptions::default();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--serial" => options.parallel = false,
            "--parallel" => options.parallel = true,
            "--seed" => {
                index += 1;
                let value = arguments.get(index).ok_or("--seed requires an integer")?;
                options.seed = Some(value.parse().map_err(|_| "invalid --seed value")?);
            }
            "--filter" => {
                index += 1;
                let value = arguments.get(index).ok_or("--filter requires text")?;
                options.filter = Some(value.clone().into_boxed_str());
            }
            selector if options.selector.is_none() => options.selector = Some(selector.into()),
            _ => return Err(USAGE.to_owned()),
        }
        index += 1;
    }
    let report = run_checkout(
        &workspace_root().join("test262"),
        &test262_config()?,
        &StubAdapter::unsupported(),
        &options,
    )
    .map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)
        .map_err(|error| error.to_string())?;
    println!();
    Ok(())
}

fn compare_test262(base: &str, new: &str, markdown: bool) -> Result<(), String> {
    let base: RunReport = serde_json::from_slice(
        &fs::read(base).map_err(|error| format!("failed to read {base}: {error}"))?,
    )
    .map_err(|error| format!("failed to parse {base}: {error}"))?;
    let new: RunReport = serde_json::from_slice(
        &fs::read(new).map_err(|error| format!("failed to read {new}: {error}"))?,
    )
    .map_err(|error| format!("failed to parse {new}: {error}"))?;
    let diff = compare_reports(&base, &new).map_err(|error| error.to_string())?;
    if markdown {
        print!("{}", diff.to_markdown());
    } else {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &diff)
            .map_err(|error| error.to_string())?;
        println!();
    }
    Ok(())
}

/// Verifies benchmark configuration, licenses, provenance, and content-addressed corpus bytes.
fn verify_benchmarks() -> Result<(), String> {
    let workspace = workspace_root();
    let config_path = workspace.join("benchmark_config.toml");
    let source = fs::read_to_string(&config_path)
        .map_err(|error| format!("failed to read {}: {error}", config_path.display()))?;
    let config = BenchmarkConfig::parse(&source).map_err(|error| error.to_string())?;
    let corpus = load_corpus(&workspace, &config).map_err(|error| error.to_string())?;
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
    )
    .map_err(|error| error.to_string())?;
    println!();
    Ok(())
}

/// Compares matched benchmark reports and emits either versioned JSON or a concise Markdown summary.
fn compare_benchmark_reports(base: &str, candidate: &str, markdown: bool) -> Result<(), String> {
    let base_report: BenchmarkReport = serde_json::from_slice(
        &fs::read(base).map_err(|error| format!("failed to read {base}: {error}"))?,
    )
    .map_err(|error| format!("failed to parse {base}: {error}"))?;
    let candidate_report: BenchmarkReport = serde_json::from_slice(
        &fs::read(candidate).map_err(|error| format!("failed to read {candidate}: {error}"))?,
    )
    .map_err(|error| format!("failed to parse {candidate}: {error}"))?;
    let comparison =
        compare_benchmarks(&base_report, &candidate_report).map_err(|error| error.to_string())?;
    if markdown {
        print!("{}", comparison.to_markdown());
    } else {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &comparison)
            .map_err(|error| error.to_string())?;
        println!();
    }
    Ok(())
}

fn run_command(command: &mut Command) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("failed to run command: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn command_output(command: &mut Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("failed to run command: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Rejects host-runtime APIs, build scripts, and invalid local dependency directions in engine crates.
fn check_architecture() -> Result<(), String> {
    let workspace = workspace_root();
    let mut violations = Vec::new();

    for crate_name in ENGINE_CRATES {
        let crate_root = workspace.join("crates").join(crate_name);
        let source_root = crate_root.join("src");
        scan_source_tree(&source_root, &mut violations)?;

        if crate_root.join("build.rs").exists() {
            violations.push(format!(
                "{}: engine crates may not define build.rs",
                crate_root.display()
            ));
        }

        let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
            .map_err(|error| format!("failed to read {crate_name} manifest: {error}"))?;
        check_manifest(crate_name, &manifest, &mut violations);
    }

    if violations.is_empty() {
        return Ok(());
    }

    Err(format!(
        "architecture violations:\n{}",
        violations.join("\n")
    ))
}

/// Recursively scans production Rust sources; integration tests remain outside this tree by convention.
fn scan_source_tree(root: &Path, violations: &mut Vec<String>) -> Result<(), String> {
    for entry in
        fs::read_dir(root).map_err(|error| format!("failed to read {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to read directory entry: {error}"))?;
        let path = entry.path();

        if path.is_dir() {
            scan_source_tree(&path, violations)?;
            continue;
        }

        if path.extension().is_some_and(|extension| extension == "rs") {
            check_source_file(&path, violations)?;
        }
    }

    Ok(())
}

/// Records direct host-runtime API usage so the architectural failure is clear before compilation.
fn check_source_file(path: &Path, violations: &mut Vec<String>) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;

    for pattern in FORBIDDEN_SOURCE_PATTERNS {
        if source.contains(pattern) {
            violations.push(format!(
                "{}: forbidden engine API `{pattern}`",
                path.display()
            ));
        }
    }

    Ok(())
}

/// Enforces the initial one-way crate graph without requiring a host-facing dependency in engine crates.
fn check_manifest(crate_name: &str, manifest: &str, violations: &mut Vec<String>) {
    let forbidden_dependencies = match crate_name {
        "tachyon-value" | "tachyon-bytecode" => {
            ["tachyon-gc", "tachyon-compiler", "tachyon-vm", "tachyon"].as_slice()
        }
        "tachyon-gc" => [
            "tachyon-bytecode",
            "tachyon-compiler",
            "tachyon-vm",
            "tachyon",
        ]
        .as_slice(),
        "tachyon-compiler" => ["tachyon-gc", "tachyon-vm", "tachyon"].as_slice(),
        "tachyon-vm" => ["tachyon-compiler", "tachyon"].as_slice(),
        "tachyon" => [].as_slice(),
        _ => unreachable!("ENGINE_CRATES contains only known crates"),
    };

    for dependency in forbidden_dependencies {
        if manifest_has_dependency(manifest, dependency) {
            violations.push(format!(
                "{crate_name}: forbidden dependency on {dependency}"
            ));
        }
    }
}

fn manifest_has_dependency(manifest: &str, dependency: &str) -> bool {
    manifest.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with(dependency) && line[dependency.len()..].trim_start().starts_with('=')
    })
}

#[cfg(test)]
mod tests {
    use super::{FORBIDDEN_SOURCE_PATTERNS, manifest_has_dependency};

    #[test]
    fn dependency_lookup_does_not_match_package_name_prefixes() {
        assert!(!manifest_has_dependency(
            "name = \"tachyon-value\"",
            "tachyon"
        ));
        assert!(manifest_has_dependency(
            "tachyon = { path = \"../tachyon\" }",
            "tachyon"
        ));
    }

    #[test]
    fn host_io_markers_cover_the_engine_boundary() {
        assert!(FORBIDDEN_SOURCE_PATTERNS.contains(&"std::fs"));
        assert!(FORBIDDEN_SOURCE_PATTERNS.contains(&"std::net"));
        assert!(FORBIDDEN_SOURCE_PATTERNS.contains(&"std::process"));
        assert!(FORBIDDEN_SOURCE_PATTERNS.contains(&"std::io"));
    }
}
