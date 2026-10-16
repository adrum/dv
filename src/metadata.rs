//! A local cache of Microsoft's release metadata.
//!
//! Listing what can be installed means reading the channel index and a
//! megabyte or more of `releases.json` per channel. Fetching all of it on
//! every `dv list --remote` makes a version picker that calls dv on each
//! visit visibly slow, for data that changes a few times a month.
//!
//! So metadata is kept in `cache/metadata/`, with the validators the server
//! sent. A copy younger than the caller's limit is used as-is; an older one
//! is revalidated with a conditional request, which costs one small round
//! trip when nothing changed. If the network is unreachable, a cached copy is
//! used anyway — with a warning, never silently.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::exit::{self, Failed};
use crate::net::{self, Conditional};
use crate::paths;
use crate::signature;

/// How old cached metadata may be before it is checked with the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Use a copy younger than this without asking.
    MaxAge(Duration),
    /// Always ask the server whether the copy is current.
    Revalidate,
    /// Never ask: answer from the cache whatever its age, or fail. For a
    /// caller that shows cached data at once and refreshes in its own
    /// background, rather than dv leaving a process behind to do it.
    CacheOnly,
}

/// How long `dv list --remote` trusts cached metadata. Microsoft publishes
/// a few times a month; ten minutes keeps a picker instant without hiding a
/// release for long. `--refresh` skips it, and installs always revalidate.
pub const LIST_MAX_AGE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    /// Unix seconds when the server last confirmed or sent this copy.
    checked_at: u64,
    /// The signature file this copy was verified against. A copy without one
    /// — cached before dv verified signatures — is never used.
    #[serde(default)]
    signature: String,
}

/// The oldest moment any metadata used by this process was confirmed with
/// the server, in Unix seconds — `u64::MAX` until something is fetched.
///
/// A listing is assembled from several documents of different ages, and the
/// honest answer to "how current is this?" is the oldest of them. Kept as
/// process state because a listing is one short-lived command, and threading
/// an age through every caller of `fetch` would buy nothing else.
static OLDEST_CHECK: AtomicU64 = AtomicU64::new(u64::MAX);

/// When the oldest metadata behind this command was last confirmed current.
pub fn oldest_check() -> Option<u64> {
    match OLDEST_CHECK.load(Ordering::Relaxed) {
        u64::MAX => None,
        at => Some(at),
    }
}

fn note_check(at: u64) {
    OLDEST_CHECK.fetch_min(at, Ordering::Relaxed);
}

pub fn dir() -> Result<PathBuf> {
    Ok(paths::cache_dir()?.join("metadata"))
}

/// The cache files for one URL.
struct Files {
    body: PathBuf,
    record: PathBuf,
    signature: PathBuf,
}

/// A cached copy whose signature verified just now.
struct Cached {
    body: String,
    record: Record,
    expires_at: u64,
}

/// The body at `url` — from the cache when `freshness` allows it — after
/// verifying Microsoft's signature on it.
pub fn fetch(url: &str, freshness: Freshness) -> Result<String> {
    let dir = dir()?;
    let files = files_for(&dir, url);
    let cached = read_cached(&files, url);
    let now = crate::installs::now_unix();

    if freshness == Freshness::CacheOnly {
        return match cached {
            Some(cached) => {
                warn_if_expired(url, cached.expires_at, now);
                note_check(cached.record.checked_at);
                Ok(cached.body)
            }
            None => Err(Failed::new(
                exit::NETWORK,
                "nothing verified is cached yet — run `dv list --remote` once with a \
                 network connection",
            )),
        };
    }

    // A recent copy is used as-is — unless Microsoft's signature on it has
    // expired, in which case the server is asked for a newer one.
    if let Some(cached) = &cached
        && let Freshness::MaxAge(limit) = freshness
        && now.saturating_sub(cached.record.checked_at) < limit.as_secs()
        && now <= cached.expires_at
    {
        note_check(cached.record.checked_at);
        return Ok(cached.body.clone());
    }

    let validators = cached.as_ref().map(|cached| {
        (
            cached.record.etag.as_deref(),
            cached.record.last_modified.as_deref(),
        )
    });
    let (etag, last_modified) = validators.unwrap_or((None, None));
    match net::fetch_conditional(url, etag, last_modified) {
        Ok(Conditional::NotModified) if cached.is_some() => {
            let Cached {
                body,
                mut record,
                expires_at,
            } = cached.expect("checked above");
            // The server says this is still current; Microsoft's signature
            // must say so too.
            ensure_current(url, expires_at, now)?;
            record.checked_at = now;
            // Best effort: failing to note the check only means the next
            // call asks again.
            let _ = write_atomic(&files.record, &serde_json::to_vec(&record)?);
            note_check(now);
            Ok(body)
        }
        // A 304 with nothing cached cannot happen unless validators were
        // sent; fetch again plainly rather than trust it.
        Ok(Conditional::NotModified) => {
            let body = net::fetch_text(url)?;
            accept(&dir, &files, url, body, None, None, now)
        }
        Ok(Conditional::Modified {
            body,
            etag,
            last_modified,
        }) => accept(&dir, &files, url, body, etag, last_modified, now),
        Err(err) => match cached {
            Some(cached) => {
                eprintln!(
                    "dv: could not reach {url} ({err:#}) — using release metadata cached {} ago",
                    describe_age(now.saturating_sub(cached.record.checked_at))
                );
                warn_if_expired(url, cached.expires_at, now);
                note_check(cached.record.checked_at);
                Ok(cached.body)
            }
            None => Err(err),
        },
    }
}

