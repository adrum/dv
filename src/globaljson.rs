//! `global.json`: the file .NET itself reads to choose an SDK.
//!
//! dv does not invent a version file. `global.json` is what the `dotnet` host
//! reads, what Visual Studio, Rider and CI images honor, and what teams
//! already commit — so it is what `dv pin` writes and what resolution reads.
//!
//! The selection rules here mirror the host's own resolver
//! (`src/native/corehost/fxr/sdk_resolver.cpp` in dotnet/runtime) rather than
//! the documentation's summary of it, because the two differ in the details
//! that decide which SDK runs:
//!
//! - only the **nearest** `global.json` counts; one without `sdk.version`
//!   stops the search rather than deferring to a parent
//! - a pinned version with no `rollForward` uses `patch`
//! - `disable` accepts the exact version and nothing else
//! - `patch` and `feature` (and `minor`, `major`) prefer the *lowest* matching
//!   feature band and the newest patch within it; the `latest*` policies take
//!   the newest match outright
//! - a file that fails to parse is ignored with a warning, not fatal
//!
//! Getting these right matters more than usual: dv runs each SDK from an
//! install root that holds only that SDK, so if dv's choice and the host's
//! disagreed the host would refuse to start rather than quietly pick another.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::version::Version;

pub const FILE: &str = "global.json";

/// `sdk.rollForward`, named exactly as the host names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollForward {
    Disable,
    Patch,
    Feature,
    Minor,
    Major,
    LatestPatch,
    LatestFeature,
    LatestMinor,
    LatestMajor,
}

impl RollForward {
    const ALL: [(RollForward, &'static str); 9] = [
        (RollForward::Disable, "disable"),
        (RollForward::Patch, "patch"),
        (RollForward::Feature, "feature"),
        (RollForward::Minor, "minor"),
        (RollForward::Major, "major"),
        (RollForward::LatestPatch, "latestPatch"),
        (RollForward::LatestFeature, "latestFeature"),
        (RollForward::LatestMinor, "latestMinor"),
        (RollForward::LatestMajor, "latestMajor"),
    ];

    /// Parsed case-insensitively, as the host does.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, known)| known.eq_ignore_ascii_case(name.trim()))
            .map(|(policy, _)| *policy)
    }

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(policy, _)| *policy == self)
            .map(|(_, name)| *name)
            .expect("every policy is named")
    }

    fn uses_latest(self) -> bool {
        matches!(
            self,
            RollForward::LatestPatch
                | RollForward::LatestFeature
                | RollForward::LatestMinor
                | RollForward::LatestMajor
        )
    }
}

impl std::fmt::Display for RollForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// What a `global.json` asks of the SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkRequest {
    pub version: Option<Version>,
    pub roll_forward: RollForward,
    pub allow_prerelease: bool,
}

impl Default for SdkRequest {
    /// What the host does with no `global.json` at all: newest SDK, previews
    /// included.
    fn default() -> Self {
        Self {
            version: None,
            roll_forward: RollForward::LatestMajor,
            allow_prerelease: true,
        }
    }
}

impl SdkRequest {
    /// Whether this request names a version. Only that counts as a pin: a file
    /// with no version and default policy asks for nothing a fallback would
    /// not already give.
    pub fn is_pin(&self) -> bool {
        self.version.is_some()
    }

    /// Whether `candidate` is acceptable at all — the host's `matches_policy`.
    pub fn matches(&self, candidate: &Version) -> bool {
        if !self.allow_prerelease && candidate.is_prerelease() {
            return false;
        }
        let Some(requested) = &self.version else {
            return true;
        };
        if self.roll_forward == RollForward::Disable {
            return candidate == requested;
        }
        let same_major = candidate.major() == requested.major();
        let same_minor = same_major && candidate.minor() == requested.minor();
        let same_band = same_minor && candidate.band() == requested.band();
        let in_scope = match self.roll_forward {
            RollForward::Disable => unreachable!("handled above"),
            RollForward::Patch | RollForward::LatestPatch => same_band,
            RollForward::Feature | RollForward::LatestFeature => same_minor,
            RollForward::Minor | RollForward::LatestMinor => same_major,
            RollForward::Major | RollForward::LatestMajor => true,
        };
        in_scope && candidate >= requested
    }

