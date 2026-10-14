//! Install roots: the directories `dotnet` actually runs from.
//!
//! A .NET install root holds one `dotnet` host beside side-by-side SDKs,
//! runtimes and targeting packs, and the host chooses among them per command.
//! That is right for runtimes — an app built for net8.0 needs the 8.0 runtime
//! even when SDK 10 built it — and awkward for SDKs, because the host's choice
//! only listens to `global.json`. A default, an environment override, or
//! "newest *release*, not the preview" cannot be expressed to it at all.
//!
//! So dv composes roots out of the extracted SDK archives in `versions/`:
//!
//! - `roots/<sdk>/` exposes exactly one SDK, and every runtime, host and pack
//!   from every installed SDK. The shim runs this, so the SDK dv resolved is
//!   the only one the host can pick, while any installed runtime can run what
//!   it builds.
//! - `dotnet/` exposes everything, like a system-wide install. It is what
//!   `DOTNET_ROOT` names for editors and global tools that start outside dv.
//!
//! Roots are built from **hard links**, not symlinks. The SDK locates its
//! install root from its own resolved path — a symlinked `sdk/<version>`
//! leads it back into `versions/`, where only its own packs exist, and it
//! quietly downloads the rest from NuGet and hands launched apps the wrong
//! `DOTNET_ROOT`. Hard links have no "real" path to escape to, cost no disk,
//! and need no special privilege on Windows.
//!
//! Each root records which entries dv put there. Sync only ever adds or
//! removes recorded entries, so anything else written into a root — a
//! workload's packs, say — is left alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::installs;
use crate::paths;
use crate::platform;
use crate::version::Version;

/// The record of what dv placed in a root, keyed by path relative to it.
const RECORD: &str = ".dv-root.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    /// Relative path → the installed SDK it was linked from.
    entries: BTreeMap<String, String>,
}

/// Which roots an entry belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Shared by every root: hosts, runtimes, packs.
    Shared,
    /// Belongs to one SDK: the SDK itself, its templates and workload
    /// manifests. Only its own root, and the combined root, get it.
    PerSdk,
}

/// One linkable unit of an SDK tree.
#[derive(Debug, Clone)]
struct Entry {
    relative: String,
    scope: Scope,
    owner: Version,
}

/// Rebuild every root to match what is installed.
///
/// `refreshed` lists SDKs whose trees were just replaced (a forced reinstall):
/// their entries are relinked even though the paths have not changed, so no
/// root keeps the old files alive.
pub fn sync(refreshed: &[Version]) -> Result<()> {
    let installed = installs::installed()?;
    let _lock = crate::lock::acquire(&paths::home()?, "roots", "updating install roots")?;

    let versions = paths::versions_dir()?;
    let mut entries = Vec::new();
    for version in &installed {
        entries.extend(entries_of(version)?);
    }

    for version in &installed {
        let desired = desired_for(&entries, Some(version));
        sync_root(&versions, &installs::root(version)?, &desired, refreshed)?;
    }
    remove_stale_roots(&installed)?;

    let combined = paths::combined_root()?;
    if installed.is_empty() {
        remove_root(&combined)?;
    } else {
        sync_root(
            &versions,
            &combined,
            &desired_for(&entries, None),
            refreshed,
        )?;
    }
    Ok(())
}

/// What an SDK tree offers, at the granularity roots are composed from.
fn entries_of(version: &Version) -> Result<Vec<Entry>> {
    let tree = paths::version_dir(&version.to_string())?;
    let mut entries = Vec::new();
    let mut add = |relative: String, scope: Scope| {
        entries.push(Entry {
            relative,
            scope,
            owner: version.clone(),
        })
    };

    for name in children(&tree)? {
        let path = tree.join(&name);
        if path.is_file() {
            if name.starts_with('.') {
                continue; // dv's own record, not part of the SDK
            }
            // `dnx` is the SDK's own launcher script; the host and the
            // license texts are interchangeable between SDKs.
            let scope = if name == installs::command_file("dnx") {
                Scope::PerSdk
            } else {
                Scope::Shared
            };
            add(name, scope);
            continue;
        }
        match name.as_str() {
            // <kind>/<name>/<version>: host/fxr/10.0.13,
            // shared/Microsoft.NETCore.App/10.0.13, packs/<pack>/<version>.
            "host" | "shared" | "packs" => {
                for kind in children(&path)? {
                    for leaf in children(&path.join(&kind))? {
                        add(format!("{name}/{kind}/{leaf}"), Scope::Shared);
                    }
                }
            }
            // sdk/10.0.402, templates/10.0.13: one level down.
            "sdk" | "templates" => {
                for leaf in children(&path)? {
                    add(format!("{name}/{leaf}"), Scope::PerSdk);
                }
            }
            // sdk-manifests/<band>/<manifest>: two SDKs in one band share the
            // band directory, so the unit is the manifest inside it.
            "sdk-manifests" => {
                for band in children(&path)? {
                    for leaf in children(&path.join(&band))? {
                        add(format!("{name}/{band}/{leaf}"), Scope::PerSdk);
                    }
                }
            }
            // Anything newer than this code is linked whole, per SDK, which
            // is the conservative reading of a directory dv does not know.
            _ => add(name, Scope::PerSdk),
        }
    }
    Ok(entries)
}

