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
    pub missing_target: bool,
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

pub struct RemoteJob<'a> {
    pub work_dir: &'a str,
    pub ws_dir: &'a str,
    pub sync_dir: &'a str,
    pub env_mode: EnvMode,
    pub target_cpu: Option<&'a str>,
    pub rustflags_var: &'a str,
    pub check_target: Option<&'a str>,
    pub cargo_cmd: &'a [String],
}

pub fn build_remote_script(job: &RemoteJob<'_>) -> String {
    let mut parts = Vec::new();
    parts.push(format!("cd {}", job.work_dir));

    if std::io::stderr().is_terminal() || std::io::stdout().is_terminal() {
        parts.push("export CARGO_TERM_COLOR=always".to_string());
    }

    // Prevent remote host's own RUSTFLAGS (e.g. -C target-cpu=znver5) from leaking into builds.
    parts.push("unset RUSTFLAGS".to_string());
    if let Some(cpu) = job.target_cpu {
        parts.push(format!(
            "export {}=\"-C target-cpu={cpu}\"",
            job.rustflags_var
        ));
    }

    let quoted_cargo: Vec<String> = job.cargo_cmd.iter().map(|a| shell_quote(a)).collect();
    let base_cargo = format!("cargo {}", quoted_cargo.join(" "));
    let cargo_line = match job.check_target {
        Some(triple) => {
            let q_triple = shell_quote(triple);
            let inner = format!(
                "libdir=$(rustc --target {q_triple} --print target-libdir 2>/dev/null) && [ -d \"$libdir\" ] || {{ echo \"cargo-anywhere: target '{triple}' is not installed on remote host (install via 'rustup target add {triple}' or Nix/devenv)\" >&2; exit 2; }} && {base_cargo}"
            );
            format!("sh -c {}", shell_quote(&inner))
        }
        None => base_cargo,
    };
    let run_in_work = shell_quote(&format!("cd {} && {cargo_line}", job.work_dir));

    let wrapped = match job.env_mode {
        EnvMode::None => cargo_line,
        EnvMode::Devenv => format!("devenv shell -- {cargo_line}"),
        EnvMode::Nix => format!("nix develop {} -c {cargo_line}", job.ws_dir),
        EnvMode::Auto => format!(
            "if [ -f {ws}/devenv.nix ]; then (cd {ws} && devenv shell -- sh -c {run_in_work}); \
             elif [ -f {sync}/devenv.nix ]; then (cd {sync} && devenv shell -- sh -c {run_in_work}); \
             elif [ -f {ws}/flake.nix ]; then nix develop {ws} -c {cargo_line}; \
             elif [ -f {sync}/flake.nix ]; then nix develop {sync} -c {cargo_line}; \
             else {cargo_line}; fi",
            ws = job.ws_dir,
            sync = job.sync_dir,
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
    let mut missing_target = false;

    for line in reader.lines() {
        let line = line?;
        if let Ok(val) = serde_json::from_str::<Value>(&line) {
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
                        if rendered.contains("can't find crate for `core`")
                            || rendered.contains("target may not be installed")
                        {
                            missing_target = true;
                        }
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
        missing_target,
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
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn builds_devenv_script_with_target_cpu() {
        let cmd = vec!["build".into(), "--release".into()];
        let script = build_remote_script(&RemoteJob {
            work_dir: "~/remote-builds/abc/bevy",
            ws_dir: "~/remote-builds/abc/bevy",
            sync_dir: "~/remote-builds/abc",
            env_mode: EnvMode::Devenv,
            target_cpu: Some("znver4"),
            rustflags_var: "RUSTFLAGS",
            check_target: None,
            cargo_cmd: &cmd,
        });
        assert!(script.contains("RUSTFLAGS=\"-C target-cpu=znver4\""));
        assert!(script.contains("devenv shell -- cargo build --release"));
    }

    #[test]
    fn builds_auto_and_none_scripts() {
        let check_cmd = vec!["check".into()];
        let auto = build_remote_script(&RemoteJob {
            work_dir: "~/remote-builds/abc/app",
            ws_dir: "~/remote-builds/abc/app",
            sync_dir: "~/remote-builds/abc",
            env_mode: EnvMode::Auto,
            target_cpu: None,
            rustflags_var: "RUSTFLAGS",
            check_target: None,
            cargo_cmd: &check_cmd,
        });
        assert!(auto.contains("unset RUSTFLAGS"));
        assert!(!auto.contains("target-cpu"));
        assert!(auto.contains("devenv.nix"));
        assert!(auto.contains("flake.nix"));

        let test_cmd = vec!["test".into()];
        let plain = build_remote_script(&RemoteJob {
            work_dir: "~/remote-builds/abc",
            ws_dir: "~/remote-builds/abc",
            sync_dir: "~/remote-builds/abc",
            env_mode: EnvMode::None,
            target_cpu: None,
            rustflags_var: "RUSTFLAGS",
            check_target: Some("aarch64-unknown-linux-gnu"),
            cargo_cmd: &test_cmd,
        });
        assert!(plain.contains("rustc --target aarch64-unknown-linux-gnu --print target-libdir"));
        assert!(plain.contains("[ -d"));
        assert!(plain.contains("cargo test"));
    }
}
