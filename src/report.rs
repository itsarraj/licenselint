use serde::Serialize;

use crate::model::{Dependency, Ecosystem};
use crate::policy::{self, Policy, Verdict};

/// One dependency plus the policy's verdict on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub dep: Dependency,
    pub verdict: Verdict,
    pub violation: bool,
}

/// Applies the policy to every dependency, worst verdicts first so the
/// things you have to act on are at the top of a long report.
pub fn evaluate(deps: Vec<Dependency>, policy: &Policy) -> Vec<Finding> {
    let mut findings: Vec<Finding> = deps
        .into_iter()
        .map(|dep| {
            let verdict = policy::check(&dep, policy);
            let violation = verdict.is_violation(policy);
            Finding {
                dep,
                verdict,
                violation,
            }
        })
        .collect();
    findings.sort_by_key(|f| {
        let rank = match f.verdict {
            Verdict::Denied { .. } => 0,
            Verdict::NotAllowed => 1,
            Verdict::Unparseable { .. } => 2,
            Verdict::Unknown => 3,
            Verdict::Ok => 4,
        };
        (rank, f.dep.ecosystem.to_string(), f.dep.name.clone())
    });
    findings
}

/// Counts for the footer line.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub total: usize,
    pub cargo: usize,
    pub npm: usize,
    pub pip: usize,
    pub denied: usize,
    pub not_allowed: usize,
    pub unparseable: usize,
    pub unknown: usize,
    pub violations: usize,
}

pub fn summarize(findings: &[Finding]) -> Summary {
    let mut s = Summary {
        total: findings.len(),
        ..Summary::default()
    };
    for f in findings {
        match f.dep.ecosystem {
            Ecosystem::Cargo => s.cargo += 1,
            Ecosystem::Npm => s.npm += 1,
            Ecosystem::Pip => s.pip += 1,
        }
        match f.verdict {
            Verdict::Denied { .. } => s.denied += 1,
            Verdict::NotAllowed => s.not_allowed += 1,
            Verdict::Unparseable { .. } => s.unparseable += 1,
            Verdict::Unknown => s.unknown += 1,
            Verdict::Ok => {}
        }
        if f.violation {
            s.violations += 1;
        }
    }
    s
}

fn package_column(dep: &Dependency) -> String {
    format!("{} {}", dep.name, dep.version)
}

/// The human-readable report. `violations_only` drops the `ok` lines,
/// which is what you want in CI; the footer still counts everything, so
/// a quiet report never hides how much was actually scanned.
pub fn render_text(findings: &[Finding], violations_only: bool) -> String {
    let shown: Vec<&Finding> = findings
        .iter()
        .filter(|f| !violations_only || f.violation)
        .collect();

    let pkg_width = shown
        .iter()
        .map(|f| package_column(&f.dep).chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 44);

    let mut out = String::new();
    for f in &shown {
        let pkg = package_column(&f.dep);
        let license = f.dep.license.as_deref().unwrap_or("-");
        let note = match &f.verdict {
            Verdict::Denied { by } => format!("  (denied by {by})"),
            Verdict::NotAllowed => "  (not on the allowlist)".to_string(),
            Verdict::Unparseable { .. } => "  (not a parseable SPDX expression)".to_string(),
            Verdict::Unknown => format!("  (no license source; via {})", f.dep.license_source),
            Verdict::Ok => String::new(),
        };
        out.push_str(&format!(
            "{:<12} {:<6} {:<pkg_width$}  {}{}\n",
            f.verdict.label(),
            f.dep.ecosystem.to_string(),
            pkg,
            license,
            note
        ));
    }

    let s = summarize(findings);
    if violations_only && shown.is_empty() {
        out.push_str("no policy violations\n");
    }
    out.push_str(&format!(
        "\n{} dependencies scanned (cargo {}, npm {}, pip {}) — \
         {} denied, {} not-allowed, {} unparseable, {} unknown\n",
        s.total, s.cargo, s.npm, s.pip, s.denied, s.not_allowed, s.unparseable, s.unknown
    ));
    out
}

#[derive(Serialize)]
struct JsonFinding<'a> {
    #[serde(flatten)]
    dep: &'a Dependency,
    #[serde(flatten)]
    verdict: &'a Verdict,
    violation: bool,
}

#[derive(Serialize)]
struct JsonReport<'a> {
    summary: Summary,
    dependencies: Vec<JsonFinding<'a>>,
}

