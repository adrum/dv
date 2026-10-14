//! `dv self update` — replace this binary with the newest published one.
//!
//! dv's own releases are plain release assets: `dv-<platform>.tar.gz` behind
//! the "latest release" redirect, with a `.sha256` sidecar. The same two files
//! `install.sh` uses, so there is one publishing format, not two.

use anyhow::{Context, Result, bail};

use crate::archive::{self, Layout};
use crate::exit::{self, Failed};
use crate::{net, paths, platform};

/// Where dv's own releases are published. Set `DV_RELEASE_BASE_URL` at build
/// time to point a fork or mirror elsewhere; set it at run time to test one.
const DEFAULT_RELEASE_BASE_URL: &str = match option_env!("DV_DEFAULT_RELEASE_BASE_URL") {
    Some(url) => url,
    None => "https://github.com/adrum/dv/releases/latest/download",
};

pub fn update(force: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let base = std::env::var("DV_RELEASE_BASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASE_BASE_URL.to_string());
    let asset = format!("dv-{}.tar.gz", platform::current()?);
    let url = format!("{}/{asset}", base.trim_end_matches('/'));

    // Verification fails closed: no sidecar, or a malformed one, stops here.
    let sidecar = net::fetch_text(&format!("{url}.sha256"))
        .with_context(|| format!("could not fetch the checksum for {asset}"))?;
    let expected = sidecar
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Failed::new(
            exit::VERIFICATION,
            format!("the published checksum for {asset} is not a sha256 — dv was not replaced"),
        ));
    }

    let cached = paths::cache_dir()?.join(format!(".self-{asset}"));
    let _ = std::fs::remove_file(&cached);
    let downloaded = net::download(&url, &cached)?.sha256;
    if downloaded != expected {
        let _ = std::fs::remove_file(&cached);
        return Err(Failed::new(
            exit::VERIFICATION,
            format!(
                "{asset} does not match its published checksum — expected sha256 {expected}, \
                 got {downloaded}. dv was not replaced."
            ),
        ));
    }

    let staging = paths::cache_dir()?.join(".dv-self.incoming");
    let _ = std::fs::remove_dir_all(&staging);
    archive::extract(&cached, &staging, Layout::StripWrapper)?;
    let _ = std::fs::remove_file(&cached);
    let dv = platform::exe("dv");
    let incoming = staging.join("bin").join(&dv);
    if !incoming.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("{asset} does not contain bin/{dv}");
    }

    let latest = std::process::Command::new(&incoming)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    if latest == current && !force {
        let _ = std::fs::remove_dir_all(&staging);
        println!("dv {current} is already the newest published build");
        return Ok(());
    }

    let target = std::env::current_exe().context("could not determine dv's own path")?;
    let beside = target.with_extension("incoming");
    std::fs::copy(&incoming, &beside).with_context(|| {
        format!(
            "could not stage the new dv next to {} — is that directory writable?",
            target.display()
        )
    })?;
    paths::make_executable(&beside)?;
    swap_in(&beside, &target).with_context(|| format!("could not replace {}", target.display()))?;
    let _ = std::fs::remove_dir_all(&staging);

    println!("dv {current} -> {latest}");
    Ok(())
}

/// Swap in place. Replacing a running binary by rename is safe on Unix — the
/// running process keeps its open inode.
#[cfg(not(windows))]
fn swap_in(new: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(new, target)
}

/// Windows will not replace or delete a running executable, but it will
/// rename one. So the running dv steps aside and the new one takes its name.
#[cfg(windows)]
fn swap_in(new: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    let old = target.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(target, &old)?;
    if let Err(err) = std::fs::rename(new, target) {
        let _ = std::fs::rename(&old, target);
        return Err(err);
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}
