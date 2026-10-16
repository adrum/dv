//! Downloading, verifying and unpacking an SDK.

use anyhow::{Context, Result, bail};

use crate::archive::{self, Layout};
use crate::config::Config;
use crate::exit::{self, Failed};
use crate::globaljson::SdkRequest;
use crate::installs::{self, ArtifactRecord};
use crate::metadata::Freshness;
use crate::releases::{self, Sdk};
use crate::runtimes;
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
    let check = version.clone();
    let changed = install_archive(
        &Item {
            label: format!(".NET SDK {version}"),
            name: version.to_string(),
            parent: paths::versions_dir()?,
            version: version.clone(),
            rid,
            file,
            owner: roots::Owner::sdk(&version),
        },
        force,
        move |tree| {
            tree.join(platform::exe("dotnet")).is_file()
                && tree
                    .join("sdk")
                    .join(check.to_string())
                    .join("dotnet.dll")
                    .is_file()
        },
    )?;
    Ok(Outcome { version, changed })
}

/// Install one published runtime, already chosen.
pub fn install_runtime(runtime: &releases::Runtime, rid: &str, force: bool) -> Result<Outcome> {
    let version = runtime.version.clone();
    let component = runtime.component;
    let file = runtime.archive_for(rid).ok_or_else(|| {
        Failed::new(
            exit::NOT_PUBLISHED,
            format!(
                "{} {version} has no archive published for {rid}",
                component.display()
            ),
        )
    })?;
    let check = version.clone();
    let changed = install_archive(
        &Item {
            label: format!("{} {version}", component.display()),
            name: format!("{component}-{version}"),
            parent: runtimes::runtimes_dir()?,
            version: version.clone(),
            rid,
            file,
            owner: roots::Owner::runtime(component, &version),
        },
        force,
        move |tree| runtimes::is_complete(tree, component, &check),
    )?;
    Ok(Outcome { version, changed })
}

/// What `install_archive` installs, and where.
struct Item<'a> {
    /// `.NET SDK 10.0.402`, `ASP.NET Core runtime 8.0.12` — for messages.
    label: String,
    /// The directory name under `parent`.
    name: String,
    parent: std::path::PathBuf,
    version: Version,
    rid: &'a str,
    file: &'a releases::File,
    /// How install roots refer to this tree.
    owner: roots::Owner,
}

/// Download, verify, extract and swap in one archive, then relink roots.
/// Returns whether anything changed.
fn install_archive(
    item: &Item<'_>,
    force: bool,
    is_complete: impl Fn(&std::path::Path) -> bool,
) -> Result<bool> {
    let Item {
        label,
        name,
        parent,
        version,
        rid,
        file,
        owner,
    } = item;
    let expected = file.verified_sha512()?.to_ascii_lowercase();

    // Everything below writes to paths derived from the name — the cache
    // entry, the staging tree, the install directory itself — so one holder
    // at a time, per install.
    let _lock = lock::acquire(parent, name, &format!("installing {label}"))?;

    let target = parent.join(name);
    let replacing = is_complete(&target);
    if !force
        && replacing
        && let Some(record) = installs::read_record(&target)?
        && record.sha512 == expected
    {
        // Already here. Still make sure it is reachable: a previous run may
        // have been interrupted after extracting and before composing roots.
        roots::sync(&[])?;
        shims::sync()?;
        return Ok(false);
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

    let staging = parent.join(format!(".{name}.incoming"));
    let _ = std::fs::remove_dir_all(&staging);
    eprintln!("  extracting {label}");
    archive::extract(&cached, &staging, Layout::AsIs)?;
    if !is_complete(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        bail!(
            "{} does not contain {label} — it is not the archive the metadata describes",
            file.cache_name()
        );
    }

    // Swap late, so a failure above leaves the previous install untouched.
    if target.exists() {
        let previous = parent.join(format!(".{name}.previous"));
        let _ = std::fs::remove_dir_all(&previous);
        std::fs::rename(&target, &previous)
            .with_context(|| format!("could not move the existing {label} aside"))?;
        let result = std::fs::rename(&staging, &target);
        if result.is_err() {
            let _ = std::fs::rename(&previous, &target);
            result.with_context(|| format!("could not install {label}"))?;
        }
        let _ = std::fs::remove_dir_all(&previous);
    } else {
        std::fs::rename(&staging, &target)
            .with_context(|| format!("could not install {label} into {}", target.display()))?;
    }

    installs::write_record(
        &target,
        &ArtifactRecord {
            version: version.to_string(),
            platform: rid.to_string(),
            file: file.cache_name(),
            url: file.url.clone(),
            sha512: expected,
            installed_at: installs::now_unix(),
            // Recorded from the tree as extracted, so doctor can later speak
            // about what is installed rather than about the download.
            files: installs::hash_tree(&target)?,
        },
    )?;

    eprintln!("  linking install roots");
    let refreshed = if replacing {
        vec![owner.clone()]
    } else {
        Vec::new()
    };
    roots::sync(&refreshed)?;
    shims::sync()?;
    Ok(true)
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
                .sdks
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

/// The published runtime a spec names: `aspnetcore@8.0` is the newest 8.0
/// ASP.NET Core runtime published for this machine.
pub fn choose_runtime(spec: &runtimes::Spec) -> Result<releases::Runtime> {
    let config = Config::load()?;
    let rid = platform::current()?;
    if spec.component == runtimes::Component::WindowsDesktop && !rid.starts_with("win-") {
        bail!("the Windows Desktop runtime is only published for Windows");
    }
    // Installing always asks the server whether the metadata changed, as for
    // an SDK: a runtime patched an hour ago should be the one installed.
    let index = releases::fetch_index(&config.releases_url(), Freshness::Revalidate)?;
    let channels = releases::channels_for(&index, &spec.selector);
    let mut available: Vec<releases::Runtime> = Vec::new();
    for data in releases::fetch_channels(&channels, Freshness::Revalidate) {
        available.extend(data?.runtimes.into_iter().filter(|runtime| {
            runtime.component == spec.component && runtime.archive_for(&rid).is_some()
        }));
    }
    let versions: Vec<Version> = available.iter().map(|r| r.version.clone()).collect();
    let chosen = match &spec.selector {
        // The channels fetched for `lts` are already the LTS ones.
        Selector::Lts => Selector::Latest.best(&versions),
        selector => selector.best(&versions),
    };
    let Some(version) = chosen else {
        return Err(Failed::new(
            exit::NOT_PUBLISHED,
            format!(
                "no {} {} is published for {rid} — run `dv runtime list --remote` to see \
                 what is",
                spec.component.display(),
                spec.selector
            ),
        ));
    };
    Ok(available
        .into_iter()
        .find(|runtime| runtime.version == version)
        .expect("chosen from this list"))
}
