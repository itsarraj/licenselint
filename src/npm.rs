//! npm dependency licenses from `package-lock.json`.
//!
//! Modern npm (lockfileVersion 2 and 3) writes a `license` field into
//! each entry of the lockfile's `packages` map, so in the common case
//! this is offline and authoritative, same as Cargo. It is not
//! guaranteed to be there, though — older lockfiles and entries written
//! by other tooling omit it — so when it is missing this falls back to
//! reading `node_modules/<pkg>/package.json`, which only works if
//! dependencies are actually installed. Anything still unresolved after
//! that is reported as unresolved rather than guessed at.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{Dependency, Ecosystem, LicenseSource};

#[derive(Debug, Deserialize)]
struct Lockfile {
    #[serde(rename = "lockfileVersion")]
    lockfile_version: Option<u32>,
    packages: Option<BTreeMap<String, LockEntry>>,
}

#[derive(Debug, Deserialize)]
struct LockEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    license: Option<Value>,
    /// A workspace symlink; it has no license of its own and its real
    /// entry appears elsewhere in the map.
    #[serde(default)]
    link: bool,
}

/// `node_modules/@babel/core` -> `@babel/core`, and
/// `node_modules/a/node_modules/b` -> `b`. Scoped names survive because
/// only the text after the *last* `node_modules/` is taken.
fn package_name_from_path(path: &str) -> Option<String> {
    let idx = path.rfind("node_modules/")?;
    let name = &path[idx + "node_modules/".len()..];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// npm's `license` field has three historical shapes: a plain string, an
/// object `{"type": "MIT"}`, and an array of those objects. All three are
/// still found in the wild; the array form is joined with `OR`, which is
/// what it always meant.
fn license_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Object(o) => o
            .get("type")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(license_from_value).collect();
            if parts.is_empty() {
                None
            } else if parts.len() == 1 {
                Some(parts.into_iter().next().expect("length checked"))
            } else {
                Some(format!("({})", parts.join(" OR ")))
            }
        }
        _ => None,
    }
}

/// Also reads the deprecated top-level `licenses: [...]` array that old
/// `package.json` files use instead of `license`.
fn license_from_package_json(json: &Value) -> Option<String> {
    json.get("license")
        .and_then(license_from_value)
        .or_else(|| json.get("licenses").and_then(license_from_value))
}

/// Parses a lockfile's JSON text. `lock_dir` is only used to record where
/// each dependency came from; no filesystem access happens here, which is
/// what makes this unit-testable without an installed `node_modules`.
pub fn parse_lockfile(json: &str, origin: &str) -> Result<Vec<Dependency>> {
    let lock: Lockfile = serde_json::from_str(json).context("failed to parse package-lock.json")?;

    let Some(packages) = lock.packages else {
        bail!(
            "{origin}: lockfileVersion {} has no `packages` map; only npm 7+ \
             (lockfileVersion 2/3) lockfiles are supported",
            lock.lockfile_version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<unknown>".to_string())
        );
    };

    let mut deps: Vec<Dependency> = packages
        .iter()
        // The `""` key is the project itself, not a dependency.
        .filter(|(path, entry)| !path.is_empty() && !entry.link)
        .filter_map(|(path, entry)| {
            let name = entry
                .name
                .clone()
                .or_else(|| package_name_from_path(path))?;
            let version = entry.version.clone().unwrap_or_else(|| "*".to_string());
            let license = entry.license.as_ref().and_then(license_from_value);
            let source = if license.is_some() {
                LicenseSource::NpmLockfile
            } else {
                LicenseSource::Unresolved
            };
            Some(Dependency {
                ecosystem: Ecosystem::Npm,
                name,
                version,
                license,
                license_source: source,
                origin: origin.to_string(),
            })
        })
        .collect();

    deps.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    deps.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    Ok(deps)
}

/// Second pass for entries the lockfile had no `license` for: read the
/// installed `node_modules/<name>/package.json`. Silently leaves a
/// dependency unresolved if the package is not installed — this is a
/// best-effort improvement, never a hard requirement.
pub fn resolve_from_node_modules(deps: &mut [Dependency], lock_dir: &Path) {
    for dep in deps
        .iter_mut()
        .filter(|d| d.ecosystem == Ecosystem::Npm && d.license.is_none())
    {
        let manifest = lock_dir
            .join("node_modules")
            .join(&dep.name)
            .join("package.json");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(license) = license_from_package_json(&json) {
            dep.license = Some(license);
            dep.license_source = LicenseSource::NodeModules;
        }
    }
}

