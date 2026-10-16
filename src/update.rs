//! `dv update` — move every tracked requirement to the newest SDK it accepts,
//! then remove what no requirement needs any more.
//!
//! Pruning is the half that needs care. An SDK is removed only when no
//! tracked requirement, the configured default, or the current directory
//! resolves to it — and dv says exactly what went and how to get it back.
//! What dv cannot see is a `global.json` it was never told about, which is
//! why `dv install` in a repository tracks that repository's file.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::exit::{self, Failed};
use crate::metadata::Freshness;
use crate::releases::{self, Sdk};
use crate::tracked::{Requirement, Tracked, Want};
use crate::version::Version;
use crate::{install, installs, lock, platform, roots, shims};

/// One requirement and what an update does for it.
pub struct Step {
    pub requirement: Requirement,
    /// The installed version that satisfies it before the update.
    pub current: Option<Version>,
    /// The newest published version that satisfies it.
    pub target: Option<Version>,
    /// Why nothing can be done for it, when that is the case.
    pub problem: Option<String>,
}

impl Step {
    /// Stable machine name of what the update does for this requirement.
    pub fn action(&self) -> &'static str {
        match (&self.problem, &self.current, &self.target) {
            (Some(_), _, _) => "unavailable",
            (None, Some(current), Some(target)) if current == target => "none",
            (None, _, Some(_)) => "install",
            (None, _, None) => "unavailable",
        }
    }
}

pub struct Report {
    pub steps: Vec<Step>,
    /// Versions installed by this update.
    pub installed: Vec<Version>,
    /// Versions removed because nothing needs them — or, in a dry run, that
    /// would be.
    pub removed: Vec<Version>,
    /// Requirements no longer tracked: a `global.json` deleted or unpinned.
    pub dropped: Vec<(Requirement, String)>,
}

pub fn run(dry_run: bool, prune: bool) -> Result<Report> {
    let config = Config::load()?;
    let rid = platform::current()?;
    let mut tracked = Tracked::load()?;
    let requirements = tracked.with_default(config.default.as_deref());
    let installed = installs::installed()?;

    // Read every requirement. A global.json that went away is dropped rather
    // than failing the update: the repository moved on.
    let mut wants: Vec<(Requirement, Want)> = Vec::new();
    let mut dropped = Vec::new();
    let mut broken: Vec<(Requirement, String)> = Vec::new();
    for requirement in requirements {
        match requirement.want() {
            Ok(want) => wants.push((requirement, want)),
            Err(reason) if matches!(requirement, Requirement::GlobalJson { .. }) => {
                dropped.push((requirement, reason))
            }
            Err(reason) => broken.push((requirement, reason)),
        }
    }

    let available = published(&config, &rid, &wants)?;
    let published: Vec<Version> = available.iter().map(|sdk| sdk.version.clone()).collect();

    let mut steps: Vec<Step> = wants
        .iter()
        .map(|(requirement, want)| {
            let target = want.newest_published(&published);
            Step {
                requirement: requirement.clone(),
                current: want.satisfied_by(&installed),
                problem: target
                    .is_none()
                    .then(|| format!("nothing published for {rid} satisfies it")),
                target,
            }
        })
        .collect();
    steps.extend(broken.into_iter().map(|(requirement, reason)| Step {
        requirement,
        current: None,
        target: None,
        problem: Some(reason),
    }));

    let mut to_install: Vec<Version> = steps
        .iter()
        .filter(|step| step.action() == "install")
        .filter_map(|step| step.target.clone())
        .filter(|target| !installed.contains(target))
        .collect();
    to_install.sort();
    to_install.dedup();

    let mut newly_installed = Vec::new();
    if !dry_run {
        if !dropped.is_empty() {
            tracked
                .requirements
                .retain(|requirement| !dropped.iter().any(|(gone, _)| gone == requirement));
            tracked.save()?;
        }
        for version in &to_install {
            let sdk = available
                .iter()
                .find(|sdk| &sdk.version == version)
                .expect("targets come from this list");
            eprintln!("  installing .NET SDK {version}");
            install::install_sdk(sdk, &rid, false)
                .with_context(|| format!("could not install .NET SDK {version}"))?;
            newly_installed.push(version.clone());
        }
    }

    // What the machine holds once the installs are done — real after an
    // update, projected for a dry run.
    let mut after = installed.clone();
    for version in &to_install {
        if !after.contains(version) {
            after.push(version.clone());
        }
    }
    after.sort();

    let removed = if prune {
        let needed = needed(&wants, &after)?;
        let removable: Vec<Version> = after
            .iter()
            .filter(|version| !needed.contains(version))
            .cloned()
            .collect();
        if !dry_run {
            for version in &removable {
                let _lock = lock::acquire(
                    &crate::paths::versions_dir()?,
                    &version.to_string(),
                    &format!("removing .NET SDK {version}"),
                )?;
                installs::remove(version)?;
            }
        }
        removable
    } else {
        Vec::new()
    };

    if !dry_run && (!newly_installed.is_empty() || !removed.is_empty()) {
        roots::sync(&[])?;
        shims::sync()?;
    }

    Ok(Report {
        steps,
        installed: if dry_run { to_install } else { newly_installed },
        removed,
        dropped,
    })
}

