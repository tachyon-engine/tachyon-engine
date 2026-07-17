#![allow(clippy::disallowed_methods, clippy::disallowed_types)]
//! Repository maintenance commands. Host I/O is deliberately confined to this tool crate.

use std::{env, fs, path::Path};

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
        _ => Err("usage: cargo xtask architecture check".to_owned()),
    };

    if let Err(message) = result {
        eprintln!("xtask: {message}");
        std::process::exit(1);
    }
}

/// Rejects host-runtime APIs, build scripts, and invalid local dependency directions in engine crates.
fn check_architecture() -> Result<(), String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
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
