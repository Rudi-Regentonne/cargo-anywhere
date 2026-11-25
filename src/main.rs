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

use cli::{Action, CopyMode};

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
        Action::Gc => {
            let cwd = env::current_dir()?;
            let ws_root = workspace::discover_layout(&cwd)
                .ok()
                .map(|l| l.workspace_root);
            let cfg = config::load_config(ws_root.as_deref(), &parsed.overrides)?;
            sync::check_reachable(&cfg)?;
            remote::gc_remote(&cfg)?;
            Ok(0)
        }
        Action::Cargo {
            subcommand,
            cargo_args,
            app_args,
        } => {
            let cwd = env::current_dir()?;
            let layout = workspace::discover_layout(&cwd)?;
            let cfg = config::load_config(Some(&layout.workspace_root), &parsed.overrides)?;

            let remote_sync_dir = format!(
                "{}/{}",
                cfg.remote_dir.trim_end_matches('/'),
                layout.project_hash
            );
            let remote_work_dir = join_remote(&remote_sync_dir, &layout.rel_cwd);
            let remote_ws_dir = join_remote(&remote_sync_dir, &layout.rel_workspace);

            let mut cargo_args = cargo_args;
            let remote_arch = sync::check_reachable(&cfg)?;

            let explicit_target = extract_cargo_target(&cargo_args);
            let auto_cross_target = if explicit_target.is_none()
                && cpu::normalize_arch(&remote_arch) != cpu::normalize_arch(env::consts::ARCH)
            {
                cpu::client_target_triple()
            } else {
                None
            };

            if let Some(ref triple) = auto_cross_target {
                cargo_args.push("--target".to_string());
                cargo_args.push(triple.clone());
            }

            let active_target = explicit_target.as_deref().or(auto_cross_target.as_deref());
            let target_cpu =
                if explicit_target.is_some() && cfg.target_cpu.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    cpu::resolve_target_cpu(&cfg.target_cpu)
                };
            let rustflags_var = active_target
                .map(cpu::target_rustflags_env_var)
                .unwrap_or_else(|| "RUSTFLAGS".to_string());

            sync::upload_sources(&cfg, &remote_sync_dir, &layout.sync_root)?;

            // Determine copy-back behavior
            let is_local_run = subcommand == "run" && !cfg.remote_run;
            let copy_mode = cfg.copy.unwrap_or(if is_local_run {
                CopyMode::Bin
            } else {
                CopyMode::None
            });

            if is_local_run || copy_mode == CopyMode::Bin {
                let mut build_cmd = vec![if is_local_run {
                    "build".to_string()
                } else {
                    subcommand.clone()
                }];
                build_cmd.extend(cargo_args.iter().cloned());
                if !build_cmd.iter().any(|a| a.starts_with("--message-format")) {
                    build_cmd.push("--message-format=json-render-diagnostics".to_string());
                }

                let script = remote::build_remote_script(&remote::RemoteJob {
                    work_dir: &remote_work_dir,
                    ws_dir: &remote_ws_dir,
                    sync_dir: &remote_sync_dir,
                    env_mode: cfg.env,
                    target_cpu: target_cpu.as_deref(),
                    rustflags_var: &rustflags_var,
                    check_target: active_target,
                    cargo_cmd: &build_cmd,
                });
                let out = remote::run_capturing_executables(&cfg, &script)?;
                if !out.success {
                    if out.missing_target {
                        if let Some(triple) = active_target {
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
                    &cfg,
                    &format!("{remote_ws_dir}/Cargo.lock"),
                    &layout.workspace_root.join("Cargo.lock"),
                );

                let mut local_bins = Vec::new();
                for remote_exe in &out.executables {
                    if let Some(rel_after_target) = remote_exe.split("/target/").nth(1) {
                        let local_exe = layout.workspace_root.join("target").join(rel_after_target);
                        sync::download_file(&cfg, remote_exe, &local_exe)?;
                        local_bins.push(local_exe);
                    }
                }

                if is_local_run {
                    let bin = local_bins.last().ok_or_else(|| {
                        anyhow::anyhow!("no executable produced by remote build; pass --bin <name> if the package has multiple binaries")
                    })?;
                    let status = Command::new(bin)
                        .args(&app_args)
                        .status()
                        .with_context(|| format!("executing {}", bin.display()))?;
                    return Ok(status.code().unwrap_or(1));
                }

                Ok(0)
            } else {
                let mut full_cmd = vec![subcommand];
                full_cmd.extend(cargo_args);
                if !app_args.is_empty() {
                    full_cmd.push("--".to_string());
                    full_cmd.extend(app_args);
                }

                let script = remote::build_remote_script(&remote::RemoteJob {
                    work_dir: &remote_work_dir,
                    ws_dir: &remote_ws_dir,
                    sync_dir: &remote_sync_dir,
                    env_mode: cfg.env,
                    target_cpu: target_cpu.as_deref(),
                    rustflags_var: &rustflags_var,
                    check_target: active_target,
                    cargo_cmd: &full_cmd,
                });
                let code = remote::run_streaming(&cfg, &script)?;
                if code == 0 && copy_mode == CopyMode::Target {
                    sync::download_target_dir(&cfg, &remote_ws_dir, &layout.workspace_root)?;
                }
                Ok(code)
            }
        }
    }
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

fn join_remote(base: &str, rel: &Path) -> String {
    if rel.as_os_str().is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), rel.to_string_lossy())
    }
}
