use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::ResolvedConfig;

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
    let mut ssh_cmd = Command::new("ssh");
    ssh_cmd.args(cfg.ssh_identity_args());
    let status = ssh_cmd
        .args([&target, &format!("mkdir -p {remote_sync_dir}")])
        .status()
        .context("running ssh mkdir")?;
    if !status.success() {
        bail!("failed to create remote directory on {target}");
    }

    let src = format!("{}/", local_sync_root.display());
    let dst = format!("{target}:{remote_sync_dir}/");

    let mut rsync_cmd = Command::new("rsync");
    if let Some(rsh) = cfg.rsync_rsh() {
        rsync_cmd.args(["-e", &rsh]);
    }
    let status = rsync_cmd
        .args([
            "-az",
            "--delete",
            "--filter=:- .gitignore",
            "--exclude=.git",
            "--exclude=target",
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
    let src = format!("{target}:{remote_path}");
    let mut rsync_cmd = Command::new("rsync");
    if let Some(rsh) = cfg.rsync_rsh() {
        rsync_cmd.args(["-e", &rsh]);
    }
    let status = rsync_cmd
        .args(["-az", &src, &local_path.to_string_lossy()])
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
    let src = format!("{target}:{remote_workspace_dir}/target/");
    let dst = format!("{}/target/", local_workspace_dir.display());
    std::fs::create_dir_all(local_workspace_dir.join("target"))?;

    let mut rsync_cmd = Command::new("rsync");
    if let Some(rsh) = cfg.rsync_rsh() {
        rsync_cmd.args(["-e", &rsh]);
    }
    let status = rsync_cmd
        .args(["-az", "--delete", &src, &dst])
        .status()
        .context("running rsync target download")?;
    if !status.success() {
        bail!("rsync target download from {target} failed");
    }
    Ok(())
}
