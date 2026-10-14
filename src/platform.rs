//! Host platform identification, as a .NET runtime identifier: the `rid`
//! Microsoft's release metadata files every download under.

use anyhow::{Result, bail};

/// The runtime identifier SDK downloads are published for on this machine:
/// `osx-arm64`, `linux-x64`, `linux-musl-arm64`, `win-x64`, …
pub fn current() -> Result<String> {
    if let Ok(forced) = std::env::var("DV_PLATFORM")
        && !forced.trim().is_empty()
    {
        return Ok(forced.trim().to_string());
    }
    let os = match std::env::consts::OS {
        "macos" => "osx",
        "linux" if is_musl() => "linux-musl",
        "linux" => "linux",
        "windows" => "win",
        other => {
            bail!(".NET SDKs are not published for {other} — dv supports macOS, Linux and Windows")
        }
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => bail!(".NET SDKs are not published for {other} processors"),
    };
    Ok(format!("{os}-{arch}"))
}

/// The suffix of `DOTNET_ROOT_<ARCH>`, which an app host prefers over plain
/// `DOTNET_ROOT` when it is set.
pub fn root_variable_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => Some("ARM64"),
        "x86_64" => Some("X64"),
        _ => None,
    }
}

/// Whether this Linux runs on musl rather than glibc.
///
/// The two need different builds, and running a glibc SDK on Alpine fails
/// with a "not found" for a binary that plainly exists. musl's dynamic loader
/// is the one reliable tell.
fn is_musl() -> bool {
    let Ok(entries) = std::fs::read_dir("/lib") else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("ld-musl-"))
    })
}

/// The file name of the executable called `name`: `dotnet.exe` on Windows,
/// `dotnet` everywhere else.
///
/// Every path to a binary goes through this. A lookup that forgets the suffix
/// does not fail on Windows, it just finds nothing.
pub fn exe(name: &str) -> String {
    with_suffix(name, std::env::consts::EXE_SUFFIX)
}

fn with_suffix(name: &str, suffix: &str) -> String {
    if suffix.is_empty() || name.to_ascii_lowercase().ends_with(suffix) {
        return name.to_string();
    }
    format!("{name}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suffix_is_added_once_and_only_where_there_is_one() {
        assert_eq!(with_suffix("dotnet", ""), "dotnet");
        assert_eq!(with_suffix("dotnet", ".exe"), "dotnet.exe");
        assert_eq!(with_suffix("dotnet.exe", ".exe"), "dotnet.exe");
        assert_eq!(with_suffix("DOTNET.EXE", ".exe"), "DOTNET.EXE");
    }

    #[test]
    fn the_host_has_a_rid() {
        let rid = current().unwrap();
        assert!(rid.ends_with("-arm64") || rid.ends_with("-x64"), "{rid}");
    }
}