    /// The SDK the host would choose from `candidates`, if any.
    pub fn select<'a, I>(&self, candidates: I) -> Option<Version>
    where
        I: IntoIterator<Item = &'a Version>,
    {
        let candidates: Vec<&Version> = candidates.into_iter().collect();

        // `disable` and `patch` take the exact version first when it is there.
        if let Some(requested) = &self.version
            && matches!(self.roll_forward, RollForward::Disable | RollForward::Patch)
            && candidates.contains(&requested)
        {
            return Some(requested.clone());
        }

        let mut best: Option<&Version> = None;
        for candidate in candidates.into_iter().filter(|c| self.matches(c)) {
            if best.is_none_or(|previous| self.is_better(candidate, previous)) {
                best = Some(candidate);
            }
        }
        best.cloned()
    }

    /// The host's `is_better_match`, for two versions that both match.
    fn is_better(&self, current: &Version, previous: &Version) -> bool {
        let same_band = current.major() == previous.major()
            && current.minor() == previous.minor()
            && current.band() == previous.band();
        if self.version.is_none() || self.roll_forward.uses_latest() || same_band {
            current > previous
        } else {
            current < previous
        }
    }

    /// A short statement of the request, for messages: `10.0.402
    /// (rollForward: latestFeature)`.
    pub fn describe(&self) -> String {
        match &self.version {
            Some(version) => format!("{version} (rollForward: {})", self.roll_forward),
            None => format!("any SDK (rollForward: {})", self.roll_forward),
        }
    }
}

/// A `global.json` on disk and what it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalJson {
    pub path: PathBuf,
    /// The SDK request, or why the host will ignore the file.
    pub request: std::result::Result<SdkRequest, String>,
}

/// The `global.json` the host would read from `cwd`: the nearest one, walking
/// up to the filesystem root.
pub fn find(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(FILE))
        .find(|file| file.is_file())
}

/// Find and read the nearest `global.json`.
pub fn nearest(cwd: &Path) -> Result<Option<GlobalJson>> {
    let Some(path) = find(cwd) else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    Ok(Some(GlobalJson {
        request: parse(&raw),
        path,
    }))
}

/// Read the SDK request out of a `global.json` body, or explain why the host
/// would ignore it.
pub fn parse(raw: &str) -> std::result::Result<SdkRequest, String> {
    let json: serde_json::Value = serde_json::from_str(&strip_comments(raw))
        .map_err(|err| format!("not valid JSON: {err}"))?;

    let mut request = SdkRequest::default();
    let sdk = match json.get("sdk") {
        None | Some(serde_json::Value::Null) => return Ok(request),
        Some(serde_json::Value::Object(sdk)) => sdk,
        Some(_) => return Err("`sdk` is not an object".into()),
    };

    match sdk.get("version") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(raw)) => {
            let version: Version = raw
                .parse()
                .map_err(|_| format!("`sdk.version` is not a valid version: `{raw}`"))?;
            request.version = Some(version);
            request.roll_forward = RollForward::Patch;
        }
        Some(_) => return Err("`sdk.version` is not a string".into()),
    }

    match sdk.get("rollForward") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(raw)) => {
            request.roll_forward = RollForward::parse(raw)
                .ok_or_else(|| format!("`sdk.rollForward` is not a known policy: `{raw}`"))?;
            if request.roll_forward != RollForward::LatestMajor && request.version.is_none() {
                return Err(format!(
                    "`sdk.rollForward` is `{raw}`, which requires `sdk.version`"
                ));
            }
        }
        Some(_) => return Err("`sdk.rollForward` is not a string".into()),
    }

    match sdk.get("allowPrerelease") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Bool(allow)) => request.allow_prerelease = *allow,
        Some(_) => return Err("`sdk.allowPrerelease` is not a boolean".into()),
    }
    // A prerelease pin always allows prereleases, whatever the file says.
    if request.version.as_ref().is_some_and(Version::is_prerelease) {
        request.allow_prerelease = true;
    }
    Ok(request)
}

