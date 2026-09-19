//! Python dependency licenses — and the one ecosystem where this is
//! genuinely hard.
//!
//! Cargo and npm both ship the license *with the dependency list*:
//! `cargo metadata` carries each crate's `license` field, and modern
//! `package-lock.json` entries carry a `license` field. `requirements.txt`
//! carries nothing but names and version specifiers, and there is no
//! offline, universal mapping from a PyPI package name to a license. So
//! there are exactly two honest options here, and both are opt-in:
//!
//! 1. `--pip-show` — ask the *currently active* Python environment via
//!    `pip show`, which reads the installed package metadata. Accurate,
//!    but only covers packages that are actually installed, and answers
//!    for the installed version rather than the one pinned in the file.
//! 2. `pip_licenses` in the config file — a map the user maintains by
//!    hand. Tedious, but it is the only thing that works in CI without
//!    an installed environment.
//!
//! Anything neither resolves is reported as `unknown`. It is never
//! guessed at, and the asymmetry is never hidden: a pip dependency with
//! no license shown means "nothing here knows", not "no license".

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::model::{Dependency, Ecosystem, LicenseSource};

/// PEP 503 name normalisation: case-insensitive, and `-`, `_`, `.` are
/// all equivalent. Needed because a requirements file says `PyGObject`
/// or `huggingface_hub` while `pip show` answers with its own spelling.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        } else {
            out.push(ch);
            last_dash = false;
        }
    }
    out
}

