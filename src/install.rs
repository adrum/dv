//! Downloading, verifying and unpacking an SDK.

use anyhow::{Context, Result, bail};

use crate::archive::{self, Layout};
use crate::config::Config;
use crate::exit::{self, Failed};
use crate::globaljson::SdkRequest;
use crate::installs::{self, ArtifactRecord};
use crate::metadata::Freshness;
use crate::releases::{self, Sdk};
use crate::version::{Selector, Version};
use crate::{lock, net, paths, platform, roots, shims};

/// What to install.
pub enum Target {
    /// A version or line named on the command line.
    Selector(Selector),
    /// What a `global.json` asks for.
    Request(SdkRequest),
}

pub struct Outcome {
    pub version: Version,
    /// False when the requested version was already installed from the same
    /// archive and nothing was downloaded.
    pub changed: bool,
}

pub fn install(target: &Target, force: bool) -> Result<Outcome> {
    let config = Config::load()?;
    let rid = platform::current()?;
    let sdk = choose(&config, &rid, target)?;
    install_sdk(&sdk, &rid, force)
}

/// Install one published SDK, already chosen.
pub fn install_sdk(sdk: &Sdk, rid: &str, force: bool) -> Result<Outcome> {
    let version = sdk.version.clone();
    let file = sdk.archive_for(rid).ok_or_else(|| {
        Failed::new(
            exit::NOT_PUBLISHED,
            format!(".NET SDK {version} has no archive published for {rid}"),
        )
    })?;
    let expected = file.verified_sha512()?.to_ascii_lowercase();

    // Everything below writes to paths derived from the version — the cache
    // entry, the staging tree, the version directory itself — so one holder
    // at a time, per version.
    let _lock = lock::acquire(
        &paths::versions_dir()?,
        &version.to_string(),
        &format!("installing .NET SDK {version}"),
    )?;

    let replacing = installs::is_installed(&version)?;
    if !force
        && replacing
        && let Some(record) = installs::read_record(&version)?
        && record.sha512 == expected
    {
        // Already here. Still make sure it is reachable: a previous run may
        // have been interrupted after extracting and before composing roots.
        roots::sync(&[])?;
        shims::sync()?;
        return Ok(Outcome {
            version,
            changed: false,
        });
    }

    // Reuse a cached archive only when it hashes to what Microsoft published.
    let cached = paths::cache_dir()?.join(file.cache_name());
    let downloaded = if cached.is_file() && net::digest_file(&cached)?.sha512 == expected {
        eprintln!("  using cached {}", file.cache_name());
        expected.clone()
    } else {
        let _ = std::fs::remove_file(&cached);
        net::download(&file.url, &cached)?.sha512
    };

    if downloaded != expected {
        let _ = std::fs::remove_file(&cached);
        return Err(Failed::new(
            exit::VERIFICATION,
            format!(
                "{} does not match Microsoft's published hash — expected sha512 {expected}, \
                 got {downloaded}. Nothing was installed.",
                file.cache_name()
            ),
        ));
    }

    let target = paths::version_dir(&version.to_string())?;
    let staging = paths::versions_dir()?.join(format!(".{version}.incoming"));
    let _ = std::fs::remove_dir_all(&staging);
    eprintln!("  extracting .NET SDK {version}");
    archive::extract(&cached, &staging, Layout::AsIs)?;

    let dll = staging
        .join("sdk")
        .join(version.to_string())
        .join("dotnet.dll");
    if !staging.join(platform::exe("dotnet")).is_file() || !dll.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!(
            "{} does not contain the dotnet host and sdk/{version} — it is not an SDK archive",
            file.cache_name()
        );
    }

    // Swap late, so a failure above leaves the previous install untouched.
    if target.exists() {
        let previous = paths::versions_dir()?.join(format!(".{version}.previous"));
        let _ = std::fs::remove_dir_all(&previous);
        std::fs::rename(&target, &previous)
            .with_context(|| format!("could not move the existing {version} aside"))?;
        let result = std::fs::rename(&staging, &target);
        if result.is_err() {
            let _ = std::fs::rename(&previous, &target);
            result.with_context(|| format!("could not install {version}"))?;
        }
        let _ = std::fs::remove_dir_all(&previous);
    } else {
        std::fs::rename(&staging, &target)
            .with_context(|| format!("could not install {version} into {}", target.display()))?;
    }

    installs::write_record(
        &version,
        &ArtifactRecord {
            version: version.to_string(),
            platform: rid.to_string(),
            file: file.cache_name(),
            url: file.url.clone(),
            sha512: expected,
            installed_at: installs::now_unix(),
            // Recorded from the tree as extracted, so doctor can later speak
            // about what is installed rather than about the download.
            files: installs::hash_tree(&version)?,
        },
    )?;

    eprintln!("  linking install roots");
    let refreshed = if replacing {
        vec![version.clone()]
    } else {
        Vec::new()
    };
    roots::sync(&refreshed)?;
    shims::sync()?;

    Ok(Outcome {
        version,
        changed: true,
    })
}

/// The published SDK a target names.
fn choose(config: &Config, rid: &str, target: &Target) -> Result<Sdk> {
    // Installing always asks the server whether the metadata changed: a patch
    // released minutes ago should install, and a 304 costs next to nothing.
    let index = releases::fetch_index(&config.releases_url(), Freshness::Revalidate)?;
    let channels = match target {
        Target::Selector(selector) => releases::channels_for(&index, selector),
        Target::Request(request) => releases::channels_for_request(&index, request),
    };

    let mut available: Vec<Sdk> = Vec::new();
    for sdks in releases::fetch_channels(&channels, Freshness::Revalidate) {
        available.extend(
            sdks?
                .into_iter()
                .filter(|sdk| sdk.archive_for(rid).is_some()),
        );
    }
    let versions: Vec<Version> = available.iter().map(|sdk| sdk.version.clone()).collect();

    let chosen = match target {
        // The channels fetched for `lts` are already the LTS ones; within
        // them, it means the newest release.
        Target::Selector(Selector::Lts) => Selector::Latest.best(&versions),
        Target::Selector(selector) => selector.best(&versions),
        // What the host would choose if everything published were installed
        // — releases first, so `allowPrerelease` defaulting to true does not
        // turn every unpinned `dv install` into a preview.
        Target::Request(request) => {
            let releases: Vec<&Version> = versions.iter().filter(|v| !v.is_prerelease()).collect();
            request
                .select(releases)
                .or_else(|| request.select(&versions))
        }
    };

    let Some(version) = chosen else {
        let wanted = match target {
            Target::Selector(selector) => format!(".NET SDK {selector}"),
            Target::Request(request) => format!(".NET SDK matching {}", request.describe()),
        };
        return Err(Failed::new(
            exit::NOT_PUBLISHED,
            format!("no {wanted} is published for {rid} — run `dv list --remote` to see what is"),
        ));
    };
    Ok(available
        .into_iter()
        .find(|sdk| sdk.version == version)
        .expect("chosen from this list"))
}
