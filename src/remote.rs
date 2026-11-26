use std::io::{self, BufRead, BufReader, IsTerminal, Write};
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

pub fn quote_remote_path(path: &str) -> String {
    if path == "~" {
        return "\"$HOME\"".to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if rest.is_empty() {
            return "\"$HOME\"".to_string();
        }
        return format!("\"$HOME\"/{}", shell_quote(rest));
    }
    shell_quote(path)
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
    let q_work = quote_remote_path(job.work_dir);
    let q_ws = quote_remote_path(job.ws_dir);
    let q_sync = quote_remote_path(job.sync_dir);

    let mut parts = Vec::new();
    parts.push(format!("cd {q_work}"));

    if std::io::stderr().is_terminal() || std::io::stdout().is_terminal() {
        parts.push("export CARGO_TERM_COLOR=always".to_string());
    }

    // Prevent remote host's own RUSTFLAGS (e.g. -C target-cpu=znver5) from leaking into builds.
    parts.push("unset RUSTFLAGS".to_string());
    if job.rustflags_var != "RUSTFLAGS" && job.target_cpu.is_none() {
        parts.push(format!("unset {}", job.rustflags_var));
    }
    if let Some(cpu) = job.target_cpu {
        let flag = shell_quote(&format!("-C target-cpu={cpu}"));
        parts.push(format!("export {}={flag}", job.rustflags_var));
    }

    let quoted_cargo: Vec<String> = job.cargo_cmd.iter().map(|a| shell_quote(a)).collect();
    let base_cargo = format!("cargo {}", quoted_cargo.join(" "));
    let cargo_line = match job.check_target {
        Some(triple) => {
            let q_triple = shell_quote(triple);
            let q_msg = shell_quote(&format!(
                "cargo-anywhere: target '{triple}' is not installed on remote host (install via 'rustup target add {triple}' or Nix/devenv)"
            ));
            let inner = format!(
                "libdir=$(rustc --target {q_triple} --print target-libdir 2>/dev/null) && [ -d \"$libdir\" ] || {{ printf '%s\\n' {q_msg} >&2; exit 2; }} && {base_cargo}"
            );
            format!("sh -c {}", shell_quote(&inner))
        }
        None => base_cargo,
    };
    let run_in_work = shell_quote(&format!("cd {q_work} && {cargo_line}"));

    let wrapped = match job.env_mode {
        EnvMode::None => cargo_line,
        EnvMode::Devenv => format!("devenv shell -- {cargo_line}"),
        EnvMode::Nix => format!("nix develop {q_ws} -c {cargo_line}"),
        EnvMode::Auto => format!(
            "if [ -f {q_ws}/devenv.nix ]; then (cd {q_ws} && devenv shell -- sh -c {run_in_work}); \
             elif [ -f {q_sync}/devenv.nix ]; then (cd {q_sync} && devenv shell -- sh -c {run_in_work}); \
             elif [ -f {q_ws}/flake.nix ]; then nix develop {q_ws} -c {cargo_line}; \
             elif [ -f {q_sync}/flake.nix ]; then nix develop {q_sync} -c {cargo_line}; \
             else {cargo_line}; fi",
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
    let mut out_handle = io::stdout().lock();
    let mut err_handle = io::stderr().lock();
    let (executables, missing_target) =
        parse_cargo_json_stream(reader, &mut out_handle, &mut err_handle)?;

    let status = child.wait()?;
    let exit_code = status.code().unwrap_or(1);
    Ok(RemoteBuildOutput {
        success: status.success(),
        exit_code,
        executables,
        missing_target,
    })
}

pub fn parse_cargo_json_stream<R: BufRead, W1: Write, W2: Write>(
    reader: R,
    non_json_out: &mut W1,
    diag_err: &mut W2,
) -> Result<(Vec<String>, bool)> {
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
                        write!(diag_err, "{rendered}")?;
                    }
                }
                _ => {}
            }
        } else {
            writeln!(non_json_out, "{line}")?;
        }
    }

    Ok((executables, missing_target))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RemoteStatus {
    pub arch: String,
    pub has_rsync: bool,
    pub cargo_version: Option<String>,
    pub has_devenv: bool,
    pub has_nix: bool,
    pub remote_dir_writable: bool,
}

pub fn build_status_script(remote_dir: &str) -> String {
    let q_dir = quote_remote_path(remote_dir.trim());
    format!(
        "echo \"arch=$(uname -m)\"; \
         if command -v rsync >/dev/null 2>&1; then echo \"rsync=1\"; else echo \"rsync=0\"; fi; \
         if command -v cargo >/dev/null 2>&1; then echo \"cargo=$(cargo --version 2>/dev/null | head -n1)\"; else echo \"cargo=\"; fi; \
         if command -v devenv >/dev/null 2>&1; then echo \"devenv=1\"; else echo \"devenv=0\"; fi; \
         if command -v nix >/dev/null 2>&1; then echo \"nix=1\"; else echo \"nix=0\"; fi; \
         if mkdir -p {q_dir} 2>/dev/null && [ -w {q_dir} ]; then echo \"remote_dir_writable=1\"; else echo \"remote_dir_writable=0\"; fi"
    )
}

