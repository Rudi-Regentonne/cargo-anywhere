use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::ResolvedConfig;
use crate::remote::quote_remote_path;

/// Formats a remote path for `rsync` with `--protect-args` (`-s`):
/// no shell quoting is needed, and a leading `~/` is stripped because
/// `rsync` resolves relative remote paths from the user's home directory.
fn rsync_remote_path(path: &str) -> String {
    if path == "~" {
        ".".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        if rest.is_empty() {
            ".".to_string()
        } else {
            rest.to_string()
        }
    } else {
        path.to_string()
    }
}

pub fn check_reachable(cfg: &ResolvedConfig) -> Result<String> {
    let target = cfg.ssh_target();
    let mut cmd = Command::new("ssh");
    cmd.args(["-o", "ConnectTimeout=3", "-o", "BatchMode=yes"]);
    cmd.args(cfg.ssh_identity_args());
    let out = cmd
        .args([&target, "uname -m"])
        .output()
        .context("running ssh connectivity check")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reason = stderr.lines().last().unwrap_or("connection failed").trim();
        bail!("remote host '{target}' is unreachable ({reason})");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn upload_sources(
    cfg: &ResolvedConfig,
    remote_sync_dir: &str,
    local_sync_root: &Path,
) -> Result<()> {
    let target = cfg.ssh_target();
    let q_remote_sync_dir = quote_remote_path(remote_sync_dir);
    let mut ssh_cmd = Command::new("ssh");
    ssh_cmd.args(cfg.ssh_identity_args());
    let status = ssh_cmd
        .args([&target, &format!("mkdir -p {q_remote_sync_dir}")])
        .status()
        .context("running ssh mkdir")?;
    if !status.success() {
        bail!("failed to create remote directory on {target}");
    }

    let src = format!("{}/", local_sync_root.display());
    let dst = format!("{target}:{}/", rsync_remote_path(remote_sync_dir));

    let mut rsync_cmd = Command::new("rsync");
    if let Some(rsh) = cfg.rsync_rsh() {
        rsync_cmd.args(["-e", &rsh]);
    }
    let status = rsync_cmd
        .args([
            "-azs",
            "--delete",
            "--filter=:- .gitignore",
            "--exclude=.git",
            "--exclude=/target",
            &src,
            &dst,
        ])
        .status()
        .context("running rsync upload")?;

    if !status.success() {
        bail!("rsync upload to {target} failed");
    }
    Ok(())
}

pub fn download_file(cfg: &ResolvedConfig, remote_path: &str, local_path: &Path) -> Result<()> {
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let target = cfg.ssh_target();
    let src = format!("{target}:{}", rsync_remote_path(remote_path));
    let mut rsync_cmd = Command::new("rsync");
    if let Some(rsh) = cfg.rsync_rsh() {
        rsync_cmd.args(["-e", &rsh]);
    }
    let status = rsync_cmd
        .args(["-azs", &src, &local_path.to_string_lossy()])
        .status()
        .with_context(|| format!("downloading {src}"))?;
    if !status.success() {
        bail!("rsync failed downloading {src}");
    }
    Ok(())
}

pub fn download_target_dir(
    cfg: &ResolvedConfig,
    remote_workspace_dir: &str,
    local_workspace_dir: &Path,
) -> Result<()> {
    let target = cfg.ssh_target();
    let src = format!("{target}:{}/target/", rsync_remote_path(remote_workspace_dir));
    let dst = format!("{}/target/", local_workspace_dir.display());
    std::fs::create_dir_all(local_workspace_dir.join("target"))?;

    let mut rsync_cmd = Command::new("rsync");
    if let Some(rsh) = cfg.rsync_rsh() {
        rsync_cmd.args(["-e", &rsh]);
    }
    let status = rsync_cmd
        .args(["-azs", "--delete", &src, &dst])
        .status()
        .context("running rsync target download")?;
    if !status.success() {
        bail!("rsync target download from {target} failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rsync_remote_paths_without_shell_quotes() {
        assert_eq!(rsync_remote_path("~"), ".");
        assert_eq!(
            rsync_remote_path("~/remote-builds/abc"),
            "remote-builds/abc"
        );
        assert_eq!(
            rsync_remote_path("~/my builds/abc"),
            "my builds/abc"
        );
        assert_eq!(
            rsync_remote_path("/tmp/my builds/abc"),
            "/tmp/my builds/abc"
        );
    }
}
