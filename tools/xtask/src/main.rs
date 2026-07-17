#![allow(clippy::disallowed_methods, clippy::disallowed_types)]
//! Repository maintenance commands. Host I/O is deliberately confined to this tool crate.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use benchmark_runner::{
    BENCHMARK_REPORT_SCHEMA_VERSION, BenchmarkAdapter, BenchmarkConfig, BenchmarkReport,
    CorpusScript, EngineIdentity, EngineKind, ExternalProcessAdapter, ExternalProcessConfig,
    HostMetadata, MeasurementMode, TachyonInProcessAdapter, TachyonInProcessConfig,
    compare_reports as compare_benchmarks, load_corpus, run_case,
};
use test262_runner::{
    RunOptions, RunReport, TachyonAdapter, Test262Config, compare_reports, run_checkout,
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
        [command, subcommand, rest @ ..] if command == "bench" && subcommand == "run-external" => {
            run_external_benchmarks(rest)
        }
        [command, subcommand, rest @ ..] if command == "bench" && subcommand == "run-profile" => {
            run_benchmark_profile(rest)
        }
        [command, subcommand, profile] if command == "bench" && subcommand == "build-profile" => {
            build_benchmark_profile(profile)
        }
        [command, subcommand, mode, script]
            if command == "bench" && subcommand == "run-tachyon" =>
        {
            launch_release_tachyon_benchmark(mode, script)
        }
        [command, subcommand, mode, script]
            if command == "bench" && subcommand == "run-tachyon-internal" =>
        {
            run_tachyon_benchmark(mode, script)
        }
        _ => Err(USAGE.to_owned()),
    };

    if let Err(message) = result {
        eprintln!("xtask: {message}");
        std::process::exit(1);
    }
}

const USAGE: &str = "usage:\n  cargo xtask architecture check\n  cargo xtask test262 fetch\n  cargo xtask test262 run [test/path-or-file] [--filter text] [--seed n] [--serial|--parallel]\n  cargo xtask test262 compare <base.json> <new.json> [--markdown]\n  cargo xtask bench verify\n  cargo xtask bench compare <base.json> <candidate.json> [--markdown]\n  cargo xtask bench build-profile <profile-id>\n  cargo xtask bench run-profile <profile-id> [--script id]\n  cargo xtask bench run-tachyon <parse-compile-execute|precompiled-execute|steady-state> <script-id>\n  cargo xtask bench run-external <boa|quickjs|escargot> <executable> <version> <commit> <features> <build-flags> [--script id] [--engine-arg arg]...";

struct ExternalRunOptions {
    kind: EngineKind,
    name: Box<str>,
    executable: PathBuf,
    version: Box<str>,
    commit: Box<str>,
    features: Box<str>,
    build_flags: Box<str>,
    script: Option<Box<str>>,
    engine_arguments: Vec<OsString>,
}

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