/// Machine-readable output for `--json`.
pub fn render_json(findings: &[Finding], violations_only: bool) -> serde_json::Result<String> {
    let report = JsonReport {
        summary: summarize(findings),
        dependencies: findings
            .iter()
            .filter(|f| !violations_only || f.violation)
            .map(|f| JsonFinding {
                dep: &f.dep,
                verdict: &f.verdict,
                violation: f.violation,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LicenseSource;

    fn dep(eco: Ecosystem, name: &str, version: &str, license: Option<&str>) -> Dependency {
        Dependency {
            ecosystem: eco,
            name: name.into(),
            version: version.into(),
            license: license.map(str::to_string),
            license_source: match license {
                Some(_) => LicenseSource::CargoManifest,
                None => LicenseSource::Unresolved,
            },
            origin: "fixture".into(),
        }
    }

    /// A mixed report built from real license strings observed in this
    /// monorepo (`serde`/`clap` from cargo, `caniuse-lite` from a real
    /// `package-lock.json`, `torch` from a real `requirements.txt`) plus
    /// the real GPL-3.0 crate used in the live denylist test.
    fn fixture() -> Vec<Dependency> {
        vec![
            dep(
                Ecosystem::Cargo,
                "serde",
                "1.0.229",
                Some("MIT OR Apache-2.0"),
            ),
            dep(Ecosystem::Cargo, "clap", "4.6.6", Some("MIT OR Apache-2.0")),
            dep(Ecosystem::Cargo, "kmon", "1.7.1", Some("GPL-3.0")),
            dep(
                Ecosystem::Npm,
                "caniuse-lite",
                "1.0.30001700",
                Some("CC-BY-4.0"),
            ),
            dep(Ecosystem::Pip, "torch", ">=2.2.0", None),
        ]
    }

    fn policy(allow: &[&str], deny: &[&str]) -> Policy {
        Policy {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            ..Policy::default()
        }
    }

    #[test]
    fn violations_are_sorted_to_the_top() {
        let findings = evaluate(fixture(), &policy(&[], &["GPL-3.0"]));
        assert_eq!(findings[0].dep.name, "kmon");
        assert!(findings[0].violation);
        assert!(findings.iter().skip(1).all(|f| !f.violation));
    }

    #[test]
    fn summary_counts_every_ecosystem_and_verdict() {
        let s = summarize(&evaluate(
            fixture(),
            &policy(&["MIT", "Apache-2.0"], &["GPL-3.0"]),
        ));
        assert_eq!(s.total, 5);
        assert_eq!((s.cargo, s.npm, s.pip), (3, 1, 1));
        assert_eq!(s.denied, 1, "kmon");
        assert_eq!(s.not_allowed, 1, "caniuse-lite's CC-BY-4.0");
        assert_eq!(s.unknown, 1, "torch");
        assert_eq!(
            s.violations, 3,
            "unknown counts as a violation under an allowlist"
        );
    }

    #[test]
    fn unknown_is_not_a_violation_when_only_a_denylist_is_in_force() {
        let s = summarize(&evaluate(fixture(), &policy(&[], &["GPL-3.0"])));
        assert_eq!(s.unknown, 1);
        assert_eq!(s.violations, 1, "only the GPL-3.0 crate");
    }

    #[test]
    fn text_report_names_the_offending_license() {
        let text = render_text(&evaluate(fixture(), &policy(&[], &["GPL-3.0"])), false);
        let line = text.lines().find(|l| l.contains("kmon")).unwrap();
        assert!(line.starts_with("DENIED"), "got: {line}");
        assert!(line.contains("GPL-3.0"));
        assert!(line.contains("(denied by GPL-3.0)"), "got: {line}");
        assert!(text.contains("serde 1.0.229"));
        assert!(text.contains("MIT OR Apache-2.0"));
    }

    #[test]
    fn an_unresolved_pip_dependency_prints_a_dash_not_an_empty_column() {
        let text = render_text(&evaluate(fixture(), &policy(&[], &[])), false);
        let line = text.lines().find(|l| l.contains("torch")).unwrap();
        assert!(line.contains(" - "), "got: {line}");
        assert!(line.contains("no license source"), "got: {line}");
    }

    #[test]
    fn violations_only_hides_ok_lines_but_the_footer_still_counts_them_all() {
        let text = render_text(&evaluate(fixture(), &policy(&[], &["GPL-3.0"])), true);
        assert!(text.contains("kmon"));
        assert!(!text.contains("serde"), "ok lines must be hidden");
        assert!(
            text.contains("5 dependencies scanned"),
            "the footer must still report everything scanned: {text}"
        );
    }

    #[test]
    fn a_clean_violations_only_run_says_so_explicitly() {
        let text = render_text(&evaluate(fixture(), &policy(&[], &["AGPL-3.0"])), true);
        assert!(text.contains("no policy violations"), "got: {text}");
    }

    #[test]
    fn json_output_carries_the_verdict_and_the_summary() {
        let json = render_json(&evaluate(fixture(), &policy(&[], &["GPL-3.0"])), false).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["summary"]["denied"], 1);
        assert_eq!(parsed["summary"]["total"], 5);
        let kmon = parsed["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "kmon")
            .unwrap();
        assert_eq!(kmon["verdict"], "denied");
        assert_eq!(kmon["by"], "GPL-3.0");
        assert_eq!(kmon["ecosystem"], "cargo");
        assert_eq!(kmon["license"], "GPL-3.0");
        assert_eq!(kmon["violation"], true);
    }
}
