//! Which SDK runs here.
//!
//! Resolution order, first match wins:
//!
//! 1. `DV_DOTNET_VERSION`
//! 2. the nearest `global.json`, when it names an SDK version — read with the
//!    `dotnet` host's own roll-forward rules
//! 3. the configured default
//! 4. the newest installed release (a preview only when nothing else is
//!    installed)
//!
//! Two rules hold everything together. Resolution is **read-only**: it never
//! installs, never writes, never touches the working directory, which is what
//! makes shims cheap and lets scripts ask "what would run here?". And it
//! **falls back rather than fails**: a directory with no pin resolves to the
//! newest installed SDK, because a version manager that refuses to run
//! `dotnet --info` is experienced as broken.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::exit::{self, Failed};
use crate::globaljson::{self, SdkRequest};
use crate::version::{Selector, Version};

/// The variable that overrides every file. Interface: the shell hook exports
/// it, and scripts set it.
pub const ENVIRONMENT: &str = "DV_DOTNET_VERSION";

/// Why this version was chosen. Carried through so `which` and `doctor` can
/// explain themselves — "10.0.402 (newest installed)" answers a question that
/// "10.0.402" leaves open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Environment,
    GlobalJson(PathBuf, SdkRequest),
    Default,
    NewestInstalled,
}

impl Source {
    /// A stable identifier for which resolution step answered.
    ///
    /// Deliberately not the Display text, which is written for people and may
    /// be reworded; these strings are interface and must not drift.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Environment => "environment",
            Source::GlobalJson(_, _) => "version-file",
            Source::Default => "default",
            Source::NewestInstalled => "newest-installed",
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Source::GlobalJson(path, _) => Some(path),
            _ => None,
        }
    }

    /// Whether a file or the environment chose this, rather than a fallback.
    pub fn is_pin(&self) -> bool {
        matches!(self, Source::Environment | Source::GlobalJson(_, _))
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Environment => write!(f, "{ENVIRONMENT}"),
            Source::GlobalJson(path, _) => write!(f, "{}", path.display()),
            Source::Default => write!(f, "dv default"),
            Source::NewestInstalled => write!(f, "newest installed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub version: Version,
    pub source: Source,
}

/// Everything resolution is allowed to look at. Passing it in keeps the
/// algorithm testable without a real `$HOME` or real installs.
pub struct Request<'a> {
    pub cwd: &'a Path,
    pub installed: &'a [Version],
    pub default: Option<&'a str>,
    pub environment: Option<&'a str>,
}

pub fn resolve(request: &Request<'_>) -> Result<Resolution> {
    // 1. Explicit override. An override that names an uninstalled version is
    //    an error, not a fall-through: the user said exactly what they wanted.
    if let Some(raw) = request.environment.map(str::trim).filter(|s| !s.is_empty()) {
        let selector: Selector = raw
            .parse()
            .with_context(|| format!("{ENVIRONMENT} is set to `{raw}`"))?;
        return pick(request, &selector, Source::Environment);
    }

    // 2. The nearest global.json. The host reads only the nearest one, so a
    //    file without a version — or one it would ignore as invalid — ends
    //    the search here rather than deferring to a parent directory.
    if let Some(file) = globaljson::nearest(request.cwd)?
        && let Ok(sdk) = &file.request
        && sdk.is_pin()
    {
        return match sdk.select(request.installed) {
            Some(version) => Ok(Resolution {
                version,
                source: Source::GlobalJson(file.path.clone(), sdk.clone()),
            }),
            None => Err(Failed::new(
                exit::NOT_INSTALLED,
                format!(
                    "{} requires .NET SDK {} and nothing installed satisfies it — run \
                     `dv install` here to install what it asks for",
                    file.path.display(),
                    sdk.describe(),
                ),
            )),
        };
    }

    // 3. The configured default.
    if let Some(raw) = request.default.map(str::trim).filter(|s| !s.is_empty()) {
        let selector: Selector = raw
            .parse()
            .with_context(|| format!("the dv default is set to `{raw}`"))?;
        return pick(request, &selector, Source::Default);
    }

    // 4. Newest installed. Releases first: installing a preview to try it
    //    should not quietly move every unpinned directory onto it.
    match Selector::Latest.best(request.installed) {
        Some(version) => Ok(Resolution {
            version,
            source: Source::NewestInstalled,
        }),
        None => Err(Failed::new(
            exit::NOTHING_INSTALLED,
            "no .NET SDK installed — run `dv install lts` to get one",
        )),
    }
}