pub fn parse_status_output(stdout: &str) -> RemoteStatus {
    let mut status = RemoteStatus::default();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            match k.trim() {
                "arch" => status.arch = v.to_string(),
                "rsync" => status.has_rsync = v == "1",
                "cargo" => {
                    if !v.is_empty() {
                        status.cargo_version = Some(v.to_string());
                    }
                }
                "devenv" => status.has_devenv = v == "1",
                "nix" => status.has_nix = v == "1",
                "remote_dir_writable" => status.remote_dir_writable = v == "1",
                _ => {}
            }
        }
    }
    status
}

pub fn check_remote_status(cfg: &ResolvedConfig) -> Result<RemoteStatus> {
    let target = cfg.ssh_target();
    let script = build_status_script(&cfg.remote_dir);
    let mut cmd = Command::new("ssh");
    cmd.args(["-o", "ConnectTimeout=3", "-o", "BatchMode=yes"]);
    cmd.args(cfg.ssh_identity_args());
    let out = cmd
        .args([&target, &script])
        .output()
        .context("running remote status check")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reason = stderr.lines().last().unwrap_or("connection failed").trim();
        bail!("remote host '{target}' is unreachable ({reason})");
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_status_output(&stdout))
}

pub fn build_gc_command(remote_dir: &str) -> Result<String> {
    let trimmed = remote_dir.trim();
    if trimmed.is_empty() || trimmed == "/" {
        bail!("refusing to run gc on unsafe remote_dir '{remote_dir}'");
    }
    let q_dir = quote_remote_path(trimmed);
    let q_msg = shell_quote(&format!("Cleaned {trimmed}"));
    Ok(format!("rm -rf {q_dir}/* && printf '%s\\n' {q_msg}"))
}

pub fn gc_remote(cfg: &ResolvedConfig) -> Result<()> {
    let target = cfg.ssh_target();
    let cmd_str = build_gc_command(&cfg.remote_dir)?;
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
        assert!(script.contains("export RUSTFLAGS='-C target-cpu=znver4'"));
        assert!(script.contains("devenv shell -- cargo build --release"));
    }

    #[test]
    fn quotes_remote_paths_with_spaces_and_tilde() {
        assert_eq!(quote_remote_path("~"), "\"$HOME\"");
        assert_eq!(quote_remote_path("~/remote-builds"), "\"$HOME\"/remote-builds");
        assert_eq!(
            quote_remote_path("~/my builds/app"),
            "\"$HOME\"/'my builds/app'"
        );
        assert_eq!(quote_remote_path("/tmp/my builds"), "'/tmp/my builds'");

        let cmd = vec!["build".into()];
        let script = build_remote_script(&RemoteJob {
            work_dir: "~/my builds/abc/app",
            ws_dir: "~/my builds/abc/app",
            sync_dir: "~/my builds/abc",
            env_mode: EnvMode::Nix,
            target_cpu: None,
            rustflags_var: "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
            check_target: None,
            cargo_cmd: &cmd,
        });
        assert!(script.contains("cd \"$HOME\"/'my builds/abc/app'"));
        assert!(script.contains("unset CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS"));
        assert!(script.contains("nix develop \"$HOME\"/'my builds/abc/app' -c cargo build"));
    }

    #[test]
    fn parses_cargo_json_stream_executables_and_diagnostics() {
        let input = concat!(
            "plain banner line\n",
            "{\"reason\":\"compiler-artifact\",\"executable\":null}\n",
            "{\"reason\":\"compiler-artifact\",\"executable\":\"/remote/target/release/app\"}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"rendered\":\"error[E0463]: can't find crate for `core`\\n\"}}\n",
        );
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (exes, missing) =
            parse_cargo_json_stream(input.as_bytes(), &mut out, &mut err).unwrap();

        assert_eq!(exes, vec!["/remote/target/release/app"]);
        assert!(missing);
        assert_eq!(String::from_utf8(out).unwrap(), "plain banner line\n");
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("can't find crate for `core`"));
    }

    #[test]
    fn gc_command_validates_and_quotes_remote_dir() {
        assert!(build_gc_command("").is_err());
        assert!(build_gc_command("/").is_err());
        let cmd = build_gc_command("~/my builds").unwrap();
        assert!(cmd.starts_with("rm -rf \"$HOME\"/'my builds'/*"));
    }

    #[test]
    fn builds_and_parses_remote_status() {
        let script = build_status_script("~/remote-builds");
        assert!(script.contains("mkdir -p \"$HOME\"/remote-builds"));
        assert!(script.contains("command -v rsync"));
        assert!(script.contains("command -v cargo"));

        let sample = "\
arch=x86_64
rsync=1
cargo=cargo 1.84.0 (66221abde 2024-11-19)
devenv=1
nix=1
remote_dir_writable=1
";
        let parsed = parse_status_output(sample);
        assert_eq!(
            parsed,
            RemoteStatus {
                arch: "x86_64".into(),
                has_rsync: true,
                cargo_version: Some("cargo 1.84.0 (66221abde 2024-11-19)".into()),
                has_devenv: true,
                has_nix: true,
                remote_dir_writable: true,
            }
        );
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
