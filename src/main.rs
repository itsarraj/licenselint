use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Parser;
use licenselint::discover::{self, Target};
use licenselint::model::Ecosystem;
use licenselint::policy::Policy;
use licenselint::{cargo, dedup_across_manifests, npm, python, report};

#[derive(Parser)]
#[command(
    name = "licenselint",
    about = "One dependency-license scan across Cargo, npm and pip in a polyglot repo"
)]
struct Cli {
    /// Directories to scan, or individual Cargo.toml / package-lock.json /
    /// requirements*.txt files. Defaults to the current directory.
    paths: Vec<PathBuf>,

    /// Licenses to reject, e.g. --deny GPL-3.0 --deny AGPL-3.0. Repeatable.
    #[arg(short, long, value_name = "SPDX")]
    deny: Vec<String>,

    /// Licenses to permit; anything not satisfiable by this list is
    /// flagged. Repeatable.
    #[arg(short, long, value_name = "SPDX")]
    allow: Vec<String>,

    /// JSON policy file: {"allow": [], "deny": [], "pip_licenses": {},
    /// "fail_unknown": false}. Merged with --allow/--deny.
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Resolve pip licenses by running `pip show` against the currently
    /// active Python environment (only covers installed packages).
    #[arg(long)]
    pip_show: bool,

    /// Fail on dependencies whose license could not be resolved at all.
    #[arg(long)]
    fail_unknown: bool,

    /// Print only the lines that violate the policy.
    #[arg(long)]
    violations_only: bool,

    /// Machine-readable output.
    #[arg(long)]
    json: bool,

    /// Pass --offline to `cargo metadata` (requires a warm registry cache).
    #[arg(long)]
    offline: bool,

    /// How deep to walk into directories looking for manifests.
    #[arg(long, default_value_t = 16, value_name = "N")]
    max_depth: usize,
}

fn load_policy(cli: &Cli) -> Result<Policy> {
    let mut policy = match &cli.config {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read config {}", path.display()))?;
            serde_json::from_str(&text)
                .with_context(|| format!("failed to parse config {}", path.display()))?
        }
        None => Policy::default(),
    };
    policy.allow.extend(cli.allow.iter().cloned());
    policy.deny.extend(cli.deny.iter().cloned());
    policy.fail_unknown |= cli.fail_unknown;
    Ok(policy)
}

fn run(cli: &Cli) -> Result<bool> {
    let policy = load_policy(cli)?;

    let paths = if cli.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        cli.paths.clone()
    };
    let targets = discover::discover(&paths, cli.max_depth);
    if targets.is_empty() {
        bail!("no Cargo.toml, package-lock.json or requirements*.txt found in the given paths");
    }

    let mut deps = Vec::new();
    for target in &targets {
        let scanned = match target {
            Target::Cargo(p) => cargo::scan_manifest(p, cli.offline),
            Target::Npm(p) => npm::scan_lockfile(p),
            Target::Pip(p) => python::scan_requirements(p),
        };
        match scanned {
            Ok(found) => deps.extend(found),
            // One unreadable manifest should not throw away the rest of
            // the report; say so on stderr and keep going.
            Err(e) => eprintln!("warning: skipping {}: {e:#}", target.path().display()),
        }
    }

    if deps.iter().any(|d| d.ecosystem == Ecosystem::Pip) {
        python::resolve_licenses(&mut deps, &policy.pip_licenses, cli.pip_show)?;
    }
    dedup_across_manifests(&mut deps);

    let findings = report::evaluate(deps, &policy);
    if cli.json {
        println!("{}", report::render_json(&findings, cli.violations_only)?);
    } else {
        print!("{}", report::render_text(&findings, cli.violations_only));
    }
    Ok(findings.iter().any(|f| f.violation))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(true) => ExitCode::from(1),
        Ok(false) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("licenselint: {e:#}");
            ExitCode::from(2)
        }
    }
}
