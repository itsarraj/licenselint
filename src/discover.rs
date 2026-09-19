//! Finding the manifests worth scanning, so that pointing this at a
//! polyglot repo root Just Works instead of needing three separate
//! invocations with three hand-typed paths.

use std::path::{Path, PathBuf};

/// A manifest to scan, tagged with the ecosystem it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Cargo(PathBuf),
    Npm(PathBuf),
    Pip(PathBuf),
}

impl Target {
    pub fn path(&self) -> &Path {
        match self {
            Target::Cargo(p) | Target::Npm(p) | Target::Pip(p) => p,
        }
    }
}

/// Directories that are either build output or a vendored copy of
/// dependencies we already read about from the lockfile. Descending into
/// `node_modules` in particular would turn a scan of one app into a walk
/// over tens of thousands of files for no new information.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".venv",
    "venv",
    "dist",
    "build",
    "vendor",
    ".next",
    "__pycache__",
];

/// Classifies a file by name alone. `requirements-matcher.txt` and
/// `requirements-dev.txt` count — this repo's real Python files use the
/// suffixed form.
pub fn classify(file_name: &str) -> Option<fn(PathBuf) -> Target> {
    if file_name == "Cargo.toml" {
        Some(Target::Cargo)
    } else if file_name == "package-lock.json" {
        Some(Target::Npm)
    } else if file_name.starts_with("requirements") && file_name.ends_with(".txt") {
        Some(Target::Pip)
    } else {
        None
    }
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Target>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Not following symlinks keeps a self-referential link from
        // turning the walk into an infinite loop.
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !SKIP_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                subdirs.push(path);
            }
        } else if file_type.is_file() {
            if let Some(ctor) = entry.file_name().to_str().and_then(classify) {
                out.push(ctor(path));
            }
        }
    }
    for sub in subdirs {
        walk(&sub, depth - 1, out);
    }
}

/// Drops any `Cargo.toml` that lives underneath another discovered
/// `Cargo.toml` — those are workspace members or path dependencies, and
/// `cargo metadata` on the outer manifest already covers them. Without
/// this, scanning a repo root would run `cargo metadata` once per crate
/// and report the same transitive dependencies many times over.
fn drop_nested_cargo_manifests(targets: &mut Vec<Target>) {
    let roots: Vec<PathBuf> = targets
        .iter()
        .filter_map(|t| match t {
            Target::Cargo(p) => p.parent().map(Path::to_path_buf),
            _ => None,
        })
        .collect();
    targets.retain(|t| match t {
        Target::Cargo(p) => {
            let dir = match p.parent() {
                Some(d) => d,
                None => return true,
            };
            !roots.iter().any(|r| r != dir && dir.starts_with(r))
        }
        _ => true,
    });
}

/// Expands the user's arguments into concrete targets. A file argument is
/// classified directly (so `licenselint some/odd-name.json` still works
/// if it is named like a lockfile); a directory is walked.
pub fn discover(paths: &[PathBuf], max_depth: usize) -> Vec<Target> {
    let mut targets = Vec::new();
    for path in paths {
        if path.is_file() {
            if let Some(ctor) = path.file_name().and_then(|n| n.to_str()).and_then(classify) {
                targets.push(ctor(path.clone()));
            }
        } else if path.is_dir() {
            walk(path, max_depth, &mut targets);
        }
    }
    targets.sort();
    targets.dedup();
    drop_nested_cargo_manifests(&mut targets);
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(name: &str) -> Option<Target> {
        classify(name).map(|ctor| ctor(PathBuf::from(name)))
    }

    #[test]
    fn classifies_the_three_manifest_kinds() {
        assert!(matches!(kind_of("Cargo.toml"), Some(Target::Cargo(_))));
        assert!(matches!(kind_of("package-lock.json"), Some(Target::Npm(_))));
        assert!(matches!(kind_of("requirements.txt"), Some(Target::Pip(_))));
        // The real spelling used by this monorepo's Python fixtures.
        assert!(matches!(
            kind_of("requirements-matcher.txt"),
            Some(Target::Pip(_))
        ));
    }

    #[test]
    fn ignores_unrelated_files() {
        for name in ["package.json", "Cargo.lock", "README.md", "requirements.in"] {
            assert!(kind_of(name).is_none(), "{name} should not be a target");
        }
    }

    #[test]
    fn a_cargo_manifest_nested_under_another_one_is_dropped() {
        let mut targets = vec![
            Target::Cargo(PathBuf::from("/repo/app/Cargo.toml")),
            Target::Cargo(PathBuf::from("/repo/app/crates/inner/Cargo.toml")),
            Target::Cargo(PathBuf::from("/repo/other/Cargo.toml")),
            Target::Npm(PathBuf::from("/repo/app/ui/package-lock.json")),
        ];
        drop_nested_cargo_manifests(&mut targets);
        assert_eq!(
            targets,
            vec![
                Target::Cargo(PathBuf::from("/repo/app/Cargo.toml")),
                Target::Cargo(PathBuf::from("/repo/other/Cargo.toml")),
                // An npm lockfile inside a Rust crate is still its own
                // target — the two ecosystems do not nest.
                Target::Npm(PathBuf::from("/repo/app/ui/package-lock.json")),
            ]
        );
    }

    #[test]
    fn sibling_crates_are_all_kept() {
        // The shape of this monorepo's own `tools/` directory: no root
        // manifest, one crate per subdirectory.
        let mut targets = vec![
            Target::Cargo(PathBuf::from("/repo/tools/dsnmask/Cargo.toml")),
            Target::Cargo(PathBuf::from("/repo/tools/pgqueue/Cargo.toml")),
        ];
        drop_nested_cargo_manifests(&mut targets);
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn walks_a_real_directory_tree_and_skips_build_output() {
        let root = std::env::temp_dir().join(format!("licenselint-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("svc")).unwrap();
        std::fs::create_dir_all(root.join("ui/node_modules/left-pad")).unwrap();
        std::fs::create_dir_all(root.join("svc/target/debug")).unwrap();
        std::fs::create_dir_all(root.join("ai")).unwrap();
        std::fs::write(root.join("svc/Cargo.toml"), "[package]\nname = \"svc\"\n").unwrap();
        std::fs::write(root.join("svc/target/debug/Cargo.toml"), "").unwrap();
        std::fs::write(root.join("ui/package-lock.json"), "{}").unwrap();
        std::fs::write(
            root.join("ui/node_modules/left-pad/package-lock.json"),
            "{}",
        )
        .unwrap();
        std::fs::write(root.join("ai/requirements-matcher.txt"), "").unwrap();

        let found = discover(std::slice::from_ref(&root), 16);
        let names: Vec<String> = found
            .iter()
            .map(|t| t.path().strip_prefix(&root).unwrap().display().to_string())
            .collect();
        // Grouped by ecosystem (cargo, npm, pip), then by path.
        assert_eq!(
            names,
            vec![
                "svc/Cargo.toml",
                "ui/package-lock.json",
                "ai/requirements-matcher.txt",
            ],
            "build output under target/ and vendored copies under node_modules/ must be skipped"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