/// Runs Tachyon with deterministic selection flags and writes a phase-aware JSON report to stdout.
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
        &TachyonAdapter,
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
    let engines = config
        .external_engines
        .iter()
        .map(|profile| {
            serde_json::json!({
                "id": profile.id,
                "kind": profile.kind,
                "platform": profile.platform,
                "repository": profile.repository,
                "commit": profile.commit,
                "executable_path": profile.executable_path,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_writer_pretty(
        std::io::stdout().lock(),
        &serde_json::json!({ "schema_version": 1, "engines": engines, "scripts": scripts }),
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

/// Runs an approved corpus subset through one release CLI and emits a standalone JSON report.
fn run_external_benchmarks(arguments: &[String]) -> Result<(), String> {
    let options = parse_external_run_options(arguments)?;
    let (config, corpus) = benchmark_config_and_corpus()?;
    execute_external_benchmarks(options, config, corpus)
}

/// Resolves a pinned profile, verifies its source checkout, and runs its release executable.
fn run_benchmark_profile(arguments: &[String]) -> Result<(), String> {
    let (profile_id, script) = parse_profile_run_options(arguments)?;
    let (config, corpus) = benchmark_config_and_corpus()?;
    let workspace = workspace_root();
    let profile = config
        .external_engine(&profile_id)
        .ok_or_else(|| format!("unknown external engine profile: {profile_id}"))?
        .clone();
    verify_profile_checkout(&workspace, &profile)?;
    let options = ExternalRunOptions {
        kind: profile.kind,
        name: profile.name,
        executable: workspace.join(&*profile.executable_path),
        version: profile.version,
        commit: profile.commit,
        features: profile.features,
        build_flags: profile.build_flags,
        script,
        engine_arguments: profile
            .fixed_arguments
            .into_iter()
            .map(|argument| OsString::from(argument.as_ref()))
            .collect(),
    };
    execute_external_benchmarks(options, config, corpus)
}

/// Executes profile build steps as argv arrays after validating the pinned tracked checkout.
fn build_benchmark_profile(profile_id: &str) -> Result<(), String> {
    let (config, _) = benchmark_config_and_corpus()?;
    let workspace = workspace_root();
    let profile = config
        .external_engine(profile_id)
        .ok_or_else(|| format!("unknown external engine profile: {profile_id}"))?;
    verify_profile_checkout(&workspace, profile)?;
    for step in &profile.build_steps {
        let mut command = Command::new(&*step.program);
        command
            .current_dir(workspace.join(&*step.working_directory))
            .args(step.arguments.iter().map(|argument| &**argument))
            .envs(
                step.environment
                    .iter()
                    .map(|(key, value)| (&**key, &**value)),
            );
        run_streaming_command(&mut command)?;
    }
    let executable = workspace.join(&*profile.executable_path);
    executable
        .is_file()
        .then_some(())
        .ok_or_else(|| format!("profile build did not produce {}", executable.display()))
}

/// Loads the one strict config and content-addressed corpus shared by all benchmark commands.
fn benchmark_config_and_corpus() -> Result<(BenchmarkConfig, Vec<CorpusScript>), String> {
    let workspace = workspace_root();
    let config_path = workspace.join("benchmark_config.toml");
    let source = fs::read_to_string(&config_path)
        .map_err(|error| format!("failed to read {}: {error}", config_path.display()))?;
    let config = BenchmarkConfig::parse(&source).map_err(|error| error.to_string())?;
    let corpus = load_corpus(&workspace, &config).map_err(|error| error.to_string())?;
    Ok((config, corpus))
}

/// Runs one selected profile over approved scripts and serializes the complete report.
fn execute_external_benchmarks(
    options: ExternalRunOptions,
    config: BenchmarkConfig,
    corpus: Vec<CorpusScript>,
) -> Result<(), String> {
    let identity = EngineIdentity {
        name: options.name,
        kind: options.kind,
        version: options.version,
        commit: options.commit,
        features: options.features,
        build_flags: options.build_flags,
        binary_size_bytes: None,
    };
    let mut adapter = ExternalProcessAdapter::new(
        identity,
        ExternalProcessConfig {
            executable: options.executable,
            fixed_arguments: options.engine_arguments,
            timeout: Duration::from_millis(config.external_process_timeout_millis),
            maximum_output_bytes: config.maximum_process_output_bytes,
        },
    )
    .map_err(|error| error.to_string())?;
    execute_adapter_benchmarks(
        &mut adapter,
        MeasurementMode::ColdStart,
        options.script.as_deref(),
        config,
        corpus,
    )
}

/// Re-executes the in-process benchmark in the configured Cargo release profile outside the timer.
fn launch_release_tachyon_benchmark(mode: &str, script: &str) -> Result<(), String> {
    let workspace = workspace_root();
    verify_clean_checkout(&workspace)?;
    let mut command = Command::new("cargo");
    command
        .current_dir(&workspace)
        .args([
            "run",
            "--release",
            "-p",
            "xtask",
            "--",
            "bench",
            "run-tachyon-internal",
            mode,
            script,
        ])
        .env("TACHYON_BENCH_RELEASE_CHILD", "1");
    run_streaming_command(&mut command)
}

/// Runs one mode inside the release child after binding report identity to a clean revision.
fn run_tachyon_benchmark(mode: &str, script: &str) -> Result<(), String> {
    if env::var_os("TACHYON_BENCH_RELEASE_CHILD").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Err("internal Tachyon benchmark must be launched through run-tachyon".to_owned());
    }
    let mode = match mode {
        "parse-compile-execute" => MeasurementMode::ParseCompileExecute,
        "precompiled-execute" => MeasurementMode::PrecompiledExecute,
        "steady-state" => MeasurementMode::SteadyState,
        _ => return Err(USAGE.to_owned()),
    };
    let workspace = workspace_root();
    let commit = verify_clean_checkout(&workspace)?;
    let (config, corpus) = benchmark_config_and_corpus()?;
    let build_flags = format!(
        "profile={}; panic={}; lto={}; codegen-units={}; target-cpu={}",
        config.build.profile,
        config.build.panic,
        config.build.lto,
        config.build.codegen_units,
        config.build.target_cpu
    );
    let identity = EngineIdentity {
        name: "Tachyon".into(),
        kind: EngineKind::TachyonInProcess,
        version: env!("CARGO_PKG_VERSION").into(),
        commit: commit.into(),
        features: config.build.features.clone(),
        build_flags: build_flags.into(),
        binary_size_bytes: None,
    };
    let mut adapter = TachyonInProcessAdapter::new(
        identity,
        TachyonInProcessConfig::from_benchmark(config.tachyon),
    )
    .map_err(|error| error.to_string())?;
    execute_adapter_benchmarks(&mut adapter, mode, Some(script), config, corpus)
}

/// Applies one adapter/mode to a selected approved corpus and serializes a complete report.
fn execute_adapter_benchmarks(
    adapter: &mut dyn BenchmarkAdapter,
    mode: MeasurementMode,
    selected_script: Option<&str>,
    config: BenchmarkConfig,
    corpus: Vec<CorpusScript>,
) -> Result<(), String> {
    let host = HostMetadata::collect(&config);
    let selected = corpus
        .iter()
        .filter(|script| selected_script.is_none_or(|id| id == &*script.config.id))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("benchmark script selection matched nothing".to_owned());
    }
    let mut cases = Vec::new();
    cases
        .try_reserve_exact(selected.len())
        .map_err(|_| "cannot reserve benchmark results".to_owned())?;
    for script in selected {
        cases.push(
            run_case(adapter, script, mode, &config, &host).map_err(|error| error.to_string())?,
        );
    }
    let report = BenchmarkReport {
        schema_version: BENCHMARK_REPORT_SCHEMA_VERSION,
        host,
        build: config.build,
        cases,
    };
    serde_json::to_writer_pretty(std::io::stdout().lock(), &report)
        .map_err(|error| error.to_string())?;
    println!();
    Ok(())
}

/// Returns HEAD only when tracked files match it, so report provenance never names stale source.
fn verify_clean_checkout(checkout: &Path) -> Result<String, String> {
    let commit = command_output(
        Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["rev-parse", "HEAD"]),
    )?;
    let cleanliness = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .status()
        .map_err(|error| format!("failed to inspect {}: {error}", checkout.display()))?;
    if cleanliness.success() {
        Ok(commit.trim().to_owned())
    } else {
        Err(format!(
            "benchmark checkout has tracked modifications: {}",
            checkout.display()
        ))
    }
}

fn parse_profile_run_options(arguments: &[String]) -> Result<(String, Option<Box<str>>), String> {
    match arguments {
        [profile] => Ok((profile.clone(), None)),
        [profile, flag, script] if flag == "--script" => {
            Ok((profile.clone(), Some(script.clone().into_boxed_str())))
        }
        _ => Err(USAGE.to_owned()),
    }
}

/// Verifies platform, full revision, and tracked cleanliness before trusting a profile binary.
fn verify_profile_checkout(
    workspace: &Path,
    profile: &benchmark_runner::ExternalEngineProfile,
) -> Result<(), String> {
    let platform = format!("{}-{}", env::consts::OS, env::consts::ARCH);
    if profile.platform.as_ref() != platform {
        return Err(format!(
            "profile {} targets {}, current platform is {platform}",
            profile.id, profile.platform
        ));
    }
    let checkout = workspace.join(&*profile.checkout_path);
    let actual = command_output(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["rev-parse", "HEAD"]),
    )?;
    if actual.trim() != profile.commit.as_ref() {
        return Err(format!(
            "profile {} revision mismatch: expected {}, got {}",
            profile.id,
            profile.commit,
            actual.trim()
        ));
    }
    let cleanliness = Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .status()
        .map_err(|error| format!("failed to inspect {}: {error}", checkout.display()))?;
    cleanliness
        .success()
        .then_some(())
        .ok_or_else(|| format!("profile {} checkout has tracked modifications", profile.id))
}

/// Parses explicit provenance and engine arguments without accepting shell command strings.
fn parse_external_run_options(arguments: &[String]) -> Result<ExternalRunOptions, String> {
    let [
        engine,
        executable,
        version,
        commit,
        features,
        build_flags,
        rest @ ..,
    ] = arguments
    else {
        return Err(USAGE.to_owned());
    };
    let (kind, name) = match engine.as_str() {
        "boa" => (EngineKind::BoaCli, "Boa"),
        "quickjs" => (EngineKind::QuickJsCli, "QuickJS"),
        "escargot" => (EngineKind::EscargotCli, "Escargot"),
        _ => return Err("external engine must be boa, quickjs, or escargot".to_owned()),
    };
    let mut script = None;
    let mut engine_arguments = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        let flag = &rest[index];
        index += 1;
        let value = rest
            .get(index)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--script" if script.is_none() => script = Some(value.clone().into_boxed_str()),
            "--engine-arg" => engine_arguments.push(OsString::from(value)),
            _ => return Err(USAGE.to_owned()),
        }
        index += 1;
    }
    Ok(ExternalRunOptions {
        kind,
        name: name.into(),
        executable: executable.into(),
        version: version.clone().into_boxed_str(),
        commit: commit.clone().into_boxed_str(),
        features: features.clone().into_boxed_str(),
        build_flags: build_flags.clone().into_boxed_str(),
        script,
        engine_arguments,
    })
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

