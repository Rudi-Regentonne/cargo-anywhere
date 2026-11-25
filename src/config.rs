use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::cli::{CliOverrides, CopyMode, EnvMode};

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub host: String,
    pub user: Option<String>,
    pub identity_file: Option<String>,
    pub remote_dir: String,
    pub env: EnvMode,
    pub target_cpu: String,
    pub copy: Option<CopyMode>,
    pub remote_run: bool,
}

impl ResolvedConfig {
    pub fn ssh_target(&self) -> String {
        match &self.user {
            Some(u) if !self.host.contains('@') => format!("{u}@{}", self.host),
            _ => self.host.clone(),
        }
    }

    pub fn ssh_identity_args(&self) -> Vec<String> {
        match &self.identity_file {
            Some(key) => {
                let expanded = expand_tilde(key);
                vec![
                    "-i".to_string(),
                    expanded,
                    "-o".to_string(),
                    "IdentitiesOnly=yes".to_string(),
                ]
            }
            None => Vec::new(),
        }
    }

    pub fn rsync_rsh(&self) -> Option<String> {
        let key = self.identity_file.as_ref()?;
        let expanded = expand_tilde(key);
        let quoted = if expanded.contains([' ', '\'', '"']) {
            format!("'{}'", expanded.replace('\'', "'\\''"))
        } else {
            expanded
        };
        Some(format!("ssh -i {quoted} -o IdentitiesOnly=yes"))
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PartialConfig {
    host: Option<String>,
    user: Option<String>,
    identity_file: Option<String>,
    remote_dir: Option<String>,
    env: Option<String>,
    target_cpu: Option<String>,
    copy: Option<String>,
    remote_run: Option<bool>,
}

impl PartialConfig {
    fn from_file(path: &Path) -> Result<Option<Self>> {
        if !path.is_file() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let parsed: Self = toml::from_str(&raw)
            .with_context(|| format!("parsing TOML in {}", path.display()))?;
        Ok(Some(parsed))
    }

    fn apply_into(&self, dst: &mut ConfigBuilder) -> Result<()> {
        if let Some(v) = &self.host {
            dst.host = Some(v.clone());
        }
        if let Some(v) = &self.user {
            dst.user = Some(v.clone());
        }
        if let Some(v) = &self.identity_file {
            dst.identity_file = Some(v.clone());
        }
        if let Some(v) = &self.remote_dir {
            dst.remote_dir = v.clone();
        }
        if let Some(v) = &self.env {
            dst.env = EnvMode::parse(v)?;
        }
        if let Some(v) = &self.target_cpu {
            dst.target_cpu = v.clone();
        }
        if let Some(v) = &self.copy {
            dst.copy = Some(CopyMode::parse(v)?);
        }
        if let Some(v) = self.remote_run {
            dst.remote_run = v;
        }
        Ok(())
    }
}

struct ConfigBuilder {
    host: Option<String>,
    user: Option<String>,
    identity_file: Option<String>,
    remote_dir: String,
    env: EnvMode,
    target_cpu: String,
    copy: Option<CopyMode>,
    remote_run: bool,
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self {
            host: None,
            user: None,
            identity_file: None,
            remote_dir: "~/remote-builds".to_string(),
            env: EnvMode::None,
            target_cpu: "auto".to_string(),
            copy: None,
            remote_run: false,
        }
    }
}

pub fn global_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("cargo-anywhere/config.toml"));
        }
    }
    env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/cargo-anywhere/config.toml"))
}

