//! `dv update` — move every tracked requirement to the newest SDK or runtime
//! it accepts, then remove what it moved them away from.
//!
//! Pruning is the half that needs care, so it is narrow: an update removes
//! only an SDK it superseded — what a requirement resolved to before the
//! update, now replaced — and only when nothing else still resolves to it:
//! no other requirement, not the default, not the current directory. An SDK
//! no requirement tracks is never touched; the report lists it as untracked.
//! Every removal is named, with how to get it back.

use std::fmt;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::exit::{self, Failed};
use crate::metadata::Freshness;
use crate::releases::{self, Sdk};
use crate::runtimes::{self, Component};
use crate::tracked::{Requirement, Tracked, Want};
use crate::version::Version;
use crate::{install, installs, lock, platform, roots, shims};

/// Something an update installs or removes: an SDK, or one runtime.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Item {
    Sdk(Version),
    Runtime(Component, Version),
}

impl Item {
    fn of(component: Option<Component>, version: Version) -> Self {
        match component {
            None => Item::Sdk(version),
            Some(component) => Item::Runtime(component, version),
        }
    }
}

/// `10.0.402` for an SDK, `aspnetcore@8.0.12` for a runtime. Interface: the
/// JSON lists use it.
impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Sdk(version) => write!(f, "{version}"),
            Item::Runtime(component, version) => write!(f, "{component}@{version}"),
        }
    }
}

/// SDK and runtime versions, installed or published.
#[derive(Default)]
struct Inventory {
    items: Vec<Item>,
}

impl Inventory {
    /// The versions a requirement chooses among: SDKs, or one runtime's.
    fn versions_for(&self, component: Option<Component>) -> Vec<Version> {
        self.items
            .iter()
            .filter_map(|item| match (item, component) {
                (Item::Sdk(version), None) => Some(version.clone()),
                (Item::Runtime(kind, version), Some(wanted)) if *kind == wanted => {
                    Some(version.clone())
                }
                _ => None,
            })
            .collect()
    }
}

/// One requirement and what an update does for it.
pub struct Step {
    pub requirement: Requirement,
    /// The runtime it is about, or `None` for an SDK.
    pub component: Option<Component>,
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
    /// What this update installed — or, in a dry run, would.
    pub installed: Vec<Item>,
    /// What this update superseded and removed — or, in a dry run, would.
    pub removed: Vec<Item>,
    /// Installed SDKs and runtimes no requirement resolves to, left alone.
    pub untracked: Vec<Item>,
    /// Requirements no longer tracked: a `global.json` deleted or unpinned.
    pub dropped: Vec<(Requirement, String)>,
}