/// The entries a root should hold, each from the newest SDK that has it.
///
/// `only` is the SDK a per-SDK root is for; `None` is the combined root.
fn desired_for(entries: &[Entry], only: Option<&Version>) -> BTreeMap<String, Version> {
    let mut desired: BTreeMap<String, Version> = BTreeMap::new();
    for entry in entries {
        if let Some(only) = only
            && entry.scope == Scope::PerSdk
            && &entry.owner != only
        {
            continue;
        }
        // Prefer the newest SDK's copy: its host understands every older
        // SDK, and the reverse is not true.
        let keep = desired
            .get(&entry.relative)
            .is_none_or(|current| entry.owner > *current);
        if keep {
            desired.insert(entry.relative.clone(), entry.owner.clone());
        }
    }
    desired
}

fn sync_root(
    versions: &Path,
    root: &Path,
    desired: &BTreeMap<String, Version>,
    refreshed: &[Version],
) -> Result<()> {
    paths::ensure_dir(root)?;
    let mut record = read_record(root)?;

    // Remove what is no longer wanted, comes from a different SDK now, or
    // comes from an SDK whose tree was just replaced.
    let recorded: Vec<(String, String)> = record
        .entries
        .iter()
        .map(|(relative, owner)| (relative.clone(), owner.clone()))
        .collect();
    for (relative, owner) in recorded {
        let keep = desired.get(&relative).is_some_and(|wanted| {
            wanted.to_string() == owner && !refreshed.iter().any(|r| r == wanted)
        });
        if !keep {
            unlink_entry(
                &versions.join(&owner).join(&relative),
                &root.join(&relative),
            )?;
            prune_empty_parents(root, &relative);
            record.entries.remove(&relative);
            write_record(root, &record)?;
        }
    }

    for (relative, owner) in desired {
        let target = root.join(relative);
        if record.entries.contains_key(relative) && target.exists() {
            continue;
        }
        let source = versions.join(owner.to_string()).join(relative);
        if target.is_dir() && source.is_dir() {
            // Something is already here: what is left of an entry after its
            // own files were removed — a workload's manifests, say — or an
            // interrupted sync. Merge into it rather than replacing it.
            link_dir(&source, &target)?;
        } else {
            remove_path(&target)?;
            link_tree(&source, &target)?;
        }
        record.entries.insert(relative.clone(), owner.to_string());
        write_record(root, &record)?;
    }
    Ok(())
}

/// Hard-link `source` into place at `target`, file by file.
///
/// Built beside the target and renamed in, so a root never shows a
/// half-linked SDK to a `dotnet` that starts mid-sync.
fn link_tree(source: &Path, target: &Path) -> Result<()> {
    let name = target
        .file_name()
        .context("an install root entry has no name")?
        .to_string_lossy()
        .to_string();
    let staging = target.with_file_name(format!(".{name}.dv-incoming"));
    remove_path(&staging)?;
    if let Some(parent) = staging.parent() {
        paths::ensure_dir(parent)?;
    }

    if source.is_dir() {
        link_dir(source, &staging)?;
    } else {
        link_file(source, &staging)?;
    }
    std::fs::rename(&staging, target).with_context(|| {
        format!(
            "could not move {} into place at {}",
            staging.display(),
            target.display()
        )
    })
}

fn link_dir(source: &Path, target: &Path) -> Result<()> {
    paths::ensure_dir(target)?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("could not read {}", source.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_dir(&from, &to)?;
        } else {
            link_file(&from, &to)?;
        }
    }
    Ok(())
}