/// Verify a body fresh from the server, then cache it with its signature.
fn accept(
    dir: &Path,
    files: &Files,
    url: &str,
    body: String,
    etag: Option<String>,
    last_modified: Option<String>,
    now: u64,
) -> Result<String> {
    let refuse = || format!("refusing the release metadata at {url}");
    let reference = signature::reference(&body).with_context(refuse)?;
    let signature_url = sibling(url, &reference.file);
    let signature = net::fetch_bytes(&signature_url)
        .with_context(|| format!("could not fetch the signature for {url}"))?;
    signature::verify(body.as_bytes(), &signature).with_context(refuse)?;
    ensure_current(url, reference.expires_at, now)?;

    store(
        dir,
        files,
        &body,
        &signature,
        Record {
            url: url.to_string(),
            etag,
            last_modified,
            checked_at: now,
            signature: reference.file,
        },
    );
    note_check(now);
    Ok(body)
}

/// Refuse metadata the server hands out after Microsoft's signature on it
/// expired: correctly signed but stale, which is exactly what a replay of old
/// metadata — hiding newer releases — looks like.
fn ensure_current(url: &str, expires_at: u64, now: u64) -> Result<()> {
    if now <= expires_at {
        return Ok(());
    }
    Err(Failed::new(
        exit::VERIFICATION,
        format!(
            "Microsoft's signature on the release metadata at {url} expired on {} — the \
             server is handing out stale metadata, so dv will not trust it; try again later",
            signature::format_date(expires_at)
        ),
    ))
}

/// A cached copy is local, already verified, and better than nothing when
/// the network is unreachable — but not silently.
fn warn_if_expired(url: &str, expires_at: u64, now: u64) {
    if now > expires_at {
        eprintln!(
            "dv: Microsoft's signature on the cached release metadata for {url} expired on {} \
             — newer releases may exist",
            signature::format_date(expires_at)
        );
    }
}

/// The URL of a file beside `url`.
fn sibling(url: &str, file: &str) -> String {
    match url.rsplit_once('/') {
        Some((directory, _)) => format!("{directory}/{file}"),
        None => file.to_string(),
    }
}

/// Cache file names for a URL: readable, and unique per URL. The hash keeps
/// two mirrors' `releases.json` apart; the tail keeps `ls` meaningful.
fn files_for(dir: &Path, url: &str) -> Files {
    use sha2::{Digest, Sha256};
    let hash = net::hex(&Sha256::digest(url.as_bytes())[..6]);
    let tail: String = url
        .trim_end_matches('/')
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = format!("{hash}-{tail}");
    Files {
        body: dir.join(&stem),
        record: dir.join(format!("{stem}.dv-meta.json")),
        signature: dir.join(format!("{stem}.p7s")),
    }
}

/// The cached copy for `url`, if there is one and it verifies again now.
///
/// Verified on every read rather than trusted for having been verified when
/// stored: it costs a millisecond, and it means a cache edited by hand — or
/// by something else — is noticed instead of believed.
fn read_cached(files: &Files, url: &str) -> Option<Cached> {
    let record: Record = serde_json::from_slice(&std::fs::read(&files.record).ok()?).ok()?;
    if record.url != url || record.signature.is_empty() {
        return None;
    }
    let body = std::fs::read_to_string(&files.body).ok()?;
    let signature = std::fs::read(&files.signature).ok()?;
    let verified = signature::reference(&body)
        .ok()
        .filter(|reference| reference.file == record.signature)
        .and_then(|reference| {
            signature::verify(body.as_bytes(), &signature)
                .ok()
                .map(|()| reference)
        });
    match verified {
        Some(reference) => Some(Cached {
            body,
            record,
            expires_at: reference.expires_at,
        }),
        None => {
            eprintln!(
                "dv: the cached release metadata for {url} does not verify against Microsoft's \
                 signature — ignoring it"
            );
            None
        }
    }
}

