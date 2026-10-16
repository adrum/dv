//! Where dv keeps its state on disk.
//!
//! Everything lives under one root so that uninstalling dv is `rm -rf` of a
//! single directory. `DV_HOME` names it; otherwise it is `dv` in the XDG data
//! directory (`$XDG_DATA_HOME`, else `~/.local/share`), where fnm, uv and rv
//! keep their toolchains too — or `%LOCALAPPDATA%\dv` on Windows, which has
//! no XDG:
//!
//! ```text
//! ~/.local/share/dv/
//!   versions/<sdk>/      extracted SDK archives, exactly as published
//!   roots/<sdk>/         one install root per SDK: that SDK, every runtime
//!   dotnet/              one install root holding everything (DOTNET_ROOT)
//!   shims/dotnet         real files on PATH
//!   cache/               downloaded SDK archives
//!   cache/metadata/      Microsoft's release metadata, with HTTP validators
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
    data_home().map(|dir| dir.join("dv"))
}

/// Where per-user application data belongs on this platform.
#[cfg(not(windows))]
fn data_home() -> Result<PathBuf> {
    // The spec says a relative XDG_DATA_HOME is invalid and must be ignored.
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        && dir.is_absolute()
    {
        return Ok(dir);
    }
    Ok(user_home()?.join(".local").join("share"))
}

/// Where per-user application data belongs on this platform. Local rather
/// than roaming: SDKs are hundreds of megabytes of machine-specific binaries.
#[cfg(windows)]
fn data_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        && dir.is_absolute()
    {
        return Ok(dir);
    }
    Ok(user_home()?.join("AppData").join("Local"))
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