pub fn load_config(workspace_root: Option<&Path>, cli: &CliOverrides) -> Result<ResolvedConfig> {
    let mut cfg = ConfigBuilder::default();

    // 1. Global config
    if let Some(global_path) = global_config_path() {
        if let Some(partial) = PartialConfig::from_file(&global_path)? {
            partial.apply_into(&mut cfg)?;
        }
    }

    // 2. Workspace config (.cargo-anywhere.toml -> .cargo-anywhere.local.toml)
    if let Some(root) = workspace_root {
        for name in [".cargo-anywhere.toml", ".cargo-anywhere.local.toml"] {
            if let Some(partial) = PartialConfig::from_file(&root.join(name))? {
                partial.apply_into(&mut cfg)?;
            }
        }
    }

    // 3. Environment variables
    if let Ok(v) = env::var("CARGO_ANYWHERE_HOST") {
        if !v.is_empty() {
            cfg.host = Some(v);
        }
    }
    if let Ok(v) = env::var("CARGO_ANYWHERE_USER") {
        if !v.is_empty() {
            cfg.user = Some(v);
        }
    }
    if let Ok(v) = env::var("CARGO_ANYWHERE_IDENTITY_FILE") {
        if !v.is_empty() {
            cfg.identity_file = Some(v);
        }
    }
    if let Ok(v) = env::var("CARGO_ANYWHERE_REMOTE_DIR") {
        if !v.is_empty() {
            cfg.remote_dir = v;
        }
    }
    if let Ok(v) = env::var("CARGO_ANYWHERE_ENV") {
        if !v.is_empty() {
            cfg.env = EnvMode::parse(&v)?;
        }
    }
    if let Ok(v) = env::var("CARGO_ANYWHERE_TARGET_CPU") {
        if !v.is_empty() {
            cfg.target_cpu = v;
        }
    }
    if let Ok(v) = env::var("CARGO_ANYWHERE_COPY") {
        if !v.is_empty() {
            cfg.copy = Some(CopyMode::parse(&v)?);
        }
    }
    if let Ok(v) = env::var("CARGO_ANYWHERE_REMOTE_RUN") {
        if matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes") {
            cfg.remote_run = true;
        } else if matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no") {
            cfg.remote_run = false;
        }
    }

    // 4. CLI flags (highest priority)
    if let Some(v) = &cli.host {
        cfg.host = Some(v.clone());
    }
    if let Some(v) = &cli.user {
        cfg.user = Some(v.clone());
    }
    if let Some(v) = &cli.identity_file {
        cfg.identity_file = Some(v.clone());
    }
    if let Some(v) = &cli.remote_dir {
        cfg.remote_dir = v.clone();
    }
    if let Some(v) = cli.env {
        cfg.env = v;
    }
    if let Some(v) = &cli.target_cpu {
        cfg.target_cpu = v.clone();
    }
    if let Some(v) = cli.copy {
        cfg.copy = Some(v);
    }
    if let Some(v) = cli.remote_run {
        cfg.remote_run = v;
    }

    let host = cfg.host.ok_or_else(|| {
        anyhow::anyhow!(
            "no remote host configured; pass --host <HOST>, set CARGO_ANYWHERE_HOST, or set `host = \"...\"` in ~/.config/cargo-anywhere/config.toml"
        )
    })?;

    Ok(ResolvedConfig {
        host,
        user: cfg.user,
        identity_file: cfg.identity_file,
        remote_dir: cfg.remote_dir,
        env: cfg.env,
        target_cpu: cfg.target_cpu,
        copy: cfg.copy,
        remote_run: cfg.remote_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_workspace_local_overrides_workspace() {
        let dir = env::temp_dir().join(format!("cargo-anywhere-cfg-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(
            dir.join(".cargo-anywhere.toml"),
            "host = \"server-a\"\nremote_dir = \"~/builds-a\"\n",
        )
        .unwrap();
        fs::write(
            dir.join(".cargo-anywhere.local.toml"),
            "host = \"server-b\"\ntarget_cpu = \"znver4\"\n",
        )
        .unwrap();

        let cli = CliOverrides {
            remote_dir: Some("/tmp/cli".into()),
            ..Default::default()
        };
        let resolved = load_config(Some(&dir), &cli).unwrap();
        assert_eq!(resolved.host, "server-b");
        assert_eq!(resolved.remote_dir, "/tmp/cli");
        assert_eq!(resolved.target_cpu, "znver4");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ssh_target_and_identity_formatting() {
        let cfg = ResolvedConfig {
            host: "heinz".into(),
            user: Some("builder".into()),
            identity_file: Some("/tmp/my key".into()),
            remote_dir: "~/remote-builds".into(),
            env: EnvMode::None,
            target_cpu: "auto".into(),
            copy: None,
            remote_run: false,
        };
        assert_eq!(cfg.ssh_target(), "builder@heinz");
        assert_eq!(
            cfg.ssh_identity_args(),
            vec!["-i", "/tmp/my key", "-o", "IdentitiesOnly=yes"]
        );
        assert_eq!(
            cfg.rsync_rsh().as_deref(),
            Some("ssh -i '/tmp/my key' -o IdentitiesOnly=yes")
        );

        let with_at = ResolvedConfig {
            host: "qajus@heinz".into(),
            ..cfg
        };
        assert_eq!(with_at.ssh_target(), "qajus@heinz");
    }

    #[test]
    fn errors_when_host_missing() {
        let dir = env::temp_dir().join(format!("cargo-anywhere-nohost-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let res = load_config(Some(&dir), &CliOverrides::default());
        assert!(res.is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
