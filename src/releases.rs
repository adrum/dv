//! Microsoft's release metadata: which SDKs exist, for which platforms, at
//! which URLs, with which hashes.
//!
//! dv builds nothing. Every SDK it installs is the archive Microsoft publishes,
//! described by two layers of JSON:
//!
//! ```text
//! releases-index.json            one entry per channel (10.0, 9.0, …)
//!   └─ <channel>/releases.json   every release in it, with each SDK's files
//! ```
//!
//! Each file carries a sha512, and verification **fails closed**: a file with
//! a missing, empty or malformed hash is an error, never a reason to skip the
//! check. Guarding the check with "if a hash is present" is a one-line change
//! that quietly disables integrity for any malformed entry.
//!
//! What the hash buys is protection of the archive given trustworthy metadata,
//! fetched over TLS from Microsoft. It does not defend against a compromise of
//! the metadata itself.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::exit::{self, Failed};
use crate::globaljson::SdkRequest;
use crate::metadata::{self, Freshness};
use crate::version::{Selector, Version};

/// Where the channel index is published, unless `DV_RELEASES_URL` or
/// `releases_url` in the config file says otherwise.
pub const DEFAULT_INDEX_URL: &str =
    "https://builds.dotnet.microsoft.com/dotnet/release-metadata/releases-index.json";

#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    #[serde(rename = "releases-index")]
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    #[serde(rename = "channel-version")]
    pub version: String,
    /// `preview`, `go-live`, `active`, `maintenance` or `eol`.
    #[serde(rename = "support-phase", default)]
    pub support_phase: String,
    /// `lts` or `sts`.
    #[serde(rename = "release-type", default)]
    pub release_type: String,
    #[serde(rename = "eol-date", default)]
    pub eol_date: Option<String>,
    #[serde(rename = "releases.json")]
    pub releases_url: String,
}

impl Channel {
    /// `(10, 0)` for the `10.0` channel.
    pub fn number(&self) -> Option<(u64, u64)> {
        let (major, minor) = self.version.split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    }

    pub fn is_eol(&self) -> bool {
        self.support_phase.eq_ignore_ascii_case("eol")
    }

    pub fn is_lts(&self) -> bool {
        self.release_type.eq_ignore_ascii_case("lts")
    }
}

/// One downloadable file of one SDK.
#[derive(Debug, Clone, Deserialize)]
pub struct File {
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub rid: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub url: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub hash: String,
}

impl File {
    /// The sha512, or an error explaining that the entry cannot be trusted.
    pub fn verified_sha512(&self) -> Result<&str> {
        let hash = self.hash.trim();
        if hash.is_empty() {
            return Err(Failed::new(
                exit::VERIFICATION,
                format!(
                    "the release metadata for {} has no hash — refusing to install an \
                     unverifiable archive",
                    self.url
                ),
            ));
        }
        if hash.len() != 128 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Failed::new(
                exit::VERIFICATION,
                format!(
                    "the release metadata for {} has a malformed sha512 (`{hash}`) — \
                     refusing to install an unverifiable archive",
                    self.url
                ),
            ));
        }
        Ok(hash)
    }

    /// The name the archive is cached under: the URL's own file name, which
    /// carries the version where `name` does not.
    pub fn cache_name(&self) -> String {
        self.url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.name)
            .to_string()
    }
}

/// An SDK as published.
#[derive(Debug, Clone)]
pub struct Sdk {
    pub version: Version,
    /// The `10.0` channel it belongs to.
    pub channel: String,
    pub release_date: Option<String>,
    pub runtime_version: Option<String>,
    pub files: Vec<File>,
}

impl Sdk {
    /// The archive for `rid`: a `.tar.gz`, or a `.zip` on Windows.
    pub fn archive_for(&self, rid: &str) -> Option<&File> {
        let extension = if rid.starts_with("win-") {
            ".zip"
        } else {
            ".tar.gz"
        };
        self.files
            .iter()
            .find(|file| file.rid == rid && file.name.ends_with(extension))
    }
}

#[derive(Debug, Deserialize)]
struct ChannelReleases {
    #[serde(default, deserialize_with = "null_as_default")]
    releases: Vec<Release>,
}

