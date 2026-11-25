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
        let sample = "  -march=                     \t\tznver4\n  -mtune=                     \t\tznver4\n";
        assert_eq!(parse_gcc_march(sample).as_deref(), Some("znver4"));
    }

    #[test]
    fn none_disables_target_cpu() {
        assert_eq!(resolve_target_cpu("none"), None);
        assert_eq!(resolve_target_cpu("OFF"), None);
        assert_eq!(resolve_target_cpu("znver4").as_deref(), Some("znver4"));
    }
}