/// Write `sdk.version` and `sdk.rollForward` into `path`, keeping everything
/// else in the file — `msbuild-sdks`, `test`, other `sdk` keys — as it was.
///
/// Returns whether comments had to be dropped: `global.json` allows them, a
/// JSON writer cannot keep them, and silently losing one is worse than
/// saying so.
pub fn write_pin(path: &Path, version: &Version, roll_forward: RollForward) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(raw) => Some(raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("could not read {}", path.display())),
    };

    let mut document = match &existing {
        Some(raw) => match serde_json::from_str::<serde_json::Value>(&strip_comments(raw)) {
            Ok(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
            _ => bail!(
                "{} is not a JSON object — fix it by hand, or delete it and pin again",
                path.display()
            ),
        },
        None => serde_json::json!({}),
    };
    let had_comments = existing
        .as_deref()
        .is_some_and(|raw| strip_comments(raw) != raw);

    let root = document.as_object_mut().expect("checked above");
    let sdk = root.entry("sdk").or_insert_with(|| serde_json::json!({}));
    if !sdk.is_object() {
        *sdk = serde_json::json!({});
    }
    let sdk = sdk.as_object_mut().expect("just made an object");
    sdk.insert("version".into(), version.to_string().into());
    sdk.insert("rollForward".into(), roll_forward.name().into());

    let body = serde_json::to_string_pretty(&document)?;
    std::fs::write(path, format!("{body}\n"))
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(had_comments)
}