#[derive(Debug, Deserialize)]
struct Release {
    #[serde(rename = "release-date", default)]
    release_date: Option<String>,
    #[serde(default)]
    sdk: Option<RawSdk>,
    #[serde(default, deserialize_with = "null_as_default")]
    sdks: Vec<RawSdk>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSdk {
    version: String,
    #[serde(rename = "runtime-version", default)]
    runtime_version: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    files: Vec<File>,
}

/// Read `null` as the field's default.
///
/// Microsoft's older metadata writes `null` where newer files omit a field —
/// `"sdks": null` throughout 3.0, `"rid": null` on some 2.1 and 2.2 files —
/// and `#[serde(default)]` alone covers only the missing case.
fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn fetch_index(url: &str, freshness: Freshness) -> Result<Index> {
    let body = metadata::fetch(url, freshness)
        .with_context(|| format!("could not fetch the .NET release index from {url}"))?;
    serde_json::from_str(&body).with_context(|| format!("the release index at {url} is not usable"))
}

/// Every SDK published in a channel, oldest first.
pub fn fetch_channel(channel: &Channel, freshness: Freshness) -> Result<Vec<Sdk>> {
    let url = &channel.releases_url;
    let body = metadata::fetch(url, freshness).with_context(|| {
        format!(
            "could not fetch the {} releases from {url}",
            channel.version
        )
    })?;
    parse_channel(&channel.version, &body)
        .with_context(|| format!("the {} releases at {url} are not usable", channel.version))
}

/// Fetch several channels at once, each result in the order given.
///
/// Channels are independent megabyte-sized documents, so fetching them in
/// turn makes a cold listing cost the sum of every round trip rather than
/// the slowest one.
pub fn fetch_channels(channels: &[&Channel], freshness: Freshness) -> Vec<Result<Vec<Sdk>>> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = channels
            .iter()
            .map(|channel| scope.spawn(move || fetch_channel(channel, freshness)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("fetching release metadata panicked")))
            })
            .collect()
    })
}

fn parse_channel(channel: &str, body: &str) -> Result<Vec<Sdk>> {
    let releases: ChannelReleases = serde_json::from_str(body)?;
    let mut sdks: Vec<Sdk> = Vec::new();
    for release in releases.releases {
        // `sdks` lists every SDK a release shipped, one per feature band; the
        // older single `sdk` field is all some early releases have.
        let raw = if release.sdks.is_empty() {
            release.sdk.into_iter().collect()
        } else {
            release.sdks
        };
        for raw in raw {
            let Ok(version) = raw.version.parse::<Version>() else {
                continue;
            };
            if sdks.iter().any(|sdk| sdk.version == version) {
                continue;
            }
            sdks.push(Sdk {
                version,
                channel: channel.to_string(),
                release_date: release.release_date.clone(),
                runtime_version: raw.runtime_version,
                files: raw.files,
            });
        }
    }
    sdks.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(sdks)
}

/// The channels worth fetching to answer a selector, newest first.
///
/// Each channel's metadata is a megabyte or more, so this asks for the ones
/// that can hold an answer rather than all of them.
pub fn channels_for<'a>(index: &'a Index, selector: &Selector) -> Vec<&'a Channel> {
    let mut channels: Vec<&Channel> = index
        .channels
        .iter()
        .filter(|channel| {
            let Some((major, minor)) = channel.number() else {
                return false;
            };
            match selector {
                Selector::Latest => !channel.is_eol(),
                Selector::Lts => channel.is_lts() && !channel.is_eol(),
                Selector::Major(m) => major == *m,
                Selector::Channel(m, n) | Selector::Band(m, n, _) => (major, minor) == (*m, *n),
                Selector::Exact(version) => (major, minor) == version.channel(),
            }
        })
        .collect();
    sort_newest_first(&mut channels);
    channels
}

/// The channels that can satisfy a `global.json` request, newest first.
pub fn channels_for_request<'a>(index: &'a Index, request: &SdkRequest) -> Vec<&'a Channel> {
    let mut channels: Vec<&Channel> = index
        .channels
        .iter()
        .filter(|channel| {
            let Some(number) = channel.number() else {
                return false;
            };
            match &request.version {
                // No version: the newest supported SDK.
                None => !channel.is_eol(),
                Some(version) => number >= version.channel(),
            }
        })
        .collect();
    sort_newest_first(&mut channels);
    channels
}

