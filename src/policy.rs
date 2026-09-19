use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::Dependency;
use crate::spdx;

/// The allow/deny policy, plus the hand-maintained pip license map (see
/// `python.rs` for why pip needs one and the other two ecosystems don't).
///
/// Deserialises straight from the `--config` JSON file; every field is
/// optional so a config containing only `{"deny": [...]}` is valid.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    /// Package name -> SPDX license, for pip dependencies.
    #[serde(default)]
    pub pip_licenses: BTreeMap<String, String>,
    /// Treat a dependency whose license could not be resolved as a
    /// failure even when no allowlist is in force.
    #[serde(default)]
    pub fail_unknown: bool,
}

/// The outcome of checking one dependency against the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum Verdict {
    /// Passes every rule in force.
    Ok,
    /// Unavoidably requires a denylisted license.
    Denied { by: String },
    /// An allowlist is in force and this cannot be satisfied by it.
    NotAllowed,
    /// No license string at all.
    Unknown,
    /// There was a license string, but it is not a parseable SPDX
    /// expression (`"see LICENSE"`, `"BSD-like"`-style junk) so no rule
    /// could be applied to it honestly.
    Unparseable { raw: String },
}

impl Verdict {
    /// Whether this verdict should make the run fail. `Unknown` is a
    /// failure only if an allowlist is in force (an unresolved license
    /// cannot be *proved* to be on it) or `fail_unknown` was set —
    /// otherwise it is reported but tolerated, because with pip in the
    /// mix unknowns are the normal case, not an anomaly.
    pub fn is_violation(&self, policy: &Policy) -> bool {
        match self {
            Verdict::Ok => false,
            Verdict::Denied { .. } | Verdict::NotAllowed => true,
            Verdict::Unknown | Verdict::Unparseable { .. } => {
                policy.fail_unknown || !policy.allow.is_empty()
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Denied { .. } => "DENIED",
            Verdict::NotAllowed => "NOT-ALLOWED",
            Verdict::Unknown => "unknown",
            Verdict::Unparseable { .. } => "unparseable",
        }
    }
}

/// Applies the policy to one dependency.
///
/// Order matters: the denylist is checked first, so that a dependency
/// which is both denylisted and absent from the allowlist reports the
/// more specific, more actionable reason.
pub fn check(dep: &Dependency, policy: &Policy) -> Verdict {
    let Some(raw) = dep.license.as_deref() else {
        return Verdict::Unknown;
    };
    let expr = match spdx::parse(raw) {
        Ok(e) => e,
        Err(_) => {
            return Verdict::Unparseable {
                raw: raw.to_string(),
            }
        }
    };

    if !policy.deny.is_empty() && spdx::violates_denylist(&expr, &policy.deny) {
        let by = spdx::offending_id(&expr, &policy.deny).unwrap_or_else(|| raw.to_string());
        return Verdict::Denied { by };
    }
    if !policy.allow.is_empty() && !spdx::satisfies_allowlist(&expr, &policy.allow) {
        return Verdict::NotAllowed;
    }
    Verdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ecosystem, LicenseSource};

    fn dep(license: Option<&str>) -> Dependency {
        Dependency {
            ecosystem: Ecosystem::Cargo,
            name: "somecrate".into(),
            version: "1.0.0".into(),
            license: license.map(|s| s.to_string()),
            license_source: LicenseSource::CargoManifest,
            origin: "Cargo.toml".into(),
        }
    }

    fn policy(allow: &[&str], deny: &[&str]) -> Policy {
        Policy {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            ..Policy::default()
        }
    }

    #[test]
    fn a_denylisted_license_is_denied_and_names_the_offending_id() {
        let v = check(
            &dep(Some("GPL-3.0")),
            &policy(&[], &["GPL-3.0", "AGPL-3.0"]),
        );
        assert_eq!(
            v,
            Verdict::Denied {
                by: "GPL-3.0".into()
            }
        );
        assert!(v.is_violation(&policy(&[], &["GPL-3.0"])));
    }

