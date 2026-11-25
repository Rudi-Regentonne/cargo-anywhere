use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

pub fn check_reachable(host: &str) -> Result<()> {
    let out = Command::new("ssh")
        .args([
            "-o",
            "ConnectTimeout=3",
            "-o",
            "BatchMode=yes",
            host,
            "true",
        ])
        .output()
        .context("running ssh connectivity check")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reason = stderr.lines().last().unwrap_or("connection failed").trim();
        bail!("remote host '{host}' is unreachable ({reason})");
    }
    Ok(())
}

pub fn upload_sources(host: &str, remote_sync_dir: &str, local_sync_root: &Path) -> Result<()> {
    let status = Command::new("ssh")
        .args([host, &format!("mkdir -p {remote_sync_dir}")])
        .status()
        .context("running ssh mkdir")?;
    if !status.success() {
        bail!("failed to create remote directory on {host}");
    }

    let src = format!("{}/", local_sync_root.display());
    let dst = format!("{host}:{remote_sync_dir}/");

    let status = Command::new("rsync")
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
        bail!("rsync upload to {host} failed");
    }
    Ok(())
}

pub fn download_file(host: &str, remote_path: &str, local_path: &Path) -> Result<()> {
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let src = format!("{host}:{remote_path}");
    let status = Command::new("rsync")
        .args(["-az", &src, &local_path.to_string_lossy()])
        .status()
        .with_context(|| format!("downloading {src}"))?;
    if !status.success() {
        bail!("rsync failed downloading {src}");
    }
    Ok(())
}

pub fn download_target_dir(
    host: &str,
    remote_workspace_dir: &str,
    local_workspace_dir: &Path,
) -> Result<()> {
    let src = format!("{host}:{remote_workspace_dir}/target/");
    let dst = format!("{}/target/", local_workspace_dir.display());
    std::fs::create_dir_all(local_workspace_dir.join("target"))?;

    let status = Command::new("rsync")
        .args(["-az", "--delete", &src, &dst])
        .status()
        .context("running rsync target download")?;
    if !status.success() {
        bail!("rsync target download from {host} failed");
    }
    Ok(())
}