/// Scans a `package-lock.json` on disk, including the `node_modules`
/// fallback for entries the lockfile itself did not label.
pub fn scan_lockfile(path: &Path) -> Result<Vec<Dependency>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let origin = path.display().to_string();
    let mut deps = parse_lockfile(&text, &origin)?;
    if let Some(dir) = path.parent() {
        resolve_from_node_modules(&mut deps, dir);
    }
    Ok(deps)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped exactly like the real lockfiles in this monorepo
    /// (`apps/siliconbazaar/bff/package-lock.json` is lockfileVersion 3
    /// with inline `license` fields, and really does contain entries with
    /// no `license` at all).
    const REAL_SHAPED_LOCK: &str = r#"{
      "name": "siliconbazaar",
      "lockfileVersion": 3,
      "packages": {
        "": { "name": "siliconbazaar", "version": "4.0.0", "license": "MIT" },
        "node_modules/@esbuild/aix-ppc64": { "version": "0.28.1", "dev": true, "license": "MIT" },
        "node_modules/express": { "version": "4.21.2", "license": "MIT" },
        "node_modules/caniuse-lite": { "version": "1.0.30001700", "license": "CC-BY-4.0" },
        "node_modules/psl": { "version": "1.9.0", "license": "MIT" },
        "node_modules/unlabelled-thing": { "version": "2.0.0" },
        "node_modules/old-style": { "version": "1.0.0", "license": { "type": "BSD-3-Clause" } },
        "node_modules/multi-licensed": {
          "version": "3.0.0",
          "license": [{ "type": "MIT" }, { "type": "Apache-2.0" }]
        },
        "node_modules/a/node_modules/nested-dup": { "version": "1.2.3", "license": "ISC" },
        "node_modules/workspace-link": { "resolved": "packages/thing", "link": true }
      }
    }"#;

    fn find<'a>(deps: &'a [Dependency], name: &str) -> &'a Dependency {
        deps.iter()
            .find(|d| d.name == name)
            .expect("dependency present")
    }

    #[test]
    fn reads_inline_licenses_from_the_lockfile() {
        let deps = parse_lockfile(REAL_SHAPED_LOCK, "package-lock.json").unwrap();
        assert_eq!(find(&deps, "express").license.as_deref(), Some("MIT"));
        assert_eq!(
            find(&deps, "express").license_source,
            LicenseSource::NpmLockfile
        );
        assert_eq!(
            find(&deps, "caniuse-lite").license.as_deref(),
            Some("CC-BY-4.0")
        );
    }

    #[test]
    fn scoped_and_nested_package_names_are_recovered_from_the_path() {
        let deps = parse_lockfile(REAL_SHAPED_LOCK, "package-lock.json").unwrap();
        assert_eq!(find(&deps, "@esbuild/aix-ppc64").version, "0.28.1");
        assert_eq!(find(&deps, "nested-dup").version, "1.2.3");
    }

    #[test]
    fn the_root_project_entry_and_workspace_links_are_excluded() {
        let deps = parse_lockfile(REAL_SHAPED_LOCK, "package-lock.json").unwrap();
        assert!(!deps.iter().any(|d| d.name == "siliconbazaar"));
        assert!(!deps.iter().any(|d| d.name == "workspace-link"));
    }

    #[test]
    fn an_entry_with_no_license_field_is_unresolved_not_guessed() {
        let deps = parse_lockfile(REAL_SHAPED_LOCK, "package-lock.json").unwrap();
        let dep = find(&deps, "unlabelled-thing");
        assert_eq!(dep.license, None);
        assert_eq!(dep.license_source, LicenseSource::Unresolved);
    }

    #[test]
    fn the_legacy_object_and_array_license_shapes_are_understood() {
        let deps = parse_lockfile(REAL_SHAPED_LOCK, "package-lock.json").unwrap();
        assert_eq!(
            find(&deps, "old-style").license.as_deref(),
            Some("BSD-3-Clause")
        );
        assert_eq!(
            find(&deps, "multi-licensed").license.as_deref(),
            Some("(MIT OR Apache-2.0)"),
            "an array of licenses has always meant OR, and must parse as an SPDX expression"
        );
        // And the joined form must actually be parseable downstream.
        assert!(crate::spdx::parse("(MIT OR Apache-2.0)").is_ok());
    }

    #[test]
    fn a_v1_lockfile_is_a_clear_error_not_a_silent_empty_report() {
        let v1 =
            r#"{"name":"x","lockfileVersion":1,"dependencies":{"lodash":{"version":"4.17.21"}}}"#;
        let err = parse_lockfile(v1, "package-lock.json")
            .unwrap_err()
            .to_string();
        assert!(err.contains("lockfileVersion 1"), "got: {err}");
    }

    #[test]
    fn package_json_licenses_array_fallback_is_understood() {
        let json: Value =
            serde_json::from_str(r#"{"licenses":[{"type":"MIT"},{"type":"GPL-2.0"}]}"#).unwrap();
        assert_eq!(
            license_from_package_json(&json).as_deref(),
            Some("(MIT OR GPL-2.0)")
        );
    }

    #[test]
    fn node_modules_fallback_fills_in_a_missing_license() {
        let dir = std::env::temp_dir().join(format!("licenselint-npm-test-{}", std::process::id()));
        let pkg_dir = dir.join("node_modules").join("unlabelled-thing");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"unlabelled-thing","license":"ISC"}"#,
        )
        .unwrap();

        let mut deps = parse_lockfile(REAL_SHAPED_LOCK, "package-lock.json").unwrap();
        resolve_from_node_modules(&mut deps, &dir);
        let dep = find(&deps, "unlabelled-thing");
        assert_eq!(dep.license.as_deref(), Some("ISC"));
        assert_eq!(dep.license_source, LicenseSource::NodeModules);
        // A package that is simply not installed stays unresolved.
        assert_eq!(
            find(&deps, "express").license_source,
            LicenseSource::NpmLockfile
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
