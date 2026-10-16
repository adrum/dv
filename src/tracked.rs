//! What the user asked for, as distinct from what is installed.
//!
//! `dv install 10.0` installs 10.0.402 today, but what was asked for is "the
//! newest 10.0", and next month that is 10.0.403. Recording the request is
//! what lets `dv update` move it forward, and what tells dv which installed
//! SDKs are still wanted and which an update left behind.
//!
//! Requirements live in `tracked.json` in dv's home, plain and readable:
//!
//! - **explicit** — a version or line named on the command line: `10.0`,
//!   `lts`, or an exact `10.0.402`, which is a pin and never moves
//! - **global-json** — a `global.json` that `dv install` was run against,
//!   re-read on every update so an edited pin is followed and a deleted one
//!   is dropped
//! - **existing** — an SDK installed before dv tracked anything, kept as an
//!   exact pin so upgrading dv never deletes something by surprise
//!
//! The `dv default` setting is a requirement too, read from the config file
//! rather than stored here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::globaljson::{self, SdkRequest};
use crate::installs;
use crate::paths;
use crate::version::{Selector, Version};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum Requirement {
    Explicit {
        selector: String,
    },
    GlobalJson {
        path: PathBuf,
    },
    Existing {
        version: String,
    },
    /// The configured default. Never stored: it comes from `config.toml`.
    #[serde(skip)]
    Default {
        selector: String,
    },
}

impl Requirement {
    /// Stable machine name of where the requirement came from. Interface.
    pub fn source(&self) -> &'static str {
        match self {
            Requirement::Explicit { .. } => "explicit",
            Requirement::GlobalJson { .. } => "global-json",
            Requirement::Existing { .. } => "existing",
            Requirement::Default { .. } => "default",
        }
    }

    pub fn source_path(&self) -> Option<&Path> {
        match self {
            Requirement::GlobalJson { path } => Some(path),
            _ => None,
        }
    }

    /// What a person reads: `10.0`, `lts`, a path.
    pub fn label(&self) -> String {
        match self {
            Requirement::Explicit { selector } | Requirement::Default { selector } => {
                selector.clone()
            }
            Requirement::GlobalJson { path } => path.display().to_string(),
            Requirement::Existing { version } => version.clone(),
        }
    }

    /// What the requirement asks for today, or why it no longer asks for
    /// anything — a `global.json` deleted, emptied of its pin, or broken.
    pub fn want(&self) -> std::result::Result<Want, String> {
        match self {
            Requirement::Explicit { selector } | Requirement::Default { selector } => selector
                .parse()
                .map(Want::Selector)
                .map_err(|err| err.to_string()),
            Requirement::Existing { version } => version
                .parse()
                .map(|v| Want::Selector(Selector::Exact(v)))
                .map_err(|err: anyhow::Error| err.to_string()),
            Requirement::GlobalJson { path } => {
                let raw =
                    std::fs::read_to_string(path).map_err(|_| "it no longer exists".to_string())?;
                let request = globaljson::parse(&raw)
                    .map_err(|reason| format!("it is not usable: {reason}"))?;
                if !request.is_pin() {
                    return Err("it no longer pins an SDK".to_string());
                }
                Ok(Want::Request(request))
            }
        }
    }

    /// Whether the requirement is an exact pin, which an update never moves.
    pub fn is_pinned(&self) -> bool {
        matches!(self.want(), Ok(Want::Selector(Selector::Exact(_))))
    }
}

/// A requirement, read and ready to match against versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    Selector(Selector),
    Request(SdkRequest),
}

impl Want {
    /// The installed version this is satisfied by: what the `dotnet` host
    /// would pick for a `global.json`, the best match for a selector.
    pub fn satisfied_by(&self, installed: &[Version]) -> Option<Version> {
        match self {
            Want::Selector(selector) => selector.best(installed),
            Want::Request(request) => request.select(installed),
        }
    }