    #[test]
    fn real_dual_licensed_crates_from_this_repo_pass_a_permissive_allowlist() {
        // Every one of these is a verbatim license string taken from
        // `cargo metadata` on tools/dsnmask in this monorepo.
        let permissive = policy(
            &["MIT", "Apache-2.0", "Unlicense", "Unicode-3.0"],
            &["GPL-3.0"],
        );
        for real in [
            "MIT OR Apache-2.0",                   // serde, clap, anyhow, url, regex
            "Unlicense OR MIT",                    // memchr, aho-corasick
            "Apache-2.0 OR MIT",                   // utf8_iter, utf8parse
            "MIT",                                 // strsim, synstructure
            "Unicode-3.0",                         // the whole icu_* family
            "(MIT OR Apache-2.0) AND Unicode-3.0", // unicode-ident
        ] {
            assert_eq!(check(&dep(Some(real)), &permissive), Verdict::Ok, "{real}");
        }
    }

    #[test]
    fn dropping_unicode_3_0_from_the_allowlist_flags_the_real_icu_crates() {
        // A genuinely useful real-world result: a "MIT/Apache only"
        // allowlist rejects most of the modern `url` dependency tree,
        // because `idna` pulled in the ICU crates under Unicode-3.0.
        let strict = policy(&["MIT", "Apache-2.0"], &[]);
        assert_eq!(
            check(&dep(Some("Unicode-3.0")), &strict),
            Verdict::NotAllowed
        );
        assert_eq!(
            check(&dep(Some("(MIT OR Apache-2.0) AND Unicode-3.0")), &strict),
            Verdict::NotAllowed
        );
        assert_eq!(check(&dep(Some("MIT OR Apache-2.0")), &strict), Verdict::Ok);
    }

    #[test]
    fn dual_licensed_with_one_denied_half_is_not_a_violation() {
        let v = check(&dep(Some("MIT OR GPL-3.0")), &policy(&[], &["GPL-3.0"]));
        assert_eq!(v, Verdict::Ok);
    }

    #[test]
    fn denylist_wins_over_allowlist_for_a_clearer_message() {
        let both = policy(&["MIT"], &["GPL-3.0"]);
        assert_eq!(
            check(&dep(Some("GPL-3.0")), &both),
            Verdict::Denied {
                by: "GPL-3.0".into()
            }
        );
    }

    #[test]
    fn unknown_is_tolerated_under_deny_only_but_fails_under_an_allowlist() {
        let deny_only = policy(&[], &["GPL-3.0"]);
        let with_allow = policy(&["MIT"], &[]);
        let v = check(&dep(None), &deny_only);
        assert_eq!(v, Verdict::Unknown);
        assert!(
            !v.is_violation(&deny_only),
            "with pip in the mix unknowns are normal; deny-only must not fail on them"
        );
        assert!(
            v.is_violation(&with_allow),
            "an unresolved license cannot be proved to be on an allowlist"
        );
    }

    #[test]
    fn fail_unknown_makes_unknown_a_violation_even_with_no_allowlist() {
        let strict = Policy {
            deny: vec!["GPL-3.0".into()],
            fail_unknown: true,
            ..Policy::default()
        };
        assert!(check(&dep(None), &strict).is_violation(&strict));
    }

    #[test]
    fn junk_license_text_is_unparseable_not_silently_ok() {
        // npm packages really do ship things like this.
        let v = check(
            &dep(Some("SEE LICENSE IN LICENSE.md")),
            &policy(&["MIT"], &[]),
        );
        assert!(matches!(v, Verdict::Unparseable { .. }));
        assert!(v.is_violation(&policy(&["MIT"], &[])));
    }

    #[test]
    fn config_json_deserialises_with_only_some_fields_present() {
        let p: Policy = serde_json::from_str(r#"{"deny":["GPL-3.0"]}"#).unwrap();
        assert_eq!(p.deny, vec!["GPL-3.0".to_string()]);
        assert!(p.allow.is_empty());
        assert!(p.pip_licenses.is_empty());
        assert!(!p.fail_unknown);

        let p: Policy = serde_json::from_str(
            r#"{"allow":["MIT"],"pip_licenses":{"torch":"BSD-3-Clause"},"fail_unknown":true}"#,
        )
        .unwrap();
        assert_eq!(
            p.pip_licenses.get("torch").map(String::as_str),
            Some("BSD-3-Clause")
        );
        assert!(p.fail_unknown);
    }
}
