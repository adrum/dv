//! Runtime-only installs: a shared framework without an SDK.
//!
//! An SDK already brings the runtime it was built with, and every install
//! root links every runtime dv has — so most machines never need this. It is
//! for the rest: an app that targets a runtime no installed SDK carries, a
//! server that runs apps and builds none, a test matrix across runtimes.
//!
//! A runtime archive is laid out like an SDK archive without `sdk/`, `packs/`
//! and `templates/` — the host, `host/fxr`, and `shared/<framework>/<version>`
//! — so it is extracted as-is into `runtimes/<component>-<version>/` and
//! linked into roots exactly like an SDK's own runtimes.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};

use crate::paths;
use crate::version::{Selector, Version};

/// Which shared framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Component {
    /// Microsoft.NETCore.App — what every app needs.
    Runtime,
    /// Microsoft.AspNetCore.App, which ships with the base runtime it needs.
    AspNetCore,
    /// Microsoft.WindowsDesktop.App, published for Windows only.
    WindowsDesktop,
}

impl Component {
    pub const ALL: [Component; 3] = [
        Component::Runtime,
        Component::AspNetCore,
        Component::WindowsDesktop,
    ];

    /// The name used on the command line and in directory names. Interface.
    pub fn name(self) -> &'static str {
        match self {
            Component::Runtime => "runtime",
            Component::AspNetCore => "aspnetcore",
            Component::WindowsDesktop => "windowsdesktop",
        }
    }

    /// The archive's file name for a platform, exactly — the same file lists
    /// carry installers, app-host packs, targeting packs and "composite"
    /// builds, none of which are this.
    pub fn archive_name(self, rid: &str) -> String {
        let extension = if rid.starts_with("win-") {
            "zip"
        } else {
            "tar.gz"
        };
        let prefix = match self {
            Component::Runtime => "dotnet-runtime",
            Component::AspNetCore => "aspnetcore-runtime",
            Component::WindowsDesktop => "windowsdesktop-runtime",
        };
        format!("{prefix}-{rid}.{extension}")
    }

    /// The `shared/` directory the framework lives in.
    pub fn framework(self) -> &'static str {
        match self {
            Component::Runtime => "Microsoft.NETCore.App",
            Component::AspNetCore => "Microsoft.AspNetCore.App",
            Component::WindowsDesktop => "Microsoft.WindowsDesktop.App",
        }
    }

    /// What people call it, for messages.
    pub fn display(self) -> &'static str {
        match self {
            Component::Runtime => ".NET runtime",
            Component::AspNetCore => "ASP.NET Core runtime",
            Component::WindowsDesktop => "Windows Desktop runtime",
        }
    }
}

impl FromStr for Component {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "runtime" | "dotnet" | "netcore" => Ok(Component::Runtime),
            "aspnetcore" | "aspnet" => Ok(Component::AspNetCore),
            "windowsdesktop" | "desktop" => Ok(Component::WindowsDesktop),
            other => bail!(
                "`{other}` is not a runtime — try `runtime`, `aspnetcore` or `windowsdesktop`"
            ),
        }
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// `aspnetcore@8.0`, or a bare `8.0` for the base runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub component: Component,
    pub selector: Selector,
}

impl FromStr for Spec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (component, version) = match s.trim().split_once('@') {
            Some((component, version)) => (component.parse()?, version),
            None => (Component::Runtime, s.trim()),
        };
        let selector: Selector = version.parse()?;
        if matches!(selector, Selector::Band(..)) {
            bail!(
                "runtimes have no feature bands — use a line such as `{component}@8.0`, or an \
                 exact version"
            );
        }
        Ok(Spec {
            component,
            selector,
        })
    }
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.component, self.selector)
    }
}

/// An installed runtime.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Installed {
    pub component: Component,
    pub version: Version,
}

impl Installed {
    pub fn dir(&self) -> Result<PathBuf> {
        dir(self.component, &self.version)
    }
}

impl fmt::Display for Installed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.component, self.version)
    }
}

pub fn runtimes_dir() -> Result<PathBuf> {
    Ok(paths::home()?.join("runtimes"))
}

pub fn dir(component: Component, version: &Version) -> Result<PathBuf> {
    Ok(runtimes_dir()?.join(format!("{component}-{version}")))
}

/// Installed runtimes, oldest first within each component.
///
/// A directory only counts when it holds the host and the framework it is
/// named for; a half-extracted one is not an install.
pub fn installed() -> Result<Vec<Installed>> {
    let dir = runtimes_dir()?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", dir.display())),
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some((component, version)) = name.split_once('-') else {
            continue;
        };
        let (Ok(component), Ok(version)) =
            (component.parse::<Component>(), version.parse::<Version>())
        else {
            continue;
        };
        if is_complete(&entry.path(), component, &version) {
            found.push(Installed { component, version });
        }
    }
    found.sort();
    Ok(found)
}

pub fn is_complete(tree: &std::path::Path, component: Component, version: &Version) -> bool {
    tree.join(crate::platform::exe("dotnet")).is_file()
        && tree
            .join("shared")
            .join(component.framework())
            .join(version.to_string())
            .is_dir()
}

/// Versions of one component that are installed.
pub fn versions_of(installed: &[Installed], component: Component) -> Vec<Version> {
    installed
        .iter()
        .filter(|runtime| runtime.component == component)
        .map(|runtime| runtime.version.clone())
        .collect()
}

pub fn remove(runtime: &Installed) -> Result<()> {
    let dir = runtime.dir()?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_like_dotnetup_writes_them() {
        let spec: Spec = "aspnetcore@8.0".parse().unwrap();
        assert_eq!(spec.component, Component::AspNetCore);
        assert_eq!(spec.selector, Selector::Channel(8, 0));

        let bare: Spec = "10.0.13".parse().unwrap();
        assert_eq!(bare.component, Component::Runtime);
        assert_eq!(bare.to_string(), "runtime@10.0.13");

        assert_eq!(
            "aspnet@lts".parse::<Spec>().unwrap().component,
            Component::AspNetCore
        );
        assert_eq!(
            "desktop@8".parse::<Spec>().unwrap().component,
            Component::WindowsDesktop
        );
        assert!("aspnetcore@8.0.1xx".parse::<Spec>().is_err());
        assert!("blazor@8.0".parse::<Spec>().is_err());
    }

    #[test]
    fn archive_names_are_exact() {
        assert_eq!(
            Component::AspNetCore.archive_name("osx-arm64"),
            "aspnetcore-runtime-osx-arm64.tar.gz"
        );
        assert_eq!(
            Component::Runtime.archive_name("win-x64"),
            "dotnet-runtime-win-x64.zip"
        );
        assert_eq!(
            Component::WindowsDesktop.archive_name("win-arm64"),
            "windowsdesktop-runtime-win-arm64.zip"
        );
    }
}
