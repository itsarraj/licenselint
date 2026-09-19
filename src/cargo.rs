//! Cargo dependency licenses, straight from `cargo metadata`.
//!
//! This is the easy ecosystem: every crate on crates.io carries a
//! `license` (or `license-file`) field in its own `Cargo.toml`, and
//! `cargo metadata --format-version 1` hands you the resolved graph with
//! that field already attached — no network calls of our own, no
//! guessing. What you get is authoritative.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::model::{Dependency, Ecosystem, LicenseSource};

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<MetadataPackage>,
    #[serde(default)]
    workspace_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MetadataPackage {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    license: Option<String>,
}

/// Turns `cargo metadata --format-version 1` output into dependencies.
///
/// Packages listed in `workspace_members` are dropped: those are the
/// project's *own* crates, and linting your own license against your own
/// policy is noise. Everything else in the resolved graph is kept — which
/// deliberately includes dev-dependencies, build-dependencies and
/// dependencies for other target platforms, because `cargo metadata`
/// returns the unfiltered graph and over-reporting is the safe direction
/// for a license review.
pub fn parse_metadata(json: &str, origin: &str) -> Result<Vec<Dependency>> {
    let metadata: Metadata =
        serde_json::from_str(json).context("failed to parse `cargo metadata` output")?;

    let mut deps: Vec<Dependency> = metadata
        .packages
        .iter()
        .filter(|p| !metadata.workspace_members.contains(&p.id))
        .map(|p| {
            // A crate with only `license-file` set is saying "my license
            // is in this file and is not expressible as an SPDX id" —
            // usually a custom or non-standard license. Reporting the
            // filename as if it were a license id would be a lie, so
            // those stay unresolved exactly like a crate with no license
            // information at all.
            let (license, source) = match p.license.as_deref().map(str::trim) {
                Some(l) if !l.is_empty() => (Some(l.to_string()), LicenseSource::CargoManifest),
                _ => (None, LicenseSource::Unresolved),
            };
            Dependency {
                ecosystem: Ecosystem::Cargo,
                name: p.name.clone(),
                version: p.version.clone(),
                license,
                license_source: source,
                origin: origin.to_string(),
            }
        })
        .collect();

    deps.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    deps.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    Ok(deps)
}