fn pick(request: &Request<'_>, selector: &Selector, source: Source) -> Result<Resolution> {
    match selector.best(request.installed) {
        Some(version) => Ok(Resolution { version, source }),
        None => Err(Failed::new(
            exit::NOT_INSTALLED,
            format!(
                ".NET SDK {selector} is required by {source} but is not installed — \
                 run `dv install {selector}`"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    struct Fixture {
        dir: tempfile::TempDir,
        installed: Vec<Version>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                installed: vec![
                    v("8.0.426"),
                    v("9.0.105"),
                    v("9.0.111"),
                    v("10.0.402"),
                    v("11.0.100-rc.2.26504.105"),
                ],
            }
        }

        fn write(&self, relative: &str, body: &str) -> PathBuf {
            let path = self.dir.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            path
        }

        fn sub(&self, relative: &str) -> PathBuf {
            let path = self.dir.path().join(relative);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        fn request<'a>(&'a self, cwd: &'a Path, environment: Option<&'a str>) -> Request<'a> {
            Request {
                cwd,
                installed: &self.installed,
                default: None,
                environment,
            }
        }
    }

    #[test]
    fn environment_wins_over_everything() {
        let fixture = Fixture::new();
        fixture.write("global.json", r#"{"sdk":{"version":"9.0.105"}}"#);
        let resolution = resolve(&fixture.request(&fixture.sub(""), Some("8.0"))).unwrap();
        assert_eq!(resolution.version, v("8.0.426"));
        assert_eq!(resolution.source, Source::Environment);
    }

    #[test]
    fn global_json_uses_the_hosts_rules() {
        let fixture = Fixture::new();
        let file = fixture.write("global.json", r#"{"sdk":{"version":"9.0.100"}}"#);
        let resolution = resolve(&fixture.request(&fixture.sub("src/app"), None)).unwrap();
        // patch is the default: newest in the 9.0.1xx band.
        assert_eq!(resolution.version, v("9.0.111"));
        assert!(matches!(resolution.source, Source::GlobalJson(ref path, _) if *path == file));
        assert_eq!(resolution.source.kind(), "version-file");
        assert!(resolution.source.is_pin());
    }

    #[test]
    fn the_nearest_global_json_ends_the_search_even_without_a_version() {
        let fixture = Fixture::new();
        fixture.write("global.json", r#"{"sdk":{"version":"9.0.105"}}"#);
        fixture.write("tools/global.json", r#"{"msbuild-sdks":{}}"#);
        let resolution = resolve(&fixture.request(&fixture.sub("tools"), None)).unwrap();
        assert_eq!(resolution.version, v("10.0.402"));
        assert_eq!(resolution.source, Source::NewestInstalled);
    }

    #[test]
    fn an_invalid_global_json_is_ignored_like_the_host_ignores_it() {
        let fixture = Fixture::new();
        fixture.write("global.json", "{ not json");
        let resolution = resolve(&fixture.request(&fixture.sub(""), None)).unwrap();
        assert_eq!(resolution.source, Source::NewestInstalled);
    }

    #[test]
    fn pinned_but_missing_names_the_next_action() {
        let fixture = Fixture::new();
        fixture.write(
            "global.json",
            r#"{"sdk":{"version":"9.0.300","rollForward":"latestPatch"}}"#,
        );
        let error = resolve(&fixture.request(&fixture.sub(""), None)).unwrap_err();
        assert_eq!(exit::code_for(&error), exit::NOT_INSTALLED);
        let message = error.to_string();
        assert!(message.contains("dv install"), "{message}");
        assert!(
            message.contains("9.0.300 (rollForward: latestPatch)"),
            "{message}"
        );
    }

    #[test]
    fn default_is_used_before_newest() {
        let fixture = Fixture::new();
        let cwd = fixture.sub("");
        let mut request = fixture.request(&cwd, None);
        request.default = Some("8.0");
        let resolution = resolve(&request).unwrap();
        assert_eq!(resolution.version, v("8.0.426"));
        assert_eq!(resolution.source, Source::Default);
    }

    #[test]
    fn newest_installed_skips_previews() {
        let fixture = Fixture::new();
        let resolution = resolve(&fixture.request(&fixture.sub(""), None)).unwrap();
        assert_eq!(resolution.version, v("10.0.402"));
        assert!(!resolution.source.is_pin());
    }

    #[test]
    fn nothing_installed_names_the_next_action() {
        let fixture = Fixture::new();
        let request = Request {
            cwd: fixture.dir.path(),
            installed: &[],
            default: None,
            environment: None,
        };
        let error = resolve(&request).unwrap_err();
        assert_eq!(exit::code_for(&error), exit::NOTHING_INSTALLED);
        assert!(error.to_string().contains("dv install lts"));
    }
}