/// Every installed version something still needs.
///
/// Each requirement keeps the version it resolves to — the one the `dotnet`
/// host would choose for a `global.json`. So does whatever the current
/// directory resolves to, which covers a repository dv was never told about
/// at least for the one you are standing in.
fn needed(wants: &[(Requirement, Want)], installed: &[Version]) -> Result<Vec<Version>> {
    let mut needed: Vec<Version> = wants
        .iter()
        .filter_map(|(_, want)| want.satisfied_by(installed))
        .collect();
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(Some(file)) = crate::globaljson::nearest(&cwd)
        && let Ok(request) = file.request
        && request.is_pin()
        && let Some(version) = request.select(installed)
    {
        needed.push(version);
    }
    needed.sort();
    needed.dedup();
    Ok(needed)
}

/// Every SDK published for this machine in the channels the requirements can
/// reach, fetched fresh: an update exists to notice new releases.
fn published(config: &Config, rid: &str, wants: &[(Requirement, Want)]) -> Result<Vec<Sdk>> {
    if wants.is_empty() {
        return Ok(Vec::new());
    }
    let index = releases::fetch_index(&config.releases_url(), Freshness::Revalidate)?;
    let mut channels: Vec<&releases::Channel> = Vec::new();
    for (_, want) in wants {
        let reachable = match want {
            Want::Selector(selector) => releases::channels_for(&index, selector),
            Want::Request(request) => releases::channels_for_request(&index, request),
        };
        for channel in reachable {
            if !channels.iter().any(|c| c.version == channel.version) {
                channels.push(channel);
            }
        }
    }

    let mut available = Vec::new();
    for sdks in releases::fetch_channels(&channels, Freshness::Revalidate) {
        available.extend(
            sdks?
                .into_iter()
                .filter(|sdk| sdk.archive_for(rid).is_some()),
        );
    }
    Ok(available)
}

/// The exit code a finished update leaves with: success, unless something
/// asked for could not be found.
pub fn failure(report: &Report) -> Option<anyhow::Error> {
    let unavailable: Vec<String> = report
        .steps
        .iter()
        .filter(|step| step.action() == "unavailable")
        .map(|step| step.requirement.label())
        .collect();
    (!unavailable.is_empty()).then(|| {
        Failed::new(
            exit::NOT_PUBLISHED,
            format!(
                "nothing could be found for {} — everything else was updated",
                unavailable.join(", ")
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    fn want(selector: &str) -> (Requirement, Want) {
        let requirement = Requirement::Explicit {
            selector: selector.into(),
        };
        let want = requirement.want().unwrap();
        (requirement, want)
    }

    #[test]
    fn each_requirement_keeps_what_it_resolves_to_and_nothing_older() {
        let installed = [v("8.0.426"), v("10.0.401"), v("10.0.402")];
        let wants = [want("10.0"), want("8.0.426")];
        let kept = needed(&wants, &installed).unwrap();
        assert!(kept.contains(&v("10.0.402")));
        assert!(kept.contains(&v("8.0.426")), "an exact pin is kept");
        assert!(!kept.contains(&v("10.0.401")), "superseded by 10.0.402");
    }

    #[test]
    fn a_step_says_what_it_does() {
        let step = |current: Option<&str>, target: Option<&str>| Step {
            requirement: Requirement::Explicit {
                selector: "10.0".into(),
            },
            current: current.map(v),
            target: target.map(v),
            problem: None,
        };
        assert_eq!(step(Some("10.0.402"), Some("10.0.402")).action(), "none");
        assert_eq!(step(Some("10.0.401"), Some("10.0.402")).action(), "install");
        assert_eq!(step(None, Some("10.0.402")).action(), "install");
        assert_eq!(step(None, None).action(), "unavailable");
    }
}
