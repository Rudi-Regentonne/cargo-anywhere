use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvMode {
    None,
    Devenv,
    Nix,
    Auto,
}

impl EnvMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Ok(Self::None),
            "devenv" => Ok(Self::Devenv),
            "nix" => Ok(Self::Nix),
            "auto" => Ok(Self::Auto),
            other => bail!("invalid env mode '{other}' (expected: none, devenv, nix, auto)"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    None,
    Bin,
    Target,
}

impl CopyMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Ok(Self::None),
            "bin" | "binary" => Ok(Self::Bin),
            "target" => Ok(Self::Target),
            other => bail!("invalid copy mode '{other}' (expected: none, bin, target)"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    pub host: Option<String>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
    pub remote_dir: Option<String>,
    pub env: Option<EnvMode>,
    pub target_cpu: Option<String>,
    pub copy: Option<CopyMode>,
    pub remote_run: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Help,
    Version,
    Gc,
    Cargo {
        subcommand: String,
        /// Flags/args passed to cargo (before `--`).
        cargo_args: Vec<String>,
        /// Application args passed after `--`.
        app_args: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCli {
    pub overrides: CliOverrides,
    pub action: Action,
}

pub fn print_help() {
    println!(
        "\
cargo-anywhere {}
Run Cargo commands on a remote host with automatic client CPU optimization.

USAGE:
    cargo anywhere [OPTIONS] <CARGO_SUBCOMMAND> [CARGO_ARGS...] [-- APP_ARGS...]
    cargo anywhere [OPTIONS] gc

OPTIONS:
    --host <HOST>            Remote SSH host (or SSH config alias)
    --user <USER>            SSH username (overrides SSH config / host user)
    --identity-file <PATH>   SSH private key file (adds -i <PATH> -o IdentitiesOnly=yes)
    --remote-dir <DIR>       Remote base directory (default: ~/remote-builds)
    --env <MODE>             Remote env wrapper: none | devenv | nix | auto (default: none)
    --target-cpu <CPU>       Target CPU for RUSTFLAGS: auto | <cpu> | none (default: auto)
    --no-target-cpu          Disable -C target-cpu injection
    --copy-bin               Copy back compiled executables + Cargo.lock
    --copy-target            Copy back full target/ directory
    --no-copy                Do not copy artifacts back
    --remote-run             For `run`, execute on the remote host instead of locally
    --local-run              For `run`, execute locally after building on the remote (default)
    -h, --help               Print help
    -V, --version            Print version
",
        env!("CARGO_PKG_VERSION")
    );
}

pub fn parse_args<I>(args: I) -> Result<ParsedCli>
where
    I: IntoIterator<Item = String>,
{
    let mut raw: Vec<String> = args.into_iter().collect();
    if raw.first().map(|s| s.as_str()) == Some("anywhere") {
        raw.remove(0);
    }

    let mut overrides = CliOverrides::default();
    let mut positional: Vec<String> = Vec::new();
    let mut app_args: Vec<String> = Vec::new();
    let mut after_double_dash = false;
    let mut i = 0;

    while i < raw.len() {
        let arg = &raw[i];

        if after_double_dash {
            app_args.push(arg.clone());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }

        // Only treat `-h`/`--help` and `-V`/`--version` as tool flags before a subcommand.
        if positional.is_empty() && (arg == "-h" || arg == "--help") {
            return Ok(ParsedCli {
                overrides,
                action: Action::Help,
            });
        }
        if positional.is_empty() && (arg == "-V" || arg == "--version") {
            return Ok(ParsedCli {
                overrides,
                action: Action::Version,
            });
        }

        if let Some(val) = take_opt_value(&raw, &mut i, "--host")? {
            overrides.host = Some(val);
            continue;
        }
        if let Some(val) = take_opt_value(&raw, &mut i, "--user")? {
            overrides.user = Some(val);
            continue;
        }
        if let Some(val) = take_opt_value(&raw, &mut i, "--identity-file")? {
            overrides.identity_file = Some(val);
            continue;
        }
        if let Some(val) = take_opt_value(&raw, &mut i, "--remote-dir")? {
            overrides.remote_dir = Some(val);
            continue;
        }
        if let Some(val) = take_opt_value(&raw, &mut i, "--env")? {
            overrides.env = Some(EnvMode::parse(&val)?);
            continue;
        }
        if let Some(val) = take_opt_value(&raw, &mut i, "--target-cpu")? {
            overrides.target_cpu = Some(val);
            continue;
        }

        match arg.as_str() {
            "--no-target-cpu" => {
                overrides.target_cpu = Some("none".to_string());
                i += 1;
            }
            "--copy-bin" => {
                overrides.copy = Some(CopyMode::Bin);
                i += 1;
            }
            "--copy-target" => {
                overrides.copy = Some(CopyMode::Target);
                i += 1;
            }
            "--no-copy" => {
                overrides.copy = Some(CopyMode::None);
                i += 1;
            }
            "--remote-run" => {
                overrides.remote_run = Some(true);
                i += 1;
            }
            "--local-run" => {
                overrides.remote_run = Some(false);
                i += 1;
            }
            _ => {
                positional.push(arg.clone());
                i += 1;
            }
        }
    }

    if positional.is_empty() {
        return Ok(ParsedCli {
            overrides,
            action: Action::Help,
        });
    }

    let subcommand = positional.remove(0);
    if subcommand == "gc" || subcommand == "clean" {
        return Ok(ParsedCli {
            overrides,
            action: Action::Gc,
        });
    }

    Ok(ParsedCli {
        overrides,
        action: Action::Cargo {
            subcommand,
            cargo_args: positional,
            app_args,
        },
    })
}

fn take_opt_value(raw: &[String], idx: &mut usize, flag: &str) -> Result<Option<String>> {
    let cur = &raw[*idx];
    if cur == flag {
        let next = raw
            .get(*idx + 1)
            .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))?;
        *idx += 2;
        return Ok(Some(next.clone()));
    }
    if let Some(rest) = cur.strip_prefix(&format!("{flag}=")) {
        *idx += 1;
        return Ok(Some(rest.to_string()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn splits_cargo_and_app_args() {
        let parsed = parse_args(s(&[
            "anywhere",
            "--host",
            "heinz",
            "run",
            "--release",
            "--features",
            "gpu",
            "--",
            "--port",
            "8080",
        ]))
        .unwrap();

        assert_eq!(parsed.overrides.host.as_deref(), Some("heinz"));
        assert_eq!(
            parsed.action,
            Action::Cargo {
                subcommand: "run".into(),
                cargo_args: s(&["--release", "--features", "gpu"]),
                app_args: s(&["--port", "8080"]),
            }
        );
    }

    #[test]
    fn forwards_help_after_subcommand() {
        let parsed = parse_args(s(&["build", "--help"])).unwrap();
        assert_eq!(
            parsed.action,
            Action::Cargo {
                subcommand: "build".into(),
                cargo_args: s(&["--help"]),
                app_args: vec![],
            }
        );
    }

    #[test]
    fn parses_gc_with_remote_dir() {
        let parsed = parse_args(s(&["--remote-dir=/tmp/builds", "gc"])).unwrap();
        assert_eq!(parsed.overrides.remote_dir.as_deref(), Some("/tmp/builds"));
        assert_eq!(parsed.action, Action::Gc);
    }
}