fn link_file(source: &Path, target: &Path) -> Result<()> {
    if std::fs::symlink_metadata(target).is_ok() {
        remove_path(target)?;
    }
    if std::fs::hard_link(source, target).is_ok() {
        return Ok(());
    }
    // A filesystem that refuses hard links still gets a working root, at the
    // cost of the disk a copy takes.
    std::fs::copy(source, target)
        .map(|_| ())
        .with_context(|| format!("could not link or copy {}", source.display()))
}

/// Remove an entry dv placed, and only what dv placed in it.
///
/// The SDK writes into its install root: `dotnet workload update` adds new
/// manifest versions inside `sdk-manifests/<band>/<manifest>/`, a directory
/// dv linked. Removing the entry wholesale would take those with it, so only
/// the paths the SDK itself shipped — `shipped` — are removed, and the
/// directories they leave empty. When the SDK that shipped them is gone, so
/// is any way to tell, and the entry goes whole.
fn unlink_entry(shipped: &Path, target: &Path) -> Result<()> {
    if !shipped.is_dir() {
        return remove_path(target);
    }
    let Ok(entries) = std::fs::read_dir(shipped) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        unlink_entry(&entry.path(), &target.join(entry.file_name()))?;
    }
    // Not empty means something else lives here, which is the point.
    let _ = std::fs::remove_dir(target);
    Ok(())
}

fn remove_stale_roots(installed: &[Version]) -> Result<()> {
    let dir = paths::roots_dir()?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(version) = name.parse::<Version>() else {
            continue;
        };
        if !installed.contains(&version) {
            remove_root(&entry.path())?;
        }
    }
    Ok(())
}

/// Remove a whole root. Only ever called on directories dv composed.
fn remove_root(root: &Path) -> Result<()> {
    if root.join(RECORD).is_file() {
        remove_path(root)?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    let result = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => Err(err),
    };
    result.with_context(|| {
        format!(
            "could not remove {} — {}",
            path.display(),
            if cfg!(windows) {
                "close anything running from it and try again"
            } else {
                "check its permissions"
            }
        )
    })
}

/// Remove directories left empty by a removed entry, stopping at the root.
fn prune_empty_parents(root: &Path, relative: &str) {
    let mut current = Path::new(relative).parent();
    while let Some(dir) = current.filter(|dir| !dir.as_os_str().is_empty()) {
        if std::fs::remove_dir(root.join(dir)).is_err() {
            break; // not empty, or not ours to judge
        }
        current = dir.parent();
    }
}

