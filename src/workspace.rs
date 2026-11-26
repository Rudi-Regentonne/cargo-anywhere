use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct ProjectLayout {
    pub workspace_root: PathBuf,
    /// Common ancestor of `workspace_root` and any local `path` dependencies.
    pub sync_root: PathBuf,
    /// Current working directory relative to `sync_root`.
    pub rel_cwd: PathBuf,
    /// Workspace root relative to `sync_root`.
    pub rel_workspace: PathBuf,
    /// Deterministic short hash of `sync_root` for the remote build directory.
    pub project_hash: String,
}

pub fn discover_layout(cwd: &Path) -> Result<ProjectLayout> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .current_dir(cwd)
        .exec()
        .context("running `cargo metadata --no-deps`")?;

    let workspace_root = metadata.workspace_root.into_std_path_buf();
    let canon_ws = resolve_dir(&workspace_root);
    let mut dirs: Vec<PathBuf> = vec![canon_ws.clone()];
    let mut visited: HashSet<PathBuf> = HashSet::new();

    for pkg in &metadata.packages {
        if let Some(parent) = pkg.manifest_path.parent() {
            let pkg_dir = resolve_dir(parent.as_std_path());
            dirs.push(pkg_dir.clone());
            visited.insert(pkg_dir.clone());
            for dep in &pkg.dependencies {
                if let Some(dep_path) = &dep.path {
                    let std_dep = dep_path.as_std_path();
                    let raw_dep = if std_dep.is_absolute() {
                        std_dep.to_path_buf()
                    } else {
                        pkg_dir.join(std_dep)
                    };
                    let resolved_dep = resolve_dir(&raw_dep);
                    dirs.push(resolved_dep.clone());
                    collect_transitive_path_deps(&resolved_dep, &mut visited, &mut dirs);
                }
            }
        }
    }

    let sync_root = common_ancestor(&dirs).context("computing common ancestor directory")?;
    let canon_sync = resolve_dir(&sync_root);
    let canon_cwd = resolve_dir(cwd);

    let rel_cwd = canon_cwd
        .strip_prefix(&canon_sync)
        .unwrap_or(Path::new(""))
        .to_path_buf();
    let rel_workspace = canon_ws
        .strip_prefix(&canon_sync)
        .unwrap_or(Path::new(""))
        .to_path_buf();

    let mut hasher = Sha256::new();
    hasher.update(canon_sync.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let project_hash = digest[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    Ok(ProjectLayout {
        workspace_root,
        sync_root: canon_sync,
        rel_cwd,
        rel_workspace,
        project_hash,
    })
}

fn resolve_dir(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| normalize_path(path))
}

/// Lexically normalizes `.` and `..` components without requiring the path to exist.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(comp.as_os_str());
                }
            }
            _ => out.push(comp.as_os_str()),
        }
    }
    out
}

fn collect_transitive_path_deps(
    crate_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    dirs: &mut Vec<PathBuf>,
) {
    if !visited.insert(crate_dir.to_path_buf()) {
        return;
    }
    let manifest_path = crate_dir.join("Cargo.toml");
    let Ok(raw) = fs::read_to_string(&manifest_path) else {
        return;
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return;
    };

    let mut dep_paths = Vec::new();
    collect_dep_paths_from_table(&doc, &mut dep_paths);
    if let Some(target_tbl) = doc.get("target").and_then(|v| v.as_table()) {
        for cfg_tbl in target_tbl.values() {
            collect_dep_paths_from_table(cfg_tbl, &mut dep_paths);
        }
    }

    for rel in dep_paths {
        let raw_dep = if rel.is_absolute() {
            rel
        } else {
            crate_dir.join(rel)
        };
        let resolved = resolve_dir(&raw_dep);
        dirs.push(resolved.clone());
        collect_transitive_path_deps(&resolved, visited, dirs);
    }
}

fn collect_dep_paths_from_table(tbl: &toml::Value, out: &mut Vec<PathBuf>) {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = tbl.get(section).and_then(|v| v.as_table()) {
            for spec in deps.values() {
                if let Some(p) = spec.get("path").and_then(|v| v.as_str()) {
                    out.push(PathBuf::from(p));
                }
            }
        }
    }
}

pub fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = normalize_path(paths.first()?);
    let mut prefix: Vec<Component<'_>> = first.components().collect();

    for path in &paths[1..] {
        let normalized = normalize_path(path);
        let comps: Vec<Component<'_>> = normalized.components().collect();
        let shared = prefix
            .iter()
            .zip(comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(shared);
    }

    if prefix.is_empty() {
        return None;
    }

    let mut out = PathBuf::new();
    for comp in prefix {
        out.push(comp.as_os_str());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_common_ancestor_for_sibling_path_dep() {
        let paths = vec![
            PathBuf::from("/home/user/code/workspace/app"),
            PathBuf::from("/home/user/code/workspace/lib"),
        ];
        assert_eq!(
            common_ancestor(&paths),
            Some(PathBuf::from("/home/user/code/workspace"))
        );
    }

    #[test]
    fn common_ancestor_handles_single_and_nested_paths() {
        let single = vec![PathBuf::from("/home/user/code/app")];
        assert_eq!(
            common_ancestor(&single),
            Some(PathBuf::from("/home/user/code/app"))
        );

        let nested = vec![
            PathBuf::from("/a/b/c/d"),
            PathBuf::from("/a/b/e"),
            PathBuf::from("/a/b/c/f"),
        ];
        assert_eq!(common_ancestor(&nested), Some(PathBuf::from("/a/b")));
    }

    #[test]
    fn common_ancestor_normalizes_parent_dir_components() {
        let paths = vec![
            PathBuf::from("/home/user/code/app"),
            PathBuf::from("/home/user/code/app/../lib"),
        ];
        assert_eq!(
            common_ancestor(&paths),
            Some(PathBuf::from("/home/user/code"))
        );
    }

    #[test]
    fn discover_layout_includes_sibling_and_transitive_path_deps() {
        let root = std::env::temp_dir().join(format!(
            "cargo-anywhere-ws-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);

        let app_dir = root.join("app");
        let lib_a_dir = root.join("lib_a");
        let lib_b_dir = root.join("nested/lib_b");

        fs::create_dir_all(app_dir.join("src")).unwrap();
        fs::create_dir_all(lib_a_dir.join("src")).unwrap();
        fs::create_dir_all(lib_b_dir.join("src")).unwrap();

        fs::write(
            app_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nlib_a = { path = \"../lib_a\" }\n",
        )
        .unwrap();
        fs::write(app_dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        fs::write(
            lib_a_dir.join("Cargo.toml"),
            "[package]\nname = \"lib_a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nlib_b = { path = \"../nested/lib_b\" }\n",
        )
        .unwrap();
        fs::write(lib_a_dir.join("src/lib.rs"), "").unwrap();

        fs::write(
            lib_b_dir.join("Cargo.toml"),
            "[package]\nname = \"lib_b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(lib_b_dir.join("src/lib.rs"), "").unwrap();

        let layout = discover_layout(&app_dir.join("src")).unwrap();
        let canon_root = root.canonicalize().unwrap();
        assert_eq!(layout.sync_root, canon_root);
        assert_eq!(layout.rel_workspace, PathBuf::from("app"));
        assert_eq!(layout.rel_cwd, PathBuf::from("app/src"));

        let _ = fs::remove_dir_all(&root);
    }
}
