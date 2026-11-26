mod cli;
mod config;
mod cpu;
mod remote;
mod sync;
mod workspace;

use std::env;
use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{bail, Context, Result};

use cli::{Action, CliOverrides, CopyMode, EnvMode};
use config::ResolvedConfig;
use workspace::ProjectLayout;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("cargo-anywhere error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<i32> {
    let parsed = cli::parse_args(env::args().skip(1))?;

    match parsed.action {
        Action::Help => {
            cli::print_help();
            Ok(0)
        }
        Action::Version => {
            println!("cargo-anywhere {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Action::Status => run_status(&parsed.overrides),
        Action::Gc => run_gc(&parsed.overrides),
        Action::Cargo {
            subcommand,
            cargo_args,
            app_args,
        } => run_cargo(&parsed.overrides, subcommand, cargo_args, app_args),
    }
}

fn load_optional_workspace_config(overrides: &CliOverrides) -> Result<ResolvedConfig> {
    let cwd = env::current_dir()?;
    let ws_root = workspace::discover_layout(&cwd)
        .ok()
        .map(|l| l.workspace_root);
    config::load_config(ws_root.as_deref(), overrides)
}

fn run_status(overrides: &CliOverrides) -> Result<i32> {
    let cfg = load_optional_workspace_config(overrides)?;
    let local_cpu = cpu::resolve_target_cpu(&cfg.target_cpu);

    println!("target:     {}", cfg.ssh_target());
    println!(
        "identity:   {}",
        cfg.identity_file.as_deref().unwrap_or("(default)")
    );
    println!("remote_dir: {}", cfg.remote_dir);
    println!("env:        {:?}", cfg.env);
    println!("target_cpu: {}", local_cpu.as_deref().unwrap_or("(none)"));

    let status = remote::check_remote_status(&cfg)?;
    let arch = if status.arch.is_empty() {
        "unknown arch"
    } else {
        &status.arch
    };
    println!("ssh:        ok ({arch})");
    println!("rsync:      {}", ok_or_missing(status.has_rsync));
    println!(
        "cargo:      {}",
        status.cargo_version.as_deref().unwrap_or("missing")
    );
    println!("devenv:     {}", ok_or_missing(status.has_devenv));
    println!("nix:        {}", ok_or_missing(status.has_nix));
    println!(
        "remote_dir: {}",
        if status.remote_dir_writable {
            "writable"
        } else {
            "not writable"
        }
    );

    let env_ok = match cfg.env {
        EnvMode::None | EnvMode::Auto => true,
        EnvMode::Devenv => status.has_devenv,
        EnvMode::Nix => status.has_nix,
    };
    let all_ok =
        status.has_rsync && status.cargo_version.is_some() && status.remote_dir_writable && env_ok;
    Ok(if all_ok { 0 } else { 1 })
}

fn ok_or_missing(present: bool) -> &'static str {
    if present {
        "ok"
    } else {
        "missing"
    }
}

fn run_gc(overrides: &CliOverrides) -> Result<i32> {
    let cfg = load_optional_workspace_config(overrides)?;
    sync::check_reachable(&cfg)?;
    remote::gc_remote(&cfg)?;
    Ok(0)
}

struct RemotePaths {
    sync_dir: String,
    work_dir: String,
    ws_dir: String,
}

impl RemotePaths {
    fn from_layout(cfg: &ResolvedConfig, layout: &ProjectLayout) -> Self {
        let sync_dir = format!(
            "{}/{}",
            cfg.remote_dir.trim_end_matches('/'),
            layout.project_hash
        );
        Self {
            work_dir: join_remote(&sync_dir, &layout.rel_cwd),
            ws_dir: join_remote(&sync_dir, &layout.rel_workspace),
            sync_dir,
        }
    }
}

struct TargetSettings {
    active_target: Option<String>,
    target_cpu: Option<String>,
    rustflags_var: String,
}

fn is_remote_execution(subcommand: &str, cfg: &ResolvedConfig, cargo_args: &[String]) -> bool {
    if subcommand == "run" {
        return cfg.remote_run;
    }
    matches!(subcommand, "test" | "bench") && !cargo_args.iter().any(|a| a == "--no-run")
}

fn resolve_target_settings(
    cfg: &ResolvedConfig,
    remote_arch: &str,
    cargo_args: &mut Vec<String>,
    runs_on_remote: bool,
) -> TargetSettings {
    let explicit_target = extract_cargo_target(cargo_args);
    let had_explicit_target = explicit_target.is_some();
    let auto_cross_target = if !runs_on_remote
        && !had_explicit_target
        && cpu::normalize_arch(remote_arch) != cpu::normalize_arch(env::consts::ARCH)
    {
        cpu::client_target_triple()
    } else {
        None
    };

    if let Some(ref triple) = auto_cross_target {
        cargo_args.push("--target".to_string());
        cargo_args.push(triple.clone());
    }

    let active_target = explicit_target.or(auto_cross_target);
    let skip_auto_cpu =
        cfg.target_cpu.eq_ignore_ascii_case("auto") && (had_explicit_target || runs_on_remote);
    let target_cpu = if skip_auto_cpu {
        None
    } else {
        cpu::resolve_target_cpu(&cfg.target_cpu)
    };
    let rustflags_var = active_target
        .as_deref()
        .map(cpu::target_rustflags_env_var)
        .unwrap_or_else(|| "RUSTFLAGS".to_string());

    TargetSettings {
        active_target,
        target_cpu,
        rustflags_var,
    }
}

fn run_cargo(
    overrides: &CliOverrides,
    subcommand: String,
    mut cargo_args: Vec<String>,
    app_args: Vec<String>,
) -> Result<i32> {
    let cwd = env::current_dir()?;
    let layout = workspace::discover_layout(&cwd)?;
    let cfg = config::load_config(Some(&layout.workspace_root), overrides)?;
    let paths = RemotePaths::from_layout(&cfg, &layout);

    let remote_arch = sync::check_reachable(&cfg)?;
    let runs_on_remote = is_remote_execution(&subcommand, &cfg, &cargo_args);
    let target = resolve_target_settings(&cfg, &remote_arch, &mut cargo_args, runs_on_remote);

    sync::upload_sources(&cfg, &paths.sync_dir, &layout.sync_root)?;

    let is_local_run = subcommand == "run" && !cfg.remote_run;
    let copy_mode = cfg.copy.unwrap_or(if is_local_run {
        CopyMode::Bin
    } else {
        CopyMode::None
    });

    if is_local_run || copy_mode == CopyMode::Bin {
        run_and_copy_bins(
            &cfg,
            &layout,
            &paths,
            &target,
            &subcommand,
            &cargo_args,
            &app_args,
            is_local_run,
        )
    } else {
        run_streaming_cargo(
            &cfg, &layout, &paths, &target, subcommand, cargo_args, app_args, copy_mode,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn run_and_copy_bins(
    cfg: &ResolvedConfig,
    layout: &ProjectLayout,
    paths: &RemotePaths,
    target: &TargetSettings,
    subcommand: &str,
    cargo_args: &[String],
    app_args: &[String],
    is_local_run: bool,
) -> Result<i32> {
    let mut build_cmd = vec![if is_local_run {
        "build".to_string()
    } else {
        subcommand.to_string()
    }];
    build_cmd.extend(cargo_args.iter().cloned());
    if !build_cmd.iter().any(|a| a.starts_with("--message-format")) {
        build_cmd.push("--message-format=json-render-diagnostics".to_string());
    }

    let script = remote::build_remote_script(&remote::RemoteJob {
        work_dir: &paths.work_dir,
        ws_dir: &paths.ws_dir,
        sync_dir: &paths.sync_dir,
        env_mode: cfg.env,
        target_cpu: target.target_cpu.as_deref(),
        rustflags_var: &target.rustflags_var,
        check_target: target.active_target.as_deref(),
        cargo_cmd: &build_cmd,
    });
    let out = remote::run_capturing_executables(cfg, &script)?;
    if !out.success {
        if out.missing_target {
            if let Some(triple) = target.active_target.as_deref() {
                bail!(
                    "target '{triple}' is not installed on remote host '{}' (install it with `rustup target add {triple}` or add it to your Nix/devenv toolchain)",
                    cfg.host
                );
            }
        }
        return Ok(out.exit_code);
    }

    // Sync Cargo.lock back if it exists remotely (best-effort)
    let _ = sync::download_file(
        cfg,
        &format!("{}/Cargo.lock", paths.ws_dir),
        &layout.workspace_root.join("Cargo.lock"),
    );

    let mut local_bins = Vec::new();
    for remote_exe in &out.executables {
        if let Some(rel) = rel_after_target(remote_exe) {
            let local_exe = layout.workspace_root.join("target").join(rel);
            sync::download_file(cfg, remote_exe, &local_exe)?;
            local_bins.push(local_exe);
        }
    }

    if is_local_run {
        let bin = local_bins.last().ok_or_else(|| {
            anyhow::anyhow!("no executable produced by remote build; pass --bin <name> if the package has multiple binaries")
        })?;
        let status = Command::new(bin)
            .args(app_args)
            .status()
            .with_context(|| format!("executing {}", bin.display()))?;
        return Ok(status.code().unwrap_or(1));
    }

    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn run_streaming_cargo(
    cfg: &ResolvedConfig,
    layout: &ProjectLayout,
    paths: &RemotePaths,
    target: &TargetSettings,
    subcommand: String,
    cargo_args: Vec<String>,
    app_args: Vec<String>,
    copy_mode: CopyMode,
) -> Result<i32> {
    let mut full_cmd = vec![subcommand];
    full_cmd.extend(cargo_args);
    if !app_args.is_empty() {
        full_cmd.push("--".to_string());
        full_cmd.extend(app_args);
    }

    let script = remote::build_remote_script(&remote::RemoteJob {
        work_dir: &paths.work_dir,
        ws_dir: &paths.ws_dir,
        sync_dir: &paths.sync_dir,
        env_mode: cfg.env,
        target_cpu: target.target_cpu.as_deref(),
        rustflags_var: &target.rustflags_var,
        check_target: target.active_target.as_deref(),
        cargo_cmd: &full_cmd,
    });
    let code = remote::run_streaming(cfg, &script)?;
    if code == 0 && copy_mode == CopyMode::Target {
        sync::download_target_dir(cfg, &paths.ws_dir, &layout.workspace_root)?;
    }
    Ok(code)
}

fn extract_cargo_target(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--target" {
            return args.get(i + 1).cloned();
        }
        if let Some(val) = arg.strip_prefix("--target=") {
            return Some(val.to_string());
        }
        i += 1;
    }
    None
}

fn rel_after_target(remote_exe: &str) -> Option<&str> {
    remote_exe.rsplit_once("/target/").map(|(_, rel)| rel)
}

fn join_remote(base: &str, rel: &Path) -> String {
    if rel.as_os_str().is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), rel.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_cargo_target_formats() {
        assert_eq!(
            extract_cargo_target(&[
                "--release".into(),
                "--target".into(),
                "aarch64-unknown-linux-gnu".into()
            ])
            .as_deref(),
            Some("aarch64-unknown-linux-gnu")
        );
        assert_eq!(
            extract_cargo_target(&["--target=wasm32-unknown-unknown".into()]).as_deref(),
            Some("wasm32-unknown-unknown")
        );
        assert_eq!(extract_cargo_target(&["--release".into()]), None);
    }

    #[test]
    fn joins_remote_and_splits_last_target_segment() {
        assert_eq!(
            join_remote("~/remote-builds/abc/", Path::new("app/src")),
            "~/remote-builds/abc/app/src"
        );
        assert_eq!(
            join_remote("~/remote-builds/abc", Path::new("")),
            "~/remote-builds/abc"
        );
        assert_eq!(
            rel_after_target("/home/user/target/project/target/release/app"),
            Some("release/app")
        );
    }

    #[test]
    fn skips_client_auto_optimizations_for_remote_execution() {
        let mut cfg = ResolvedConfig {
            host: "build-host".into(),
            user: None,
            identity_file: None,
            remote_dir: "~/remote-builds".into(),
            env: EnvMode::None,
            target_cpu: "auto".into(),
            copy: None,
            remote_run: false,
        };

        assert!(!is_remote_execution("run", &cfg, &[]));
        assert!(!is_remote_execution("build", &cfg, &[]));
        assert!(is_remote_execution("test", &cfg, &[]));
        assert!(is_remote_execution("bench", &cfg, &[]));
        assert!(!is_remote_execution("test", &cfg, &["--no-run".into()]));

        cfg.remote_run = true;
        assert!(is_remote_execution("run", &cfg, &[]));

        // Even if remote arch differs, remote execution should not inject client --target or auto target-cpu.
        let mut args = vec!["--release".to_string()];
        let remote_target =
            resolve_target_settings(&cfg, "totally-different-arch", &mut args, true);
        assert_eq!(remote_target.active_target, None);
        assert_eq!(remote_target.target_cpu, None);
        assert_eq!(args, vec!["--release"]);

        // Explicit target_cpu is still honored if the user asked for it.
        cfg.target_cpu = "native".into();
        let explicit_cpu = resolve_target_settings(&cfg, "x86_64", &mut args, true);
        assert_eq!(explicit_cpu.target_cpu.as_deref(), Some("native"));
    }
}