/// Remove `//` and `/* */` comments outside strings, which the host accepts
/// and a strict JSON parser does not.
fn strip_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    fn request(version: &str, roll_forward: RollForward) -> SdkRequest {
        SdkRequest {
            version: Some(v(version)),
            roll_forward,
            allow_prerelease: true,
        }
    }

    fn installed() -> Vec<Version> {
        [
            "8.0.120", "8.0.426", "9.0.105", "9.0.111", "9.0.203", "9.0.310", "10.0.113",
            "10.0.402",
        ]
        .iter()
        .map(|s| v(s))
        .collect()
    }

    #[test]
    fn a_version_alone_means_patch() {
        let parsed = parse(r#"{"sdk":{"version":"9.0.105"}}"#).unwrap();
        assert_eq!(parsed.roll_forward, RollForward::Patch);
        assert_eq!(parsed.version, Some(v("9.0.105")));
        assert!(parsed.is_pin());
    }

    #[test]
    fn no_sdk_section_is_the_default_request() {
        assert_eq!(parse("{}").unwrap(), SdkRequest::default());
        assert_eq!(
            parse(r#"{"msbuild-sdks":{"Foo":"1.0"}}"#).unwrap(),
            SdkRequest::default()
        );
        assert!(!SdkRequest::default().is_pin());
    }

    #[test]
    fn invalid_files_say_why() {
        assert!(parse("{ nope").unwrap_err().contains("JSON"));
        assert!(
            parse(r#"{"sdk":{"version":"ten"}}"#)
                .unwrap_err()
                .contains("ten")
        );
        assert!(
            parse(r#"{"sdk":{"version":"9.0.100","rollForward":"sideways"}}"#)
                .unwrap_err()
                .contains("sideways")
        );
        assert!(
            parse(r#"{"sdk":{"rollForward":"latestFeature"}}"#)
                .unwrap_err()
                .contains("requires")
        );
        // latestMajor alone is the one policy that needs no version.
        assert!(parse(r#"{"sdk":{"rollForward":"latestMajor"}}"#).is_ok());
    }

    #[test]
    fn comments_are_allowed() {
        let parsed = parse(
            "{\n  // the SDK CI uses\n  \"sdk\": { /* exact */ \"version\": \"9.0.105\", \
             \"url\": \"http://example.test/x\" }\n}",
        )
        .unwrap();
        assert_eq!(parsed.version, Some(v("9.0.105")));
    }

    #[test]
    fn policy_names_are_case_insensitive() {
        assert_eq!(
            RollForward::parse("LATESTFEATURE"),
            Some(RollForward::LatestFeature)
        );
        assert_eq!(RollForward::LatestFeature.name(), "latestFeature");
    }

    #[test]
    fn disable_takes_the_exact_version_or_nothing() {
        let all = installed();
        assert_eq!(
            request("9.0.105", RollForward::Disable).select(&all),
            Some(v("9.0.105"))
        );
        assert_eq!(request("9.0.106", RollForward::Disable).select(&all), None);
    }

    #[test]
    fn patch_prefers_exact_then_newest_in_band() {
        let all = installed();
        assert_eq!(
            request("9.0.105", RollForward::Patch).select(&all),
            Some(v("9.0.105"))
        );
        assert_eq!(
            request("9.0.100", RollForward::Patch).select(&all),
            Some(v("9.0.111"))
        );
        // Never across a band.
        assert_eq!(request("9.0.112", RollForward::Patch).select(&all), None);
    }

    #[test]
    fn latest_patch_takes_the_newest_in_band() {
        let all = installed();
        assert_eq!(
            request("9.0.105", RollForward::LatestPatch).select(&all),
            Some(v("9.0.111"))
        );
    }

    #[test]
    fn feature_takes_the_lowest_band_and_its_newest_patch() {
        let all = installed();
        assert_eq!(
            request("9.0.100", RollForward::Feature).select(&all),
            Some(v("9.0.111"))
        );
        // Nothing left in 1xx at or above 9.0.112, so it rolls to 2xx.
        assert_eq!(
            request("9.0.112", RollForward::Feature).select(&all),
            Some(v("9.0.203"))
        );
        assert_eq!(
            request("9.0.100", RollForward::LatestFeature).select(&all),
            Some(v("9.0.310"))
        );
    }

    #[test]
    fn minor_and_major_stay_low_unless_latest() {
        let all = installed();
        assert_eq!(
            request("8.0.400", RollForward::Minor).select(&all),
            Some(v("8.0.426"))
        );
        assert_eq!(
            request("8.0.500", RollForward::Major).select(&all),
            Some(v("9.0.111"))
        );
        assert_eq!(
            request("8.0.100", RollForward::LatestMajor).select(&all),
            Some(v("10.0.402"))
        );
        assert_eq!(
            request("8.0.100", RollForward::LatestMinor).select(&all),
            Some(v("8.0.426"))
        );
    }

    #[test]
    fn prereleases_are_excluded_when_asked() {
        let mut all = installed();
        all.push(v("11.0.100-rc.2.26504.105"));
        let mut latest = SdkRequest::default();
        assert_eq!(latest.select(&all), Some(v("11.0.100-rc.2.26504.105")));
        latest.allow_prerelease = false;
        assert_eq!(latest.select(&all), Some(v("10.0.402")));

        // A prerelease pin forces prereleases back on.
        let parsed = parse(
            r#"{"sdk":{"version":"11.0.100-rc.1.1","allowPrerelease":false,"rollForward":"latestFeature"}}"#,
        )
        .unwrap();
        assert_eq!(parsed.select(&all), Some(v("11.0.100-rc.2.26504.105")));
    }

    #[test]
    fn nearest_file_wins_even_without_a_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"sdk":{"version":"9.0.105"}}"#).unwrap();
        let nested = dir.path().join("src/app");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join("src").join(FILE), "{}").unwrap();

        let found = nearest(&nested).unwrap().unwrap();
        assert_eq!(found.path, dir.path().join("src").join(FILE));
        assert!(!found.request.unwrap().is_pin());
    }

    #[test]
    fn pinning_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(
            &path,
            r#"{"msbuild-sdks":{"Microsoft.Build.Traversal":"4.1.0"},"sdk":{"version":"8.0.100","allowPrerelease":false}}"#,
        )
        .unwrap();

        let dropped = write_pin(&path, &v("10.0.402"), RollForward::LatestFeature).unwrap();
        assert!(!dropped);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["sdk"]["version"], "10.0.402");
        assert_eq!(json["sdk"]["rollForward"], "latestFeature");
        assert_eq!(json["sdk"]["allowPrerelease"], false);
        assert_eq!(json["msbuild-sdks"]["Microsoft.Build.Traversal"], "4.1.0");
        // Key order survives, so the diff a pin produces is just the pin.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.find("msbuild-sdks").unwrap() < body.find("\"sdk\"").unwrap());
    }

    #[test]
    fn pinning_reports_dropped_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, "{\n  // keep me\n  \"sdk\": {}\n}\n").unwrap();
        assert!(write_pin(&path, &v("10.0.402"), RollForward::Disable).unwrap());
    }

    #[test]
    fn pinning_creates_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        write_pin(&path, &v("10.0.402"), RollForward::Disable).unwrap();
        let parsed = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed, request("10.0.402", RollForward::Disable));
    }
}
