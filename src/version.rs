//! SDK versions and what a user can ask for.
//!
//! Two distinct things live here: a concrete `Version` (an SDK that is
//! installed or published) and a `Selector` (what the user asked for — `10`,
//! `10.0`, `10.0.4xx`, `10.0.402`, `latest`, `lts`).
//!
//! An SDK version is `major.minor.patch`, but the patch carries two numbers:
//! the hundreds digit is the *feature band* and the rest is the patch within
//! it. 10.0.402 is patch 2 of the 10.0.4xx band, and bands matter — a project
//! that builds on 10.0.1xx can break on 10.0.4xx, which is exactly why
//! `global.json` roll-forward policies are written in terms of them.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};

/// A concrete SDK version, prerelease tag included.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version(semver::Version);

impl Version {
    #[cfg(test)]
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self(semver::Version::new(major, minor, patch))
    }

    pub fn major(&self) -> u64 {
        self.0.major
    }

    pub fn minor(&self) -> u64 {
        self.0.minor
    }

    /// The feature band: 4 for 10.0.402.
    pub fn band(&self) -> u64 {
        self.0.patch / 100
    }

    /// The `10.0` channel this version belongs to.
    pub fn channel(&self) -> (u64, u64) {
        (self.0.major, self.0.minor)
    }

    pub fn is_prerelease(&self) -> bool {
        !self.0.pre.is_empty()
    }
}

impl FromStr for Version {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = normalize(s);
        match semver::Version::parse(s) {
            Ok(version) if version.build.is_empty() => Ok(Self(version)),
            _ => bail!("`{s}` is not an SDK version — expected one like 10.0.402"),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What was asked for, which is not always a concrete version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// The newest version available.
    Latest,
    /// The newest version of a long-term-support release.
    Lts,
    /// `10`: the newest 10.x.
    Major(u64),
    /// `10.0`: the newest SDK in that channel.
    Channel(u64, u64),
    /// `10.0.4xx`: the newest SDK in that feature band.
    Band(u64, u64, u64),
    /// `10.0.402`, or a prerelease like `11.0.100-rc.2.26504.105`.
    Exact(Version),
}

impl Selector {
    /// The best candidate this selector accepts: the newest release, or the
    /// newest prerelease when only prereleases match.
    ///
    /// Preferring releases is what makes `dv install 10.0` mean the shipped
    /// 10.0 even after a preview of the next patch appears. Falling back to
    /// prereleases is what makes `dv install 11` work before 11 ships, rather
    /// than reporting that a version visibly on the list does not exist.
    pub fn best<'a, I>(&self, candidates: I) -> Option<Version>
    where
        I: IntoIterator<Item = &'a Version>,
    {
        let matching: Vec<&Version> = candidates.into_iter().filter(|v| self.matches(v)).collect();
        matching
            .iter()
            .filter(|v| !v.is_prerelease())
            .max()
            .or_else(|| matching.iter().max())
            .map(|v| (*v).clone())
    }

    pub fn matches(&self, version: &Version) -> bool {
        match self {
            Selector::Latest => true,
            Selector::Lts => is_lts_major(version.major()),
            Selector::Major(major) => version.major() == *major,
            Selector::Channel(major, minor) => version.channel() == (*major, *minor),
            Selector::Band(major, minor, band) => {
                version.channel() == (*major, *minor) && version.band() == *band
            }
            Selector::Exact(exact) => version == exact,
        }
    }
}

/// Whether a major version is a long-term-support release.
///
/// Used only for installed SDKs, which carry no release metadata of their
/// own. Since .NET 5, even majors are LTS; before that, 2.1 and 3.1 were —
/// but those are majors 2 and 3, and nobody pins those by `lts`.
pub fn is_lts_major(major: u64) -> bool {
    major >= 6 && major.is_multiple_of(2)
}

impl FromStr for Selector {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = normalize(s);
        if s.eq_ignore_ascii_case("latest") {
            return Ok(Selector::Latest);
        }
        if s.eq_ignore_ascii_case("lts") {
            return Ok(Selector::Lts);
        }
        let numeric = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit());
        let parts: Vec<&str> = s.split('.').collect();
        match parts.as_slice() {
            [major] if numeric(major) => return Ok(Selector::Major(major.parse()?)),
            [major, minor] if numeric(major) && numeric(minor) => {
                return Ok(Selector::Channel(major.parse()?, minor.parse()?));
            }
            [major, minor, band]
                if numeric(major)
                    && numeric(minor)
                    && band.len() == 3
                    && band[1..].eq_ignore_ascii_case("xx")
                    && numeric(&band[..1]) =>
            {
                return Ok(Selector::Band(
                    major.parse()?,
                    minor.parse()?,
                    band[..1].parse()?,
                ));
            }
            _ => {}
        }
        match s.parse::<Version>() {
            Ok(version) => Ok(Selector::Exact(version)),
            Err(_) => bail!(
                "`{s}` is not an SDK version — try `10`, `10.0`, `10.0.4xx`, `10.0.402`, \
                 `latest` or `lts`"
            ),
        }
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Selector::Latest => write!(f, "latest"),
            Selector::Lts => write!(f, "lts"),
            Selector::Major(major) => write!(f, "{major}"),
            Selector::Channel(major, minor) => write!(f, "{major}.{minor}"),
            Selector::Band(major, minor, band) => write!(f, "{major}.{minor}.{band}xx"),
            Selector::Exact(v) => write!(f, "{v}"),
        }
    }
}

