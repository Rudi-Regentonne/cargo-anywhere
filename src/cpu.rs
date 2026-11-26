use std::fs;
use std::process::Command;

/// Resolves the `-C target-cpu=<cpu>` value to inject on the remote build host.
/// Returns `None` when disabled (`none`/`off`) or when auto-detection cannot determine the CPU.
pub fn resolve_target_cpu(setting: &str) -> Option<String> {
    let trimmed = setting.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("off")
    {
        return None;
    }

    if !trimmed.eq_ignore_ascii_case("auto") {
        return Some(trimmed.to_string());
    }

    detect_via_proc_cpuinfo().or_else(detect_via_gcc)
}

pub fn normalize_arch(arch: &str) -> &str {
    match arch.trim() {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        other => other,
    }
}

pub fn client_target_triple() -> Option<String> {
    let out = Command::new("rustc").arg("-vV").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_rustc_host(&stdout)
}

pub fn parse_rustc_host(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("host:") {
            let triple = rest.trim();
            if !triple.is_empty() {
                return Some(triple.to_string());
            }
        }
    }
    None
}

pub fn target_rustflags_env_var(triple: &str) -> String {
    let sanitized: String = triple
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("CARGO_TARGET_{sanitized}_RUSTFLAGS")
}

fn detect_via_gcc() -> Option<String> {
    let out = Command::new("gcc")
        .args(["-march=native", "-Q", "--help=target"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_gcc_march(&stdout)
}

fn parse_gcc_march(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("-march=") {
            let cpu = rest.trim();
            if !cpu.is_empty() && cpu != "native" && cpu != "x86-64" && cpu != "generic" {
                return Some(cpu.to_string());
            }
        }
    }
    None
}

fn detect_via_proc_cpuinfo() -> Option<String> {
    let raw = fs::read_to_string("/proc/cpuinfo").ok()?;
    parse_proc_cpuinfo(&raw)
}

fn parse_proc_cpuinfo(raw: &str) -> Option<String> {
    let mut vendor = None;
    let mut family = None;
    let mut model = None;

    for line in raw.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            match k {
                "vendor_id" if vendor.is_none() => vendor = Some(v.to_string()),
                "cpu family" if family.is_none() => family = v.parse::<u32>().ok(),
                "model" if model.is_none() => model = v.parse::<u32>().ok(),
                _ => {}
            }
        }
        if vendor.is_some() && family.is_some() && model.is_some() {
            break;
        }
    }

    match (vendor.as_deref(), family, model) {
        (Some("AuthenticAMD"), Some(26), _) => Some("znver5".to_string()),
        (Some("AuthenticAMD"), Some(25), Some(m)) => {
            // Zen 3 vs Zen 4 family 25 models
            if m >= 0x60 || (0x10..=0x1f).contains(&m) {
                Some("znver4".to_string())
            } else {
                Some("znver3".to_string())
            }
        }
        (Some("AuthenticAMD"), Some(23), Some(m)) => {
            if m >= 0x30 {
                Some("znver2".to_string())
            } else {
                Some("znver1".to_string())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gcc_march_line() {
        let sample =
            "  -march=                     \t\tznver4\n  -mtune=                     \t\tznver4\n";
        assert_eq!(parse_gcc_march(sample).as_deref(), Some("znver4"));
    }

    #[test]
    fn none_disables_target_cpu() {
        assert_eq!(resolve_target_cpu("none"), None);
        assert_eq!(resolve_target_cpu("OFF"), None);
        assert_eq!(resolve_target_cpu(" znver4 ").as_deref(), Some("znver4"));
    }

    #[test]
    fn ignores_generic_gcc_march() {
        assert_eq!(parse_gcc_march("  -march= x86-64\n"), None);
        assert_eq!(parse_gcc_march("  -march= generic\n"), None);
        assert_eq!(parse_gcc_march("  -march= native\n"), None);
    }

    #[test]
    fn parses_rustc_host_and_target_env_var() {
        let sample = "rustc 1.84.0\nhost: x86_64-unknown-linux-gnu\nrelease: 1.84.0\n";
        assert_eq!(
            parse_rustc_host(sample).as_deref(),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            target_rustflags_env_var("x86_64-unknown-linux-gnu"),
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS"
        );
        assert_eq!(normalize_arch("amd64"), "x86_64");
        assert_eq!(normalize_arch("arm64"), "aarch64");
    }

    #[test]
    fn detects_amd_zen_generations_from_cpuinfo() {
        let zen5 = "vendor_id\t: AuthenticAMD\ncpu family\t: 26\nmodel\t\t: 1\n";
        let zen4 = "vendor_id\t: AuthenticAMD\ncpu family\t: 25\nmodel\t\t: 97\n";
        let zen3 = "vendor_id\t: AuthenticAMD\ncpu family\t: 25\nmodel\t\t: 33\n";
        let zen2 = "vendor_id\t: AuthenticAMD\ncpu family\t: 23\nmodel\t\t: 113\n";
        let zen1 = "vendor_id\t: AuthenticAMD\ncpu family\t: 23\nmodel\t\t: 1\n";
        let unknown = "vendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 154\n";

        assert_eq!(parse_proc_cpuinfo(zen5).as_deref(), Some("znver5"));
        assert_eq!(parse_proc_cpuinfo(zen4).as_deref(), Some("znver4"));
        assert_eq!(parse_proc_cpuinfo(zen3).as_deref(), Some("znver3"));
        assert_eq!(parse_proc_cpuinfo(zen2).as_deref(), Some("znver2"));
        assert_eq!(parse_proc_cpuinfo(zen1).as_deref(), Some("znver1"));
        assert_eq!(parse_proc_cpuinfo(unknown), None);
    }
}