/// Runs `cargo metadata` for a real manifest on disk.
///
/// `offline` passes `--offline`, which works only if every dependency is
/// already in the local cargo registry cache; without it cargo will hit
/// the network to resolve/download manifests exactly as a normal build
/// would.
pub fn scan_manifest(manifest_path: &Path, offline: bool) -> Result<Vec<Dependency>> {
    let mut cmd = Command::new("cargo");
    cmd.arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(manifest_path);
    if offline {
        cmd.arg("--offline");
    }

    let output = cmd.output().with_context(|| {
        format!(
            "failed to run `cargo metadata` for {}",
            manifest_path.display()
        )
    })?;
    if !output.status.success() {
        bail!(
            "`cargo metadata` failed for {}: {}",
            manifest_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let json =
        String::from_utf8(output.stdout).context("`cargo metadata` emitted invalid UTF-8")?;
    parse_metadata(&json, &manifest_path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed but otherwise verbatim slice of real
    /// `cargo metadata --format-version 1` output, captured from
    /// `tools/dsnmask` in this monorepo — including the real id format
    /// current cargo emits and the real license strings of these crates.
    const REAL_DSNMASK_METADATA: &str = r#"{
      "packages": [
        {
          "id": "path+file:///home/cesium/personal/tools/dsnmask#0.1.0",
          "name": "dsnmask",
          "version": "0.1.0",
          "license": "MIT",
          "license_file": null
        },
        {
          "id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.229",
          "name": "serde",
          "version": "1.0.229",
          "license": "MIT OR Apache-2.0",
          "license_file": null
        },
        {
          "id": "registry+https://github.com/rust-lang/crates.io-index#clap@4.6.6",
          "name": "clap",
          "version": "4.6.6",
          "license": "MIT OR Apache-2.0",
          "license_file": null
        },
        {
          "id": "registry+https://github.com/rust-lang/crates.io-index#memchr@2.8.3",
          "name": "memchr",
          "version": "2.8.3",
          "license": "Unlicense OR MIT",
          "license_file": null
        },
        {
          "id": "registry+https://github.com/rust-lang/crates.io-index#unicode-ident@1.0.24",
          "name": "unicode-ident",
          "version": "1.0.24",
          "license": "(MIT OR Apache-2.0) AND Unicode-3.0",
          "license_file": null
        },
        {
          "id": "registry+https://github.com/rust-lang/crates.io-index#icu_normalizer@2.3.0",
          "name": "icu_normalizer",
          "version": "2.3.0",
          "license": "Unicode-3.0",
          "license_file": null
        },
        {
          "id": "registry+https://github.com/rust-lang/crates.io-index#customlicensed@0.1.0",
          "name": "customlicensed",
          "version": "0.1.0",
          "license": null,
          "license_file": "LICENSE.custom"
        }
      ],
      "workspace_members": ["path+file:///home/cesium/personal/tools/dsnmask#0.1.0"],
      "version": 1
    }"#;

    fn find<'a>(deps: &'a [Dependency], name: &str) -> &'a Dependency {
        deps.iter()
            .find(|d| d.name == name)
            .expect("dependency present")
    }

    #[test]
    fn extracts_the_real_license_strings() {
        let deps = parse_metadata(REAL_DSNMASK_METADATA, "Cargo.toml").unwrap();
        assert_eq!(
            find(&deps, "serde").license.as_deref(),
            Some("MIT OR Apache-2.0")
        );
        assert_eq!(
            find(&deps, "clap").license.as_deref(),
            Some("MIT OR Apache-2.0")
        );
        assert_eq!(
            find(&deps, "memchr").license.as_deref(),
            Some("Unlicense OR MIT")
        );
        assert_eq!(
            find(&deps, "unicode-ident").license.as_deref(),
            Some("(MIT OR Apache-2.0) AND Unicode-3.0")
        );
        assert_eq!(
            find(&deps, "icu_normalizer").license.as_deref(),
            Some("Unicode-3.0")
        );
        assert_eq!(
            find(&deps, "serde").license_source,
            LicenseSource::CargoManifest
        );
    }

    #[test]
    fn the_workspace_member_itself_is_excluded() {
        let deps = parse_metadata(REAL_DSNMASK_METADATA, "Cargo.toml").unwrap();
        assert!(
            !deps.iter().any(|d| d.name == "dsnmask"),
            "the crate being scanned is not one of its own dependencies"
        );
    }

    #[test]
    fn a_license_file_only_crate_is_unresolved_not_mislabelled() {
        let deps = parse_metadata(REAL_DSNMASK_METADATA, "Cargo.toml").unwrap();
        let custom = find(&deps, "customlicensed");
        assert_eq!(custom.license, None);
        assert_eq!(custom.license_source, LicenseSource::Unresolved);
    }

    #[test]
    fn output_is_sorted_and_deduped() {
        let deps = parse_metadata(REAL_DSNMASK_METADATA, "Cargo.toml").unwrap();
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn two_versions_of_the_same_crate_are_both_kept() {
        // `syn 2.x` and `syn 3.x` genuinely coexist in this repo's trees.
        let json = r#"{
          "packages": [
            {"id":"registry+x#syn@2.0.119","name":"syn","version":"2.0.119","license":"MIT OR Apache-2.0","license_file":null},
            {"id":"registry+x#syn@3.0.4","name":"syn","version":"3.0.4","license":"MIT OR Apache-2.0","license_file":null}
          ],
          "workspace_members": [],
          "version": 1
        }"#;
        let deps = parse_metadata(json, "Cargo.toml").unwrap();
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn malformed_metadata_is_an_error_not_a_panic() {
        assert!(parse_metadata("not json", "Cargo.toml").is_err());
    }
}