fn children(dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", dir.display())),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

fn read_record(root: &Path) -> Result<Record> {
    match std::fs::read_to_string(root.join(RECORD)) {
        Ok(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", root.display())),
    }
}

fn write_record(root: &Path, record: &Record) -> Result<()> {
    let path = root.join(RECORD);
    let body = serde_json::to_string_pretty(record)?;
    std::fs::write(&path, format!("{body}\n"))
        .with_context(|| format!("could not write {}", path.display()))
}

/// Whether `root` holds what dv composed into it — used by doctor.
pub fn missing_entries(root: &Path) -> Result<Vec<String>> {
    let record = read_record(root)?;
    Ok(record
        .entries
        .keys()
        .filter(|relative| !root.join(relative).exists())
        .cloned()
        .collect())
}

/// The `dotnet` host in a root.
pub fn host(root: &Path) -> PathBuf {
    root.join(platform::exe("dotnet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    fn entry(relative: &str, scope: Scope, owner: &str) -> Entry {
        Entry {
            relative: relative.into(),
            scope,
            owner: v(owner),
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry("dotnet", Scope::Shared, "8.0.426"),
            entry("dotnet", Scope::Shared, "10.0.402"),
            entry(
                "shared/Microsoft.NETCore.App/8.0.32",
                Scope::Shared,
                "8.0.426",
            ),
            entry(
                "shared/Microsoft.NETCore.App/10.0.13",
                Scope::Shared,
                "10.0.402",
            ),
            entry("sdk/8.0.426", Scope::PerSdk, "8.0.426"),
            entry("sdk/10.0.402", Scope::PerSdk, "10.0.402"),
            entry(
                "sdk-manifests/8.0.100/microsoft.net.sdk.ios",
                Scope::PerSdk,
                "8.0.426",
            ),
        ]
    }

    #[test]
    fn a_per_sdk_root_has_one_sdk_and_every_runtime() {
        let desired = desired_for(&sample(), Some(&v("8.0.426")));
        let keys: Vec<&str> = desired.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "dotnet",
                "sdk-manifests/8.0.100/microsoft.net.sdk.ios",
                "sdk/8.0.426",
                "shared/Microsoft.NETCore.App/10.0.13",
                "shared/Microsoft.NETCore.App/8.0.32",
            ]
        );
        // The newest host, even in an older SDK's root.
        assert_eq!(desired["dotnet"], v("10.0.402"));
    }

    #[test]
    fn the_combined_root_has_everything() {
        let desired = desired_for(&sample(), None);
        assert!(desired.contains_key("sdk/8.0.426"));
        assert!(desired.contains_key("sdk/10.0.402"));
        assert_eq!(desired.len(), 6);
    }

    #[test]
    fn link_tree_shares_inodes_and_lands_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/sdk/10.0.402");
        std::fs::create_dir_all(source.join("Roslyn")).unwrap();
        std::fs::write(source.join("dotnet.dll"), b"sdk").unwrap();
        std::fs::write(source.join("Roslyn/csc.dll"), b"csc").unwrap();

        let target = dir.path().join("root/sdk/10.0.402");
        link_tree(&source, &target).unwrap();
        assert_eq!(
            std::fs::read(target.join("Roslyn/csc.dll")).unwrap(),
            b"csc"
        );
        assert!(!dir.path().join("root/sdk/.10.0.402.dv-incoming").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let a = std::fs::metadata(source.join("dotnet.dll")).unwrap().ino();
            let b = std::fs::metadata(target.join("dotnet.dll")).unwrap().ino();
            assert_eq!(a, b, "linked, not copied");
        }
    }

    #[test]
    fn sync_leaves_unrecorded_entries_alone() {
        let dir = tempfile::tempdir().unwrap();
        let versions = dir.path().join("versions/10.0.402");
        std::fs::create_dir_all(versions.join("sdk/10.0.402")).unwrap();
        std::fs::write(versions.join("sdk/10.0.402/dotnet.dll"), b"sdk").unwrap();

        // Simulate a root with a workload pack dv never placed there.
        let root = dir.path().join("root");
        let workload = root.join("packs/Microsoft.iOS.Sdk/18.0.1");
        std::fs::create_dir_all(&workload).unwrap();
        std::fs::write(workload.join("pack.txt"), b"workload").unwrap();

        let mut record = Record::default();
        record
            .entries
            .insert("sdk/9.0.105".into(), "9.0.105".into());
        std::fs::create_dir_all(root.join("sdk/9.0.105")).unwrap();
        write_record(&root, &record).unwrap();

        // Desired is empty: the recorded entry goes, the workload stays.
        sync_root(&dir.path().join("versions"), &root, &BTreeMap::new(), &[]).unwrap();
        assert!(!root.join("sdk/9.0.105").exists());
        assert!(workload.join("pack.txt").is_file());
        assert!(read_record(&root).unwrap().entries.is_empty());
    }

    #[test]
    fn a_workload_update_inside_a_linked_entry_survives_relinking() {
        let dir = tempfile::tempdir().unwrap();
        let versions = dir.path().join("versions");
        let entry = "sdk-manifests/8.0.100/microsoft.net.sdk.android";
        let shipped = versions.join("8.0.426").join(entry);
        std::fs::create_dir_all(&shipped).unwrap();
        std::fs::write(shipped.join("WorkloadManifest.json"), b"shipped").unwrap();

        let root = dir.path().join("root");
        let mut desired = BTreeMap::new();
        desired.insert(entry.to_string(), v("8.0.426"));
        sync_root(&versions, &root, &desired, &[]).unwrap();

        // `dotnet workload update` adds a side-by-side manifest version.
        let update = root.join(entry).join("34.0.154");
        std::fs::create_dir_all(&update).unwrap();
        std::fs::write(update.join("WorkloadManifest.json"), b"updated").unwrap();

        // A forced reinstall relinks the entry from the fresh tree.
        std::fs::write(shipped.join("WorkloadManifest.json"), b"reinstalled").unwrap();
        sync_root(&versions, &root, &desired, &[v("8.0.426")]).unwrap();
        assert_eq!(
            std::fs::read(root.join(entry).join("WorkloadManifest.json")).unwrap(),
            b"reinstalled"
        );
        assert_eq!(
            std::fs::read(update.join("WorkloadManifest.json")).unwrap(),
            b"updated"
        );

        // Dropping the entry entirely still keeps what dv did not ship.
        sync_root(&versions, &root, &BTreeMap::new(), &[]).unwrap();
        assert!(!root.join(entry).join("WorkloadManifest.json").exists());
        assert!(update.join("WorkloadManifest.json").is_file());
    }
}