fn store(dir: &Path, files: &Files, body: &str, signature: &[u8], record: Record) {
    // Caching is an optimization: a read-only or full disk must not turn a
    // successful fetch into a failed command. The record goes last, so a
    // half-written cache entry is never mistaken for a complete one.
    let result = (|| -> Result<()> {
        paths::ensure_dir(dir)?;
        write_atomic(&files.body, body.as_bytes())?;
        write_atomic(&files.signature, signature)?;
        write_atomic(&files.record, &serde_json::to_vec(&record)?)
    })();
    if let Err(err) = result {
        eprintln!("dv: could not cache release metadata: {err:#}");
    }
}

/// Write beside the target and rename in, so two dv processes listing at
/// once never read half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let staging = path.with_extension(format!("{}.incoming", std::process::id()));
    std::fs::write(&staging, bytes)
        .with_context(|| format!("could not write {}", staging.display()))?;
    std::fs::rename(&staging, path)
        .with_context(|| format!("could not move {} into place", staging.display()))
}

fn describe_age(seconds: u64) -> String {
    match seconds {
        s if s < 120 => format!("{s} seconds"),
        s if s < 2 * 3600 => format!("{} minutes", s / 60),
        s if s < 2 * 86400 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86400),
    }
}

/// Remove all cached metadata, returning how many files went and their size.
pub fn clear() -> Result<(usize, u64)> {
    let dir = dir()?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok((0, 0));
    };
    let (mut removed, mut freed) = (0, 0);
    for entry in entries.flatten() {
        let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
            freed += bytes;
        }
    }
    Ok((removed, freed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = include_str!("testdata/releases-index.json");
    const INDEX_SIGNATURE: &[u8] = include_bytes!("testdata/releases-index.json.p7s");
    const URL: &str =
        "https://builds.dotnet.microsoft.com/dotnet/release-metadata/releases-index.json";

    #[test]
    fn cache_files_are_unique_per_url_and_readable() {
        let dir = Path::new("/cache");
        let a = files_for(
            dir,
            "https://builds.dotnet.microsoft.com/dotnet/release-metadata/10.0/releases.json",
        );
        let b = files_for(
            dir,
            "https://mirror.example.test/dotnet/release-metadata/10.0/releases.json",
        );
        assert_ne!(a.body, b.body, "two hosts' files must not collide");
        let name = a.body.file_name().unwrap().to_str().unwrap();
        assert!(name.ends_with("-10.0-releases.json"), "{name}");
        assert!(a.record.to_str().unwrap().ends_with(".dv-meta.json"));
        assert!(a.signature.to_str().unwrap().ends_with(".p7s"));
    }

    fn cache_with(dir: &Path, body: &str, record_url: &str, signature_file: &str) -> Files {
        let files = files_for(dir, URL);
        std::fs::write(&files.body, body).unwrap();
        std::fs::write(&files.signature, INDEX_SIGNATURE).unwrap();
        let record = Record {
            url: record_url.into(),
            etag: None,
            last_modified: None,
            checked_at: 1,
            signature: signature_file.into(),
        };
        std::fs::write(&files.record, serde_json::to_vec(&record).unwrap()).unwrap();
        files
    }

    #[test]
    fn a_verified_cached_copy_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let reference = signature::reference(INDEX).unwrap();
        let files = cache_with(dir.path(), INDEX, URL, &reference.file);
        let cached = read_cached(&files, URL).unwrap();
        assert_eq!(cached.expires_at, reference.expires_at);
    }

    #[test]
    fn a_cached_copy_that_does_not_verify_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let reference = signature::reference(INDEX).unwrap();

        let files = cache_with(dir.path(), INDEX, "https://other.test/x", &reference.file);
        assert!(read_cached(&files, URL).is_none(), "another URL's record");

        let tampered = INDEX.replacen("10.0", "10.9", 1);
        let files = cache_with(dir.path(), &tampered, URL, &reference.file);
        assert!(read_cached(&files, URL).is_none(), "an edited body");

        let files = cache_with(dir.path(), INDEX, URL, "");
        assert!(
            read_cached(&files, URL).is_none(),
            "cached before signatures"
        );
    }

    #[test]
    fn stale_signed_metadata_is_refused_from_the_network() {
        let error = ensure_current(URL, 100, 101).unwrap_err();
        assert_eq!(exit::code_for(&error), exit::VERIFICATION);
        assert!(ensure_current(URL, 100, 100).is_ok());
    }

    #[test]
    fn signatures_live_beside_their_files() {
        assert_eq!(
            sibling(
                "https://builds.dotnet.microsoft.com/dotnet/release-metadata/10.0/releases.json",
                "releases.json.20261013194811.p7s"
            ),
            "https://builds.dotnet.microsoft.com/dotnet/release-metadata/10.0/releases.json.20261013194811.p7s"
        );
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(describe_age(30), "30 seconds");
        assert_eq!(describe_age(600), "10 minutes");
        assert_eq!(describe_age(3 * 3600), "3 hours");
        assert_eq!(describe_age(3 * 86400), "3 days");
    }
}