pub fn run(dry_run: bool, prune: bool) -> Result<Report> {
    let config = Config::load()?;
    let rid = platform::current()?;
    let mut tracked = Tracked::load()?;
    let requirements = tracked.with_default(config.default.as_deref());
    let installed = Inventory {
        items: installs::installed()?
            .into_iter()
            .map(Item::Sdk)
            .chain(
                runtimes::installed()?
                    .into_iter()
                    .map(|runtime| Item::Runtime(runtime.component, runtime.version)),
            )
            .collect(),
    };

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

    let (sdks, runtimes) = published(&config, &rid, &wants)?;
    let available = Inventory {
        items: sdks
            .iter()
            .map(|sdk| Item::Sdk(sdk.version.clone()))
            .chain(
                runtimes
                    .iter()
                    .map(|runtime| Item::Runtime(runtime.component, runtime.version.clone())),
            )
            .collect(),
    };

    let mut steps: Vec<Step> = wants
        .iter()
        .map(|(requirement, want)| {
            let component = want.component();
            let target = want.newest_published(&available.versions_for(component));
            Step {
                requirement: requirement.clone(),
                component,
                current: want.satisfied_by(&installed.versions_for(component)),
                problem: target
                    .is_none()
                    .then(|| format!("nothing published for {rid} satisfies it")),
                target,
            }
        })
        .collect();
    steps.extend(broken.into_iter().map(|(requirement, reason)| Step {
        requirement,
        component: None,
        current: None,
        target: None,
        problem: Some(reason),
    }));

    let mut to_install: Vec<Item> = steps
        .iter()
        .filter(|step| step.action() == "install")
        .filter_map(|step| step.target.clone().map(|v| Item::of(step.component, v)))
        .filter(|item| !installed.items.contains(item))
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
        for item in &to_install {
            eprintln!("  installing {item}");
            match item {
                Item::Sdk(version) => {
                    let sdk = sdks
                        .iter()
                        .find(|sdk| &sdk.version == version)
                        .expect("targets come from this list");
                    install::install_sdk(sdk, &rid, false)
                }
                Item::Runtime(component, version) => {
                    let runtime = runtimes
                        .iter()
                        .find(|r| r.component == *component && &r.version == version)
                        .expect("targets come from this list");
                    install::install_runtime(runtime, &rid, false)
                }
            }
            .with_context(|| format!("could not install {item}"))?;
            newly_installed.push(item.clone());
        }
    }

    // What the machine holds once the installs are done — real after an
    // update, projected for a dry run.
    let mut after = installed.items.clone();
    for item in &to_install {
        if !after.contains(item) {
            after.push(item.clone());
        }
    }
    after.sort();
    let after = Inventory { items: after };

    let needed = needed(&wants, &after)?;
    let removed = if prune {
        let removable = superseded(&steps, &needed);
        if !dry_run {
            for item in &removable {
                remove(item)?;
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

    let untracked = after
        .items
        .into_iter()
        .filter(|item| !needed.contains(item) && !removed.contains(item))
        .collect();
    Ok(Report {
        steps,
        installed: if dry_run { to_install } else { newly_installed },
        removed,
        untracked,
        dropped,
    })
}

fn remove(item: &Item) -> Result<()> {
    match item {
        Item::Sdk(version) => {
            let _lock = lock::acquire(
                &crate::paths::versions_dir()?,
                &version.to_string(),
                &format!("removing .NET SDK {version}"),
            )?;
            installs::remove(version)
        }
        Item::Runtime(component, version) => {
            let _lock = lock::acquire(
                &runtimes::runtimes_dir()?,
                &format!("{component}-{version}"),
                &format!("removing {} {version}", component.display()),
            )?;
            runtimes::remove(&runtimes::Installed {
                component: *component,
                version: version.clone(),
            })
        }
    }
}

/// What this update supersedes: each requirement's version before the update,
/// where the update moves it to another, unless something still needs it.
fn superseded(steps: &[Step], needed: &[Item]) -> Vec<Item> {
    let mut gone: Vec<Item> = steps
        .iter()
        .filter(|step| step.action() == "install")
        .filter_map(|step| step.current.clone().map(|v| Item::of(step.component, v)))
        .filter(|item| !needed.contains(item))
        .collect();
    gone.sort();
    gone.dedup();
    gone
}

/// Everything installed that something still needs.
///
/// Each requirement keeps the version it resolves to — the one the `dotnet`
/// host would choose for a `global.json`. So does whatever the current
/// directory resolves to, which covers a repository dv was never told about
/// at least for the one you are standing in.
fn needed(wants: &[(Requirement, Want)], installed: &Inventory) -> Result<Vec<Item>> {
    let mut needed: Vec<Item> = wants
        .iter()
        .filter_map(|(_, want)| {
            let component = want.component();
            want.satisfied_by(&installed.versions_for(component))
                .map(|version| Item::of(component, version))
        })
        .collect();
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(Some(file)) = crate::globaljson::nearest(&cwd)
        && let Ok(request) = file.request
        && request.is_pin()
        && let Some(version) = request.select(&installed.versions_for(None))
    {
        needed.push(Item::Sdk(version));
    }
    needed.sort();
    needed.dedup();
    Ok(needed)
}

/// Every SDK and runtime published for this machine in the channels the
/// requirements can reach, fetched fresh: an update exists to notice new
/// releases.
fn published(
    config: &Config,
    rid: &str,
    wants: &[(Requirement, Want)],
) -> Result<(Vec<Sdk>, Vec<releases::Runtime>)> {
    if wants.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let index = releases::fetch_index(&config.releases_url(), Freshness::Revalidate)?;
    let mut channels: Vec<&releases::Channel> = Vec::new();
    for (_, want) in wants {
        let reachable = match want {
            Want::Selector(selector) | Want::Runtime(_, selector) => {
                releases::channels_for(&index, selector)
            }
            Want::Request(request) => releases::channels_for_request(&index, request),
        };
        for channel in reachable {
            if !channels.iter().any(|c| c.version == channel.version) {
                channels.push(channel);
            }
        }
    }

    let (mut sdks, mut runtimes) = (Vec::new(), Vec::new());
    for data in releases::fetch_channels(&channels, Freshness::Revalidate) {
        let data = data?;
        sdks.extend(
            data.sdks
                .into_iter()
                .filter(|sdk| sdk.archive_for(rid).is_some()),
        );
        runtimes.extend(
            data.runtimes
                .into_iter()
                .filter(|runtime| runtime.archive_for(rid).is_some()),
        );
    }
    Ok((sdks, runtimes))
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
        let installed = Inventory {
            items: vec![
                Item::Sdk(v("8.0.426")),
                Item::Sdk(v("10.0.401")),
                Item::Sdk(v("10.0.402")),
                Item::Runtime(Component::AspNetCore, v("8.0.11")),
                Item::Runtime(Component::AspNetCore, v("8.0.12")),
            ],
        };
        let runtime = Requirement::Runtime {
            spec: "aspnetcore@8.0".into(),
        };
        let wants = [
            want("10.0"),
            want("8.0.426"),
            (runtime.clone(), runtime.want().unwrap()),
        ];
        let kept = needed(&wants, &installed).unwrap();
        assert!(kept.contains(&Item::Sdk(v("10.0.402"))));
        assert!(
            kept.contains(&Item::Sdk(v("8.0.426"))),
            "an exact pin is kept"
        );
        assert!(
            !kept.contains(&Item::Sdk(v("10.0.401"))),
            "superseded by 10.0.402"
        );
        assert!(kept.contains(&Item::Runtime(Component::AspNetCore, v("8.0.12"))));
        assert!(!kept.contains(&Item::Runtime(Component::AspNetCore, v("8.0.11"))));
    }

    #[test]
    fn only_what_an_update_moves_past_is_removed() {
        let step = |selector: &str, current: Option<&str>, target: &str| Step {
            requirement: Requirement::Explicit {
                selector: selector.into(),
            },
            component: None,
            current: current.map(v),
            target: Some(v(target)),
            problem: None,
        };
        let steps = [
            step("10.0", Some("10.0.401"), "10.0.402"),
            step("9.0", Some("9.0.310"), "9.0.310"),
            step("lts", None, "10.0.402"),
            // Moved past, but another requirement still resolves to it.
            step("8.0.4xx", Some("8.0.420"), "8.0.426"),
        ];
        let needed: Vec<Item> = ["10.0.402", "9.0.310", "8.0.420", "8.0.426"]
            .iter()
            .map(|s| Item::Sdk(v(s)))
            .collect();
        assert_eq!(superseded(&steps, &needed), vec![Item::Sdk(v("10.0.401"))]);
    }

    #[test]
    fn a_step_says_what_it_does() {
        let step = |current: Option<&str>, target: Option<&str>| Step {
            requirement: Requirement::Explicit {
                selector: "10.0".into(),
            },
            component: None,
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
