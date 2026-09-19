pub mod cargo;
pub mod discover;
pub mod model;
pub mod npm;
pub mod policy;
pub mod python;
pub mod report;
pub mod spdx;

use crate::model::Dependency;

/// Collapses the same package+version found via several manifests into
/// one row. Scanning a directory of sibling Rust crates otherwise reports
/// `serde 1.0.229` once per crate, which buries the findings that matter.
/// The first-seen manifest is kept as the `origin`.
pub fn dedup_across_manifests(deps: &mut Vec<Dependency>) {
    let mut seen = std::collections::HashSet::new();
    deps.retain(|d| seen.insert((d.ecosystem, d.name.clone(), d.version.clone())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Ecosystem;

    #[test]
    fn the_same_crate_pulled_in_by_two_manifests_is_reported_once() {
        let mut deps = vec![
            Dependency::unresolved(Ecosystem::Cargo, "serde", "1.0.229", "a/Cargo.toml"),
            Dependency::unresolved(Ecosystem::Cargo, "serde", "1.0.229", "b/Cargo.toml"),
            Dependency::unresolved(Ecosystem::Cargo, "serde", "1.0.200", "b/Cargo.toml"),
            Dependency::unresolved(Ecosystem::Npm, "serde", "1.0.229", "ui/package-lock.json"),
        ];
        dedup_across_manifests(&mut deps);
        assert_eq!(
            deps.len(),
            3,
            "different versions and ecosystems stay distinct"
        );
        assert_eq!(deps[0].origin, "a/Cargo.toml", "the first manifest wins");
    }
}
