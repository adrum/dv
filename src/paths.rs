//! Where dv keeps its state on disk.
//!
//! Everything lives under one root (`DV_HOME`, default `~/.dv`) so that
//! uninstalling dv is `rm -rf` of a single directory:
//!
//! ```text
//! ~/.dv/
//!   versions/<sdk>/      extracted SDK archives, exactly as published
//!   roots/<sdk>/         one install root per SDK: that SDK, every runtime
//!   dotnet/              one install root holding everything (DOTNET_ROOT)
//!   shims/dotnet         real files on PATH
//!   cache/               downloaded SDK archives
//!   config.toml          default version, release index location
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Root of all dv state.
pub fn home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("DV_HOME") {
        let dir = PathBuf::from(dir);
        if dir.as_os_str().is_empty() {
            anyhow::bail!("DV_HOME is set but empty — unset it or point it at a directory");
        }
        return Ok(dir);
    }
    Ok(user_home()?.join(".dv"))
}

/// The user's home directory.
pub fn user_home() -> Result<PathBuf> {
    let home = std::env::var_os(HOME_VARIABLE).with_context(|| {
        format!("{HOME_VARIABLE} is not set, so dv cannot find its state directory — set DV_HOME")
    })?;
    Ok(PathBuf::from(home))
}

/// The variable that names the user's home directory.
///
/// Windows does not set `HOME`. Some shells there invent one, and following
/// it would give dv a different state directory depending on which shell
/// started it.
const HOME_VARIABLE: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

pub fn versions_dir() -> Result<PathBuf> {
    Ok(home()?.join("versions"))
}

pub fn version_dir(version: &str) -> Result<PathBuf> {
    Ok(versions_dir()?.join(version))
}

/// Per-SDK install roots, each exposing exactly one SDK.
pub fn roots_dir() -> Result<PathBuf> {
    Ok(home()?.join("roots"))
}

pub fn root_dir(version: &str) -> Result<PathBuf> {
    Ok(roots_dir()?.join(version))
}

/// The install root holding every SDK and runtime — what `DOTNET_ROOT` names
/// for editors and global tools launched outside dv.
pub fn combined_root() -> Result<PathBuf> {
    Ok(home()?.join("dotnet"))
}

pub fn shims_dir() -> Result<PathBuf> {
    Ok(home()?.join("shims"))
}

pub fn cache_dir() -> Result<PathBuf> {
    Ok(home()?.join("cache"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(home()?.join("config.toml"))
}

/// Record of the artifact a version was installed from, written next to the
/// extracted tree so `dv doctor` can report tampering and `dv install` can
/// detect a republished artifact.
pub fn artifact_record(version: &str) -> Result<PathBuf> {
    Ok(version_dir(version)?.join(".dv-artifact.json"))
}

/// Where `dotnet tool install --global` puts tools.
///
/// The SDK honors `DOTNET_CLI_HOME` over the user's home for this, so dv does
/// too — otherwise it would look for tools somewhere the SDK never put them.
pub fn global_tools_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("DOTNET_CLI_HOME").filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => user_home()?,
    };
    Ok(base.join(".dotnet").join("tools"))
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("could not create {}", path.display()))
}

/// Mark a file runnable.
///
/// Windows has no executable bit — a file runs because of its extension — so
/// there is nothing to set there.
pub fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("could not make {} executable", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
