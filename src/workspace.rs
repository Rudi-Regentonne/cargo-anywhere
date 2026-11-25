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
    let mut dirs: Vec<PathBuf> = vec![workspace_root.clone()];

    for pkg in &metadata.packages {
        if let Some(parent) = pkg.manifest_path.parent() {
            let pkg_dir = parent.as_std_path().to_path_buf();
            for dep in &pkg.dependencies {
                if let Some(dep_path) = &dep.path {
                    let std_dep = dep_path.as_std_path();
                    let abs = if std_dep.is_absolute() {
                        std_dep.to_path_buf()
                    } else {
                        pkg_dir.join(std_dep)
                    };
                    dirs.push(abs);
                }
            }
            dirs.push(pkg_dir);
        }
    }

    let sync_root = common_ancestor(&dirs).context("computing common ancestor directory")?;
    let canon_sync = sync_root
        .canonicalize()
        .unwrap_or_else(|_| sync_root.clone());
    let canon_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let canon_ws = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.clone());

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

pub fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?;
    let mut prefix: Vec<Component<'_>> = first.components().collect();

    for path in &paths[1..] {
        let comps: Vec<Component<'_>> = path.components().collect();
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
            PathBuf::from("/home/qajus/code/hellyeah/bevy"),
            PathBuf::from("/home/qajus/code/hellyeah/gdtf_parser"),
        ];
        assert_eq!(
            common_ancestor(&paths),
            Some(PathBuf::from("/home/qajus/code/hellyeah"))
        );
    }

    #[test]
    fn common_ancestor_handles_single_and_nested_paths() {
        let single = vec![PathBuf::from("/home/qajus/code/app")];
        assert_eq!(common_ancestor(&single), Some(PathBuf::from("/home/qajus/code/app")));

        let nested = vec![
            PathBuf::from("/a/b/c/d"),
            PathBuf::from("/a/b/e"),
            PathBuf::from("/a/b/c/f"),
        ];
        assert_eq!(common_ancestor(&nested), Some(PathBuf::from("/a/b")));
    }
}