/// Streams potentially large build logs instead of retaining unbounded child output in memory.
fn run_streaming_command(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("failed to run command: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("command exited with status {status}"))
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
    use benchmark_runner::EngineKind;

    use super::{FORBIDDEN_SOURCE_PATTERNS, manifest_has_dependency, parse_external_run_options};

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

    #[test]
    fn external_benchmark_options_preserve_provenance_and_repeated_arguments() {
        let arguments = [
            "escargot",
            "/tmp/escargot",
            "1.0",
            "deadbeef",
            "intl",
            "-O3",
            "--script",
            "basic/call-loop",
            "--engine-arg",
            "--shell",
            "--engine-arg",
            "--canblock-is-false",
        ]
        .map(str::to_owned);
        let options = parse_external_run_options(&arguments).unwrap();
        assert_eq!(options.kind, EngineKind::EscargotCli);
        assert_eq!(&*options.commit, "deadbeef");
        assert_eq!(options.script.as_deref(), Some("basic/call-loop"));
        assert_eq!(options.engine_arguments.len(), 2);
    }

    #[test]
    fn external_benchmark_options_reject_unknown_engines_and_missing_values() {
        let unknown = ["v8", "bin", "v", "c", "f", "flags"].map(str::to_owned);
        assert!(parse_external_run_options(&unknown).is_err());
        let missing_value =
            ["boa", "bin", "v", "c", "f", "flags", "--engine-arg"].map(str::to_owned);
        assert!(parse_external_run_options(&missing_value).is_err());
    }
}