fn sort_newest_first(channels: &mut [&Channel]) {
    channels.sort_by_key(|channel| std::cmp::Reverse(channel.number()));
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{"releases-index":[
      {"channel-version":"11.0","latest-sdk":"11.0.100-rc.2.26504.105","support-phase":"go-live","release-type":"sts","releases.json":"https://example.test/11.0/releases.json"},
      {"channel-version":"10.0","latest-sdk":"10.0.402","support-phase":"active","release-type":"lts","releases.json":"https://example.test/10.0/releases.json"},
      {"channel-version":"9.0","latest-sdk":"9.0.319","support-phase":"maintenance","release-type":"sts","releases.json":"https://example.test/9.0/releases.json"},
      {"channel-version":"6.0","latest-sdk":"6.0.428","support-phase":"eol","release-type":"lts","releases.json":"https://example.test/6.0/releases.json"}
    ]}"#;

    fn hash(c: char) -> String {
        c.to_string().repeat(128)
    }

    fn channel_body() -> String {
        format!(
            r#"{{"releases":[
              {{"release-date":"2026-10-13","sdk":{{"version":"10.0.402"}},"sdks":[
                {{"version":"10.0.402","runtime-version":"10.0.13","files":[
                  {{"name":"dotnet-sdk-osx-arm64.pkg","rid":"osx-arm64","url":"https://example.test/dotnet-sdk-10.0.402-osx-arm64.pkg","hash":"{a}"}},
                  {{"name":"dotnet-sdk-osx-arm64.tar.gz","rid":"osx-arm64","url":"https://example.test/dotnet-sdk-10.0.402-osx-arm64.tar.gz","hash":"{b}"}},
                  {{"name":"dotnet-sdk-win-x64.zip","rid":"win-x64","url":"https://example.test/dotnet-sdk-10.0.402-win-x64.zip","hash":"{a}"}}
                ]}},
                {{"version":"10.0.113","files":[]}}
              ]}},
              {{"release-date":"2026-09-09","sdk":{{"version":"10.0.401","files":[]}}}}
            ]}}"#,
            a = hash('a'),
            b = hash('b'),
        )
    }

    #[test]
    fn collects_every_sdk_in_every_release() {
        let sdks = parse_channel("10.0", &channel_body()).unwrap();
        let versions: Vec<String> = sdks.iter().map(|sdk| sdk.version.to_string()).collect();
        assert_eq!(versions, ["10.0.113", "10.0.401", "10.0.402"]);
        assert_eq!(sdks[2].runtime_version.as_deref(), Some("10.0.13"));
        assert_eq!(sdks[1].release_date.as_deref(), Some("2026-09-09"));
    }

    #[test]
    fn picks_the_archive_not_the_installer() {
        let sdks = parse_channel("10.0", &channel_body()).unwrap();
        let newest = sdks.last().unwrap();
        let file = newest.archive_for("osx-arm64").unwrap();
        assert!(file.name.ends_with(".tar.gz"));
        assert_eq!(file.cache_name(), "dotnet-sdk-10.0.402-osx-arm64.tar.gz");
        assert!(
            newest
                .archive_for("win-x64")
                .unwrap()
                .name
                .ends_with(".zip")
        );
        assert!(newest.archive_for("linux-x64").is_none());
    }

    #[test]
    fn nulls_in_older_metadata_read_as_absent() {
        // The shapes .NET Core 2.1, 2.2 and 3.0 metadata actually contain.
        let body = format!(
            r#"{{"releases":[
              {{"release-date":null,"sdks":null,"sdk":{{"version":"3.0.100-preview6-012264","files":[
                {{"name":"dotnet-sdk-osx-x64.tar.gz","rid":"osx-x64","url":"https://example.test/a.tar.gz","hash":"{a}"}},
                {{"name":"dotnet-sdk.zip","rid":null,"url":"https://example.test/b.zip","hash":null}}
              ]}}}},
              {{"sdk":{{"version":"3.0.103","files":null}}}}
            ]}}"#,
            a = hash('a'),
        );
        let sdks = parse_channel("3.0", &body).unwrap();
        assert_eq!(sdks.len(), 2);
        assert!(sdks[0].archive_for("osx-x64").is_some());
        assert!(sdks[1].files.is_empty());
    }

    #[test]
    fn verification_fails_closed_on_a_bad_hash() {
        let file = |hash: &str| File {
            name: "dotnet-sdk-osx-arm64.tar.gz".into(),
            rid: "osx-arm64".into(),
            url: "https://example.test/x".into(),
            hash: hash.into(),
        };
        assert!(file("").verified_sha512().is_err());
        assert!(file("   ").verified_sha512().is_err());
        assert!(file("abc123").verified_sha512().is_err());
        assert!(file(&"z".repeat(128)).verified_sha512().is_err());
        assert!(file(&"a".repeat(64)).verified_sha512().is_err());
        assert!(file(&"a".repeat(128)).verified_sha512().is_ok());
    }

    #[test]
    fn fetches_only_the_channels_that_can_answer() {
        let index: Index = serde_json::from_str(INDEX).unwrap();
        let names = |selector: &str| -> Vec<String> {
            channels_for(&index, &selector.parse().unwrap())
                .iter()
                .map(|channel| channel.version.clone())
                .collect()
        };
        assert_eq!(names("latest"), ["11.0", "10.0", "9.0"]);
        assert_eq!(names("lts"), ["10.0"]);
        assert_eq!(names("9"), ["9.0"]);
        assert_eq!(names("6.0.428"), ["6.0"]);
        assert_eq!(names("10.0.4xx"), ["10.0"]);
    }

    #[test]
    fn a_request_fetches_its_channel_and_newer() {
        let index: Index = serde_json::from_str(INDEX).unwrap();
        let request = crate::globaljson::parse(r#"{"sdk":{"version":"9.0.100"}}"#).unwrap();
        let names: Vec<&str> = channels_for_request(&index, &request)
            .iter()
            .map(|channel| channel.version.as_str())
            .collect();
        assert_eq!(names, ["11.0", "10.0", "9.0"]);
    }
}
