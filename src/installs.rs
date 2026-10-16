//! The local inventory: which SDKs are on this machine, and what they were
//! installed from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::platform;
use crate::version::Version;

/// What `dv install` recorded about the archive an SDK came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub version: String,
    pub platform: String,
    pub file: String,
    pub url: String,
    pub sha512: String,
    /// Unix seconds, for `dv doctor` and for humans reading the file.
    pub installed_at: u64,
    /// sha256 of the files that decide what runs — the host, the resolver,
    /// the SDK entry point — keyed by path relative to the version directory.
    ///
    /// The archive hash above covers the download, which says nothing about
    /// the tree once it is on disk. These are what let `dv doctor` speak about
    /// what is actually installed. Not the whole tree: an SDK is thousands of
    /// files, and hashing them all on every doctor run would make it a
    /// command nobody runs.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// Installed SDKs, oldest first.
///
/// A directory only counts when it holds the host and the SDK it is named
/// for; a half-extracted or hand-made directory is not a usable install and
/// must not be listed as one.
pub fn installed() -> Result<Vec<Version>> {
    let dir = paths::versions_dir()?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", dir.display())),
    };

    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(version) = name.parse::<Version>() else {
            continue;
        };
        if is_complete(&entry.path(), &version) {
            versions.push(version);
        }
    }
    versions.sort();
    Ok(versions)
}

fn is_complete(tree: &Path, version: &Version) -> bool {
    tree.join(platform::exe("dotnet")).is_file()
        && tree
            .join("sdk")
            .join(version.to_string())
            .join("dotnet.dll")
            .is_file()
}

pub fn is_installed(version: &Version) -> Result<bool> {
    Ok(is_complete(
        &paths::version_dir(&version.to_string())?,
        version,
    ))
}

/// The install root dv runs this SDK from.
pub fn root(version: &Version) -> Result<PathBuf> {
    paths::root_dir(&version.to_string())
}

/// Commands an SDK's install root provides: `dotnet`, and `dnx` from 10 on.
pub fn commands(version: &Version) -> Result<Vec<String>> {
    let root = root(version)?;
    let mut names = Vec::new();
    for command in crate::shims::SHIMMED {
        let path = root.join(command_file(command));
        if path.is_file() {
            names.push(command.to_string());
        }
    }
    Ok(names)
}

/// The file a top-level command lives in. `dnx` is a shell script on Unix and
/// a batch file on Windows; `dotnet` is the host executable.
pub fn command_file(command: &str) -> String {
    match command {
        "dnx" if cfg!(windows) => "dnx.cmd".to_string(),
        "dnx" => "dnx".to_string(),
        other => platform::exe(other),
    }
}

/// Hash the files in an installed tree — an SDK's or a runtime's — that
/// decide what runs: the host, hostfxr, and the SDK's entry point if there
/// is one.
pub fn hash_tree(tree: &Path) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    let mut add = |relative: String| -> Result<()> {
        let path = tree.join(&relative);
        if path.is_file() {
            hashes.insert(relative, crate::net::sha256_file(&path)?);
        }
        Ok(())
    };

    add(platform::exe("dotnet"))?;
    if let Ok(entries) = std::fs::read_dir(tree.join("sdk")) {
        for entry in entries.flatten() {
            if let Some(sdk) = entry.file_name().to_str() {
                add(format!("sdk/{sdk}/dotnet.dll"))?;
            }
        }
    }
    // hostfxr is what reads global.json and picks the SDK, so it is the file
    // that most decides what a `dotnet` command does.
    if let Ok(entries) = std::fs::read_dir(tree.join("host").join("fxr")) {
        for entry in entries.flatten() {
            let Some(fxr_version) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            for file in std::fs::read_dir(entry.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Some(name) = file.file_name().to_str() {
                    add(format!("host/fxr/{fxr_version}/{name}"))?;
                }
            }
        }
    }
    Ok(hashes)
}

/// Recorded files whose contents no longer match, and those that have gone.
pub fn changed_files(tree: &Path, record: &ArtifactRecord) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for (relative, expected) in &record.files {
        let full = tree.join(relative);
        match full.is_file() {
            false => changed.push(format!("{relative} (missing)")),
            true if &crate::net::sha256_file(&full)? != expected => changed.push(relative.clone()),
            true => {}
        }
    }
    Ok(changed)
}

/// The record written next to an installed tree.
pub fn record_path(tree: &Path) -> PathBuf {
    tree.join(".dv-artifact.json")
}

pub fn read_record(tree: &Path) -> Result<Option<ArtifactRecord>> {
    let path = record_path(tree);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(serde_json::from_str(&raw).ok()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("could not read {}", path.display())),
    }
}

pub fn write_record(tree: &Path, record: &ArtifactRecord) -> Result<()> {
    let path = record_path(tree);
    let body = serde_json::to_string_pretty(record)?;
    std::fs::write(&path, format!("{body}\n"))
        .with_context(|| format!("could not write {}", path.display()))
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn remove(version: &Version) -> Result<()> {
    let dir = paths::version_dir(&version.to_string())?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))
}
