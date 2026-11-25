use std::io::{BufRead, BufReader, IsTerminal};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::cli::EnvMode;
use crate::config::ResolvedConfig;

#[derive(Debug, Default)]
pub struct RemoteBuildOutput {
    pub success: bool,
    pub exit_code: i32,
    pub executables: Vec<String>,
}

pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b':' | b'=' | b'+'))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn build_remote_script(
    remote_work_dir: &str,
    remote_ws_dir: &str,
    remote_sync_dir: &str,
    env_mode: EnvMode,
    target_cpu: Option<&str>,
    cargo_cmd: &[String],
) -> String {
    let mut parts = Vec::new();
    parts.push(format!("cd {remote_work_dir}"));

    if std::io::stderr().is_terminal() || std::io::stdout().is_terminal() {
        parts.push("export CARGO_TERM_COLOR=always".to_string());
    }

    if let Some(cpu) = target_cpu {
        parts.push(format!("export RUSTFLAGS=\"-C target-cpu={cpu}\""));
    }

    let quoted_cargo: Vec<String> = cargo_cmd.iter().map(|a| shell_quote(a)).collect();
    let cargo_line = format!("cargo {}", quoted_cargo.join(" "));
    let run_in_work = shell_quote(&format!("cd {remote_work_dir} && {cargo_line}"));

    let wrapped = match env_mode {
        EnvMode::None => cargo_line,
        EnvMode::Devenv => format!("devenv shell -- {cargo_line}"),
        EnvMode::Nix => format!("nix develop {remote_ws_dir} -c {cargo_line}"),
        EnvMode::Auto => format!(
            "if [ -f {remote_ws_dir}/devenv.nix ]; then (cd {remote_ws_dir} && devenv shell -- sh -c {run_in_work}); \
             elif [ -f {remote_sync_dir}/devenv.nix ]; then (cd {remote_sync_dir} && devenv shell -- sh -c {run_in_work}); \
             elif [ -f {remote_ws_dir}/flake.nix ]; then nix develop {remote_ws_dir} -c {cargo_line}; \
             elif [ -f {remote_sync_dir}/flake.nix ]; then nix develop {remote_sync_dir} -c {cargo_line}; \
             else {cargo_line}; fi"
        ),
    };

    parts.push(wrapped);
    parts.join(" && ")
}

pub fn run_streaming(cfg: &ResolvedConfig, script: &str) -> Result<i32> {
    let target = cfg.ssh_target();
    let mut cmd = Command::new("ssh");
    cmd.arg("-t");
    cmd.args(cfg.ssh_identity_args());
    let status = cmd
        .args([&target, script])
        .status()
        .context("executing remote ssh command")?;
    Ok(status.code().unwrap_or(1))
}

pub fn run_capturing_executables(cfg: &ResolvedConfig, script: &str) -> Result<RemoteBuildOutput> {
    let target = cfg.ssh_target();
    let mut cmd = Command::new("ssh");
    cmd.args(cfg.ssh_identity_args());
    let mut child = cmd
        .args([&target, script])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawning remote cargo build")?;

    let stdout = child.stdout.take().context("capturing remote stdout")?;
    let reader = BufReader::new(stdout);
    let mut executables = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if let Some(val) = serde_json::from_str::<Value>(&line).ok() {
            match val.get("reason").and_then(|r| r.as_str()) {
                Some("compiler-artifact") => {
                    if let Some(exe) = val.get("executable").and_then(|e| e.as_str()) {
                        executables.push(exe.to_string());
                    }
                }
                Some("compiler-message") => {
                    if let Some(rendered) = val
                        .get("message")
                        .and_then(|m| m.get("rendered"))
                        .and_then(|r| r.as_str())
                    {
                        eprint!("{rendered}");
                    }
                }
                _ => {}
            }
        } else {
            println!("{line}");
        }
    }

    let status = child.wait()?;
    let exit_code = status.code().unwrap_or(1);
    Ok(RemoteBuildOutput {
        success: status.success(),
        exit_code,
        executables,
    })
}

pub fn gc_remote(cfg: &ResolvedConfig) -> Result<()> {
    let target = cfg.ssh_target();
    let cmd_str = format!(
        "rm -rf {}/* && echo \"Cleaned {}\"",
        cfg.remote_dir, cfg.remote_dir
    );
    let mut cmd = Command::new("ssh");
    cmd.args(cfg.ssh_identity_args());
    let status = cmd
        .args([&target, &cmd_str])
        .status()
        .context("running remote gc")?;
    if !status.success() {
        bail!("remote gc failed on {target}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_special_args() {
        assert_eq!(shell_quote("--release"), "--release");
        assert_eq!(shell_quote("a b"), "'a b'");
    }

    #[test]
    fn builds_devenv_script_with_target_cpu() {
        let script = build_remote_script(
            "~/remote-builds/abc/bevy",
            "~/remote-builds/abc/bevy",
            "~/remote-builds/abc",
            EnvMode::Devenv,
            Some("znver4"),
            &["build".into(), "--release".into()],
        );
        assert!(script.contains("RUSTFLAGS"));
        assert!(script.contains("-C target-cpu=znver4"));
        assert!(script.contains("devenv shell -- cargo build --release"));
    }
}