/// Parses a `requirements.txt` into (name, version-specifier) pairs.
///
/// Handles comments, inline comments, extras (`uvicorn[standard]`),
/// environment markers (`; python_version < "3.10"`), and every
/// comparison operator. Deliberately skips lines that are not a plain
/// requirement — `-r other.txt`, `-e .`, `--index-url ...`, and direct
/// URL/VCS installs — rather than mangling them into fake package names.
pub fn parse_requirements(text: &str, origin: &str) -> Vec<Dependency> {
    let mut deps = Vec::new();
    for raw_line in text.lines() {
        let mut line = raw_line.trim();
        if let Some(hash) = line.find(" #") {
            line = line[..hash].trim();
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        // Drop an environment marker.
        if let Some(semi) = line.find(';') {
            line = line[..semi].trim();
        }
        // A direct URL or VCS reference has no reliable package name.
        if line.contains("://") {
            continue;
        }

        let split_at = line
            .find(['=', '>', '<', '!', '~', '['])
            .unwrap_or(line.len());
        let (name_part, rest) = line.split_at(split_at);
        let name = name_part.trim();
        if name.is_empty() {
            continue;
        }
        // Skip past the extras bracket so the version spec is reported
        // without it: `uvicorn[standard]>=0.32.0` -> `>=0.32.0`.
        let spec = match rest.find(']') {
            Some(close) => rest[close + 1..].trim(),
            None => rest.trim(),
        };
        let version = if spec.is_empty() {
            "*".to_string()
        } else {
            spec.to_string()
        };

        deps.push(Dependency::unresolved(
            Ecosystem::Pip,
            name,
            version,
            origin,
        ));
    }

    deps.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    deps.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    deps
}

/// Parses `pip show a b c` output into normalised-name -> license.
///
/// Records are separated by `---`. Two different field names carry the
/// answer depending on the package's metadata version: PEP 639's
/// `License-Expression` (which is a real SPDX expression) and the older
/// free-text `License`. `License-Expression` wins when both are present.
/// Only the text on the field's own line is taken — old packages put
/// their entire license *text* in the `License` field, and the
/// continuation lines of that are not license ids.
pub fn parse_pip_show(output: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for record in output.split("\n---") {
        let mut name = None;
        let mut license = None;
        let mut license_expression = None;
        for line in record.lines() {
            let Some((key, value)) = line.split_once(": ") else {
                continue;
            };
            // Field lines start at column zero; a wrapped license-text
            // continuation line does not.
            if key.starts_with(char::is_whitespace) {
                continue;
            }
            let value = value.trim();
            match key {
                "Name" => name = Some(value.to_string()),
                "License" if !value.is_empty() => license = Some(value.to_string()),
                "License-Expression" if !value.is_empty() => {
                    license_expression = Some(value.to_string())
                }
                _ => {}
            }
        }
        if let (Some(name), Some(license)) = (name, license_expression.or(license)) {
            map.insert(normalize_name(&name), license);
        }
    }
    map
}

/// Fills in pip licenses from the config-file map first (it is the
/// user's explicit, reviewable statement) and then, if `use_pip_show` is
/// set, from the active environment.
pub fn resolve_licenses(
    deps: &mut [Dependency],
    config_map: &BTreeMap<String, String>,
    use_pip_show: bool,
) -> Result<()> {
    let normalized_config: BTreeMap<String, &String> = config_map
        .iter()
        .map(|(k, v)| (normalize_name(k), v))
        .collect();

    for dep in deps.iter_mut().filter(|d| d.ecosystem == Ecosystem::Pip) {
        if let Some(license) = normalized_config.get(&normalize_name(&dep.name)) {
            dep.license = Some((*license).clone());
            dep.license_source = LicenseSource::UserConfig;
        }
    }

    if !use_pip_show {
        return Ok(());
    }

    let missing: Vec<String> = deps
        .iter()
        .filter(|d| d.ecosystem == Ecosystem::Pip && d.license.is_none())
        .map(|d| d.name.clone())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    let output = Command::new("pip")
        .arg("show")
        .args(&missing)
        .output()
        .context("failed to run `pip show` (is pip on PATH?)")?;
    // A non-zero exit just means some of the packages are not installed;
    // the records for the ones that are still come back on stdout.
    let shown = parse_pip_show(&String::from_utf8_lossy(&output.stdout));

    for dep in deps
        .iter_mut()
        .filter(|d| d.ecosystem == Ecosystem::Pip && d.license.is_none())
    {
        if let Some(license) = shown.get(&normalize_name(&dep.name)) {
            dep.license = Some(license.clone());
            dep.license_source = LicenseSource::PipShow;
        }
    }
    Ok(())
}

/// Reads a `requirements.txt` from disk. License resolution is a
/// separate, explicitly opted-into step — see `resolve_licenses`.
pub fn scan_requirements(path: &Path) -> Result<Vec<Dependency>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_requirements(&text, &path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim content of
    /// `apps/job-search-app/ai-tests/requirements-matcher.txt` in this
    /// monorepo — including its real trailing comment line.
    const REAL_REQUIREMENTS: &str = "fastapi>=0.115.0\n\
        uvicorn[standard]>=0.32.0\n\
        psycopg[binary]>=3.2.0\n\
        torch>=2.2.0\n\
        sentence-transformers>=5.4.0\n\
        huggingface_hub>=0.25.0\n\
        # urllib is stdlib; BFF Tantivy prefilter uses urllib.request (no extra dep)\n";

    fn names(deps: &[Dependency]) -> Vec<String> {
        deps.iter().map(|d| d.name.clone()).collect()
    }

    #[test]
    fn parses_the_real_requirements_file_from_this_repo() {
        let deps = parse_requirements(REAL_REQUIREMENTS, "requirements-matcher.txt");
        assert_eq!(
            names(&deps),
            vec![
                "fastapi",
                "huggingface_hub",
                "psycopg",
                "sentence-transformers",
                "torch",
                "uvicorn",
            ]
        );
        assert!(deps.iter().all(|d| d.ecosystem == Ecosystem::Pip));
    }

    #[test]
    fn extras_are_stripped_from_the_name_and_the_version_spec_survives() {
        let deps = parse_requirements("uvicorn[standard]>=0.32.0\n", "r.txt");
        assert_eq!(deps[0].name, "uvicorn");
        assert_eq!(deps[0].version, ">=0.32.0");
    }

    #[test]
    fn every_pip_dependency_starts_out_unresolved() {
        let deps = parse_requirements(REAL_REQUIREMENTS, "r.txt");
        assert!(deps.iter().all(|d| d.license.is_none()));
        assert!(deps
            .iter()
            .all(|d| d.license_source == LicenseSource::Unresolved));
    }

    #[test]
    fn pins_comments_markers_flags_and_urls_are_handled() {
        let text = "\
            # a comment\n\
            \n\
            requests==2.31.0  # inline comment\n\
            -r other-requirements.txt\n\
            --index-url https://example.invalid/simple\n\
            -e .\n\
            numpy~=2.0\n\
            tomli ; python_version < \"3.11\"\n\
            git+https://github.com/psf/black.git#egg=black\n";
        let deps = parse_requirements(text, "r.txt");
        assert_eq!(names(&deps), vec!["numpy", "requests", "tomli"]);
        assert_eq!(deps[1].version, "==2.31.0");
        assert_eq!(
            deps[2].version, "*",
            "an unpinned requirement has no specifier"
        );
    }

    #[test]
    fn pep503_name_normalisation() {
        assert_eq!(normalize_name("huggingface_hub"), "huggingface-hub");
        assert_eq!(normalize_name("PyGObject"), "pygobject");
        assert_eq!(normalize_name("zope.interface"), "zope-interface");
        assert_eq!(normalize_name("a--b"), "a-b");
    }

    /// Verbatim `pip show` output shape from the Python 3.14 environment
    /// this was developed against — note that some packages answer with
    /// `License:` and others with PEP 639's `License-Expression:`.
    const REAL_PIP_SHOW: &str = "\
Name: requests\n\
Version: 2.34.2\n\
Summary: Python HTTP for Humans.\n\
License: Apache-2.0\n\
Location: /usr/lib/python3.14/site-packages\n\
---\n\
Name: urllib3\n\
Version: 2.7.0\n\
License-Expression: MIT\n\
Location: /usr/lib/python3.14/site-packages\n\
---\n\
Name: certifi\n\
Version: 2026.7.22\n\
License: MPL-2.0\n\
---\n\
Name: numpy\n\
Version: 2.5.3\n\
License-Expression: BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0\n\
---\n\
Name: PyGObject\n\
Version: 3.54.5\n\
License-Expression: LGPL-2.1-or-later\n\
---\n\
Name: nolicense\n\
Version: 1.0\n\
License: \n";

    #[test]
    fn parses_both_license_field_spellings_from_real_pip_show_output() {
        let map = parse_pip_show(REAL_PIP_SHOW);
        assert_eq!(map.get("requests").map(String::as_str), Some("Apache-2.0"));
        assert_eq!(map.get("urllib3").map(String::as_str), Some("MIT"));
        assert_eq!(map.get("certifi").map(String::as_str), Some("MPL-2.0"));
        assert_eq!(
            map.get("numpy").map(String::as_str),
            Some("BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0")
        );
        assert_eq!(
            map.get("pygobject").map(String::as_str),
            Some("LGPL-2.1-or-later")
        );
        assert!(
            !map.contains_key("nolicense"),
            "an empty License: field resolves nothing, rather than an empty license"
        );
    }

    #[test]
    fn license_expression_wins_over_a_free_text_license_field() {
        let out = "Name: x\nLicense: BSD License\nLicense-Expression: BSD-3-Clause\n";
        assert_eq!(
            parse_pip_show(out).get("x").map(String::as_str),
            Some("BSD-3-Clause")
        );
    }

    #[test]
    fn a_wrapped_license_text_continuation_line_is_not_treated_as_a_field() {
        let out = "Name: oldpkg\nLicense: MIT\n  Permission is hereby granted: free of charge\nVersion: 1.0\n";
        assert_eq!(
            parse_pip_show(out).get("oldpkg").map(String::as_str),
            Some("MIT")
        );
    }

    #[test]
    fn the_config_map_resolves_pip_licenses_without_any_environment() {
        let mut deps = parse_requirements(REAL_REQUIREMENTS, "r.txt");
        let mut config = BTreeMap::new();
        config.insert("torch".to_string(), "BSD-3-Clause".to_string());
        // Spelled with a dash where the requirements file uses an
        // underscore — PEP 503 says these are the same package.
        config.insert("huggingface-hub".to_string(), "Apache-2.0".to_string());

        resolve_licenses(&mut deps, &config, false).unwrap();

        let torch = deps.iter().find(|d| d.name == "torch").unwrap();
        assert_eq!(torch.license.as_deref(), Some("BSD-3-Clause"));
        assert_eq!(torch.license_source, LicenseSource::UserConfig);
        let hub = deps.iter().find(|d| d.name == "huggingface_hub").unwrap();
        assert_eq!(hub.license.as_deref(), Some("Apache-2.0"));
        // Not in the map and pip show not requested: still unresolved.
        let fastapi = deps.iter().find(|d| d.name == "fastapi").unwrap();
        assert_eq!(fastapi.license, None);
    }
}