    /// The newest published version that satisfies this — releases first, so
    /// a request that allows previews does not turn every update into one.
    pub fn newest_published(&self, published: &[Version]) -> Option<Version> {
        match self {
            Want::Selector(selector) => selector.best(published),
            Want::Request(request) => {
                let releases: Vec<&Version> =
                    published.iter().filter(|v| !v.is_prerelease()).collect();
                request
                    .select(releases)
                    .or_else(|| request.select(published))
            }
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Tracked {
    pub requirements: Vec<Requirement>,
}

fn path() -> Result<PathBuf> {
    Ok(paths::home()?.join("tracked.json"))
}

impl Tracked {
    /// The tracked requirements, creating them on first use.
    ///
    /// The first time, every SDK already installed becomes an exact pin. dv
    /// cannot know what the user meant by an install it never recorded, and
    /// the only safe reading is "keep this one".
    pub fn load() -> Result<Self> {
        let path = path()?;
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).with_context(|| {
                format!(
                    "{} is not valid — fix it by hand, or delete it and dv will track \
                     what is installed now",
                    path.display()
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let tracked = Self {
                    requirements: installs::installed()?
                        .into_iter()
                        .map(|version| Requirement::Existing {
                            version: version.to_string(),
                        })
                        .collect(),
                };
                if !tracked.requirements.is_empty() {
                    tracked.save()?;
                }
                Ok(tracked)
            }
            Err(err) => Err(err).with_context(|| format!("could not read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = path()?;
        paths::ensure_dir(&paths::home()?)?;
        let body = serde_json::to_string_pretty(self)?;
        let staging = path.with_extension("json.incoming");
        std::fs::write(&staging, format!("{body}\n"))
            .with_context(|| format!("could not write {}", staging.display()))?;
        std::fs::rename(&staging, &path)
            .with_context(|| format!("could not move {} into place", staging.display()))
    }

    /// Record a requirement, once.
    pub fn add(&mut self, requirement: Requirement) {
        if !self.requirements.contains(&requirement) {
            self.requirements.push(requirement);
        }
    }

    /// Stop tracking what `dv uninstall <selector>` removed: the requirement
    /// named the same way, and any exact pin on the version itself.
    pub fn forget(&mut self, selector: &Selector, removed: &Version) -> Vec<Requirement> {
        let named = selector.to_string();
        let exact = removed.to_string();
        let (gone, kept): (Vec<_>, Vec<_>) =
            self.requirements
                .drain(..)
                .partition(|requirement| match requirement {
                    Requirement::Explicit { selector } => *selector == named || *selector == exact,
                    Requirement::Existing { version } => *version == exact,
                    _ => false,
                });
        self.requirements = kept;
        gone
    }

    /// Every requirement, with the configured default appended.
    pub fn with_default(&self, default: Option<&str>) -> Vec<Requirement> {
        let mut all = self.requirements.clone();
        if let Some(selector) = default.map(str::trim).filter(|s| !s.is_empty()) {
            all.push(Requirement::Default {
                selector: selector.to_string(),
            });
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    fn explicit(selector: &str) -> Requirement {
        Requirement::Explicit {
            selector: selector.into(),
        }
    }

    #[test]
    fn requirements_are_plain_json() {
        let tracked = Tracked {
            requirements: vec![
                explicit("10.0"),
                Requirement::GlobalJson {
                    path: "/code/app/global.json".into(),
                },
                Requirement::Existing {
                    version: "8.0.426".into(),
                },
            ],
        };
        let json = serde_json::to_string(&tracked).unwrap();
        assert!(
            json.contains(r#"{"source":"explicit","selector":"10.0"}"#),
            "{json}"
        );
        assert!(json.contains(r#""source":"global-json""#), "{json}");
        let back: Tracked = serde_json::from_str(&json).unwrap();
        assert_eq!(back.requirements, tracked.requirements);
    }

    #[test]
    fn a_line_moves_and_an_exact_version_does_not() {
        let published = [v("10.0.401"), v("10.0.402"), v("11.0.100-rc.2.26504.105")];
        let line = explicit("10.0").want().unwrap();
        assert_eq!(line.newest_published(&published), Some(v("10.0.402")));
        assert!(!explicit("10.0").is_pinned());

        let exact = explicit("10.0.401").want().unwrap();
        assert_eq!(exact.newest_published(&published), Some(v("10.0.401")));
        assert!(explicit("10.0.401").is_pinned());
    }

    #[test]
    fn a_global_json_requirement_follows_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("global.json");
        let requirement = Requirement::GlobalJson { path: path.clone() };
        assert!(requirement.want().unwrap_err().contains("no longer exists"));

        std::fs::write(
            &path,
            r#"{"sdk":{"version":"9.0.100","rollForward":"latestFeature"}}"#,
        )
        .unwrap();
        let want = requirement.want().unwrap();
        let published = [v("9.0.105"), v("9.0.310"), v("10.0.402")];
        assert_eq!(want.newest_published(&published), Some(v("9.0.310")));
        assert_eq!(want.satisfied_by(&[v("9.0.105")]), Some(v("9.0.105")));

        std::fs::write(&path, r#"{"msbuild-sdks":{}}"#).unwrap();
        assert!(requirement.want().unwrap_err().contains("no longer pins"));
    }

    #[test]
    fn a_request_prefers_releases_even_when_previews_are_allowed() {
        let want = Want::Request(SdkRequest::default());
        let published = [v("10.0.402"), v("11.0.100-rc.2.26504.105")];
        assert_eq!(want.newest_published(&published), Some(v("10.0.402")));
    }

    #[test]
    fn forgetting_removes_the_named_requirement_and_exact_pins_only() {
        let mut tracked = Tracked {
            requirements: vec![
                explicit("10.0"),
                explicit("10.0.402"),
                Requirement::Existing {
                    version: "10.0.402".into(),
                },
                explicit("lts"),
            ],
        };
        let gone = tracked.forget(&"10.0".parse().unwrap(), &v("10.0.402"));
        assert_eq!(gone.len(), 3);
        assert_eq!(tracked.requirements, vec![explicit("lts")]);
    }

    #[test]
    fn adding_twice_tracks_once() {
        let mut tracked = Tracked::default();
        tracked.add(explicit("10.0"));
        tracked.add(explicit("10.0"));
        assert_eq!(tracked.requirements.len(), 1);
    }
}