/// Strip anything that makes a version string not a version string.
fn normalize(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('v').unwrap_or(s).trim()
}

/// The newest `keep` versions of each feature band, oldest first.
///
/// Only ever used to trim what is *shown*. Everything published stays
/// installable — an exact `global.json` pin is the whole point of writing one,
/// and a listing that is convenient to read is not a reason to make a pin
/// unresolvable.
pub fn newest_per_band(versions: &[Version], keep: usize) -> Vec<Version> {
    let mut sorted = versions.to_vec();
    sorted.sort();

    let mut kept: Vec<Version> = Vec::new();
    let mut seen: Vec<((u64, u64, u64), usize)> = Vec::new();
    for version in sorted.iter().rev() {
        let band = (version.major(), version.minor(), version.band());
        let count = match seen.iter_mut().find(|(seen_band, _)| *seen_band == band) {
            Some((_, count)) => count,
            None => {
                seen.push((band, 0));
                &mut seen.last_mut().expect("just pushed").1
            }
        };
        if *count < keep {
            *count += 1;
            kept.push(version.clone());
        }
    }
    kept.sort();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version::new(major, minor, patch)
    }

    fn pre(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn parses_releases_and_prereleases() {
        assert_eq!("10.0.402".parse::<Version>().unwrap(), v(10, 0, 402));
        assert_eq!(" v8.0.426\n".parse::<Version>().unwrap(), v(8, 0, 426));
        let rc = pre("11.0.100-rc.2.26504.105");
        assert!(rc.is_prerelease());
        assert_eq!(rc.channel(), (11, 0));
        assert_eq!(rc.to_string(), "11.0.100-rc.2.26504.105");
    }

    #[test]
    fn rejects_nonsense() {
        assert!("10.0".parse::<Version>().is_err());
        assert!("10.0.402.1".parse::<Version>().is_err());
        assert!("ten".parse::<Version>().is_err());
        assert!("".parse::<Version>().is_err());
    }

    #[test]
    fn knows_its_feature_band() {
        let version = v(10, 0, 402);
        assert_eq!(version.band(), 4);
        assert_eq!(v(8, 0, 120).band(), 1);
    }

    #[test]
    fn orders_numerically_and_prereleases_first() {
        let mut all = vec![
            v(10, 0, 100),
            pre("10.0.100-rc.2.25502.107"),
            v(9, 0, 310),
            v(9, 0, 99),
            pre("10.0.100-preview.7.25380.108"),
        ];
        all.sort();
        assert_eq!(
            all,
            vec![
                v(9, 0, 99),
                v(9, 0, 310),
                pre("10.0.100-preview.7.25380.108"),
                pre("10.0.100-rc.2.25502.107"),
                v(10, 0, 100),
            ]
        );
    }

    #[test]
    fn selector_parsing() {
        assert_eq!("latest".parse::<Selector>().unwrap(), Selector::Latest);
        assert_eq!("LTS".parse::<Selector>().unwrap(), Selector::Lts);
        assert_eq!("10".parse::<Selector>().unwrap(), Selector::Major(10));
        assert_eq!(
            "10.0".parse::<Selector>().unwrap(),
            Selector::Channel(10, 0)
        );
        assert_eq!(
            "10.0.4xx".parse::<Selector>().unwrap(),
            Selector::Band(10, 0, 4)
        );
        assert_eq!(
            "10.0.4XX".parse::<Selector>().unwrap(),
            Selector::Band(10, 0, 4)
        );
        assert_eq!(
            "10.0.402".parse::<Selector>().unwrap(),
            Selector::Exact(v(10, 0, 402))
        );
        assert!("10.0.x".parse::<Selector>().is_err());
        assert!("ten".parse::<Selector>().is_err());
    }

    #[test]
    fn selectors_round_trip_through_display() {
        for text in ["latest", "lts", "10", "10.0", "10.0.4xx", "10.0.402"] {
            assert_eq!(text.parse::<Selector>().unwrap().to_string(), text);
        }
    }

    #[test]
    fn selector_prefers_releases_over_prereleases() {
        let all = [
            v(9, 0, 310),
            v(10, 0, 113),
            v(10, 0, 402),
            pre("11.0.100-rc.2.26504.105"),
        ];
        assert_eq!(Selector::Latest.best(&all), Some(v(10, 0, 402)));
        assert_eq!(Selector::Channel(10, 0).best(&all), Some(v(10, 0, 402)));
        assert_eq!(Selector::Band(10, 0, 1).best(&all), Some(v(10, 0, 113)));
        assert_eq!(Selector::Lts.best(&all), Some(v(10, 0, 402)));
        // Nothing but a prerelease matches, so the prerelease it is.
        assert_eq!(
            Selector::Major(11).best(&all),
            Some(pre("11.0.100-rc.2.26504.105"))
        );
        assert_eq!(Selector::Major(7).best(&all), None);
    }

    #[test]
    fn keeps_the_newest_of_every_band() {
        let all = [
            v(10, 0, 100),
            v(10, 0, 101),
            v(10, 0, 102),
            v(10, 0, 400),
            v(10, 0, 401),
            v(9, 0, 310),
        ];
        assert_eq!(
            newest_per_band(&all, 2),
            vec![
                v(9, 0, 310),
                v(10, 0, 101),
                v(10, 0, 102),
                v(10, 0, 400),
                v(10, 0, 401),
            ]
        );
        assert!(newest_per_band(&[], 2).is_empty());
    }
}
