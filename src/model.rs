use serde::Serialize;
use std::fmt;

/// Which package manager a dependency came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    Cargo,
    Npm,
    Pip,
}

impl fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Ecosystem::Cargo => "cargo",
            Ecosystem::Npm => "npm",
            Ecosystem::Pip => "pip",
        };
        f.write_str(s)
    }
}

/// Where the license string came from. Recorded per-dependency because
/// the three ecosystems have genuinely different levels of confidence,
/// and a report that hides that is misleading — see the README.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LicenseSource {
    /// `cargo metadata`'s `license` field, i.e. the crate's own manifest.
    CargoManifest,
    /// A `license` field npm itself wrote into `package-lock.json`.
    NpmLockfile,
    /// `node_modules/<pkg>/package.json`, read when the lockfile entry
    /// has no `license` field.
    NodeModules,
    /// `pip show` against the currently-active Python environment.
    PipShow,
    /// A `pip_licenses` entry in the user's config file.
    UserConfig,
    /// Nothing resolved it.
    Unresolved,
}

impl fmt::Display for LicenseSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LicenseSource::CargoManifest => "cargo-manifest",
            LicenseSource::NpmLockfile => "npm-lockfile",
            LicenseSource::NodeModules => "node_modules",
            LicenseSource::PipShow => "pip-show",
            LicenseSource::UserConfig => "user-config",
            LicenseSource::Unresolved => "unresolved",
        };
        f.write_str(s)
    }
}

/// One dependency and whatever license information could be established
/// for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dependency {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: String,
    /// The raw license string, exactly as found — an SPDX *expression*,
    /// not necessarily a single id, and not normalised. `None` when
    /// nothing could resolve it.
    pub license: Option<String>,
    pub license_source: LicenseSource,
    /// The manifest/lockfile this dependency was discovered in.
    pub origin: String,
}

impl Dependency {
    pub fn unresolved(
        ecosystem: Ecosystem,
        name: impl Into<String>,
        version: impl Into<String>,
        origin: impl Into<String>,
    ) -> Self {
        Dependency {
            ecosystem,
            name: name.into(),
            version: version.into(),
            license: None,
            license_source: LicenseSource::Unresolved,
            origin: origin.into(),
        }
    }
}
