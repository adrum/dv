//! Archive extraction.
//!
//! SDK archives put `dotnet`, `sdk/`, `shared/` and the rest at the top level,
//! so they are extracted as-is. dv's own release archives wrap their contents
//! in one directory, which is stripped — the shape `tar --strip-components 1`
//! produces.
//!
//! An archive is untrusted input even when its hash checks out, so every entry
//! is checked before anything is written: no absolute paths, no `..`, and no
//! links, which an SDK archive never contains and which could otherwise point
//! a later entry outside the destination.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Whether the first path component is a wrapper directory to drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    AsIs,
    StripWrapper,
}

pub fn extract(archive: &Path, dest: &Path, layout: Layout) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    std::fs::create_dir_all(dest)
        .with_context(|| format!("could not create {}", dest.display()))?;
    let extracted = if name.ends_with(".zip") {
        extract_zip(archive, dest, layout)?
    } else {
        extract_tar_gz(archive, dest, layout)?
    };
    if extracted == 0 {
        bail!("{} contained no files", archive.display());
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path, layout: Layout) -> Result<usize> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("could not open {}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);

    let mut extracted = 0usize;
    for entry in tar
        .entries()
        .with_context(|| format!("{} is not a readable tar.gz", archive.display()))?
    {
        let mut entry = entry.with_context(|| format!("{} is corrupt", archive.display()))?;
        let path = entry.path()?.into_owned();
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            bail!(
                "refusing to extract `{}` — the archive contains a link, which no SDK \
                 archive does",
                path.display()
            );
        }
        let Some(relative) = relative_path(&path, layout)? else {
            continue;
        };
        let out = dest.join(&relative);
        if kind.is_dir() {
            std::fs::create_dir_all(&out)
                .with_context(|| format!("could not create {}", out.display()))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        entry.unpack(&out).with_context(|| {
            format!("could not extract {} to {}", path.display(), out.display())
        })?;
        extracted += 1;
    }
    Ok(extracted)
}

#[cfg(windows)]
fn extract_zip(archive: &Path, dest: &Path, layout: Layout) -> Result<usize> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("could not open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a readable zip", archive.display()))?;

    let mut extracted = 0usize;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .with_context(|| format!("{} is corrupt", archive.display()))?;
        let path = PathBuf::from(entry.name().replace('\\', "/"));
        let Some(relative) = relative_path(&path, layout)? else {
            continue;
        };
        let out = dest.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .with_context(|| format!("could not create {}", out.display()))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let mut file = std::fs::File::create(&out)
            .with_context(|| format!("could not create {}", out.display()))?;
        std::io::copy(&mut entry, &mut file)
            .with_context(|| format!("could not extract {}", path.display()))?;
        extracted += 1;
    }
    Ok(extracted)
}

/// Zip archives are only published for Windows, and only Windows carries the
/// code to read them.
#[cfg(not(windows))]
fn extract_zip(archive: &Path, _dest: &Path, _layout: Layout) -> Result<usize> {
    bail!(
        "{} is a zip archive, which dv only reads on Windows",
        archive.display()
    )
}

/// Where an entry lands relative to the destination, refusing anything that
/// would escape it. `None` for the wrapper directory itself, or for `./`.
fn relative_path(path: &Path, layout: Layout) -> Result<Option<PathBuf>> {
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => bail!(
                "refusing to extract `{}` — the archive contains an absolute or \
                 parent-directory path",
                path.display()
            ),
        }
    }
    let mut components = path
        .components()
        .filter(|component| matches!(component, Component::Normal(_)));
    if layout == Layout::StripWrapper {
        components.next();
    }
    let relative: PathBuf = components.collect();
    if relative.as_os_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tarball(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join("archive.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *body).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn an_sdk_archive_extracts_as_published() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tarball(
            dir.path(),
            &[
                ("./dotnet", b"muxer"),
                ("./sdk/10.0.402/dotnet.dll", b"sdk"),
            ],
        );
        let dest = dir.path().join("out");
        extract(&archive, &dest, Layout::AsIs).unwrap();
        assert_eq!(std::fs::read(dest.join("dotnet")).unwrap(), b"muxer");
        assert!(dest.join("sdk/10.0.402/dotnet.dll").is_file());
    }

    #[test]
    fn a_release_archive_loses_its_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tarball(dir.path(), &[("dv-0.1.0-osx-arm64/bin/dv", b"binary")]);
        let dest = dir.path().join("out");
        extract(&archive, &dest, Layout::StripWrapper).unwrap();
        assert_eq!(std::fs::read(dest.join("bin/dv")).unwrap(), b"binary");
    }

    /// `tar::Builder` refuses to write a traversing path, so the header is
    /// filled in by hand — which is exactly how a hostile archive arrives.
    fn hostile_tarball(dir: &Path, name: &str, kind: tar::EntryType) -> PathBuf {
        let path = dir.join("hostile.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let body = b"nope";
        let mut header = tar::Header::new_gnu();
        header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_entry_type(kind);
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &body[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn refuses_paths_that_escape_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let archive = hostile_tarball(dir.path(), "./../../evil", tar::EntryType::Regular);
        let dest = dir.path().join("out");
        assert!(extract(&archive, &dest, Layout::AsIs).is_err());
        assert!(!dir.path().join("evil").exists());
    }

    #[test]
    fn refuses_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let archive = hostile_tarball(dir.path(), "/etc/dv-evil", tar::EntryType::Regular);
        assert!(extract(&archive, &dir.path().join("out"), Layout::AsIs).is_err());
    }

    #[test]
    fn refuses_links() {
        let dir = tempfile::tempdir().unwrap();
        let archive = hostile_tarball(dir.path(), "./sdk", tar::EntryType::Symlink);
        assert!(extract(&archive, &dir.path().join("out"), Layout::AsIs).is_err());
    }

    #[test]
    fn an_empty_archive_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tarball(dir.path(), &[]);
        assert!(extract(&archive, &dir.path().join("out"), Layout::AsIs).is_err());
    }
}
