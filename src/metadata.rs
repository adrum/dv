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

/// The body at `url`, from the cache when `freshness` allows it.
pub fn fetch(url: &str, freshness: Freshness) -> Result<String> {
    let dir = dir()?;
    let (body_path, record_path) = files_for(&dir, url);
    let cached = read_cached(&body_path, &record_path, url);
    let now = crate::installs::now_unix();

    if freshness == Freshness::CacheOnly {
        return match cached {
            Some((body, record)) => {
                note_check(record.checked_at);
                Ok(body)
            }
            None => Err(Failed::new(
                exit::NETWORK,
                "nothing is cached yet — run `dv list --remote` once with a network \
                 connection",
            )),
        };
    }

    if let Some((body, record)) = &cached
        && let Freshness::MaxAge(limit) = freshness
        && now.saturating_sub(record.checked_at) < limit.as_secs()
    {
        note_check(record.checked_at);
        return Ok(body.clone());
    }

    let validators = cached
        .as_ref()
        .map(|(_, record)| (record.etag.as_deref(), record.last_modified.as_deref()));
    let (etag, last_modified) = validators.unwrap_or((None, None));
    match net::fetch_conditional(url, etag, last_modified) {
        Ok(Conditional::NotModified) if cached.is_some() => {
            let (body, mut record) = cached.expect("checked above");
            record.checked_at = now;
            // Best effort: failing to note the check only means the next
            // call asks again.
            let _ = write_atomic(&record_path, &serde_json::to_vec(&record)?);
            note_check(now);
            Ok(body)
        }
        // A 304 with nothing cached cannot happen unless validators were
        // sent; fetch again plainly rather than trust it.
        Ok(Conditional::NotModified) => {
            note_check(now);
            net::fetch_text(url)
        }
        Ok(Conditional::Modified {
            body,
            etag,
            last_modified,
        }) => {
            store(
                &dir,
                &body_path,
                &record_path,
                url,
                &body,
                etag,
                last_modified,
                now,
            );
            note_check(now);
            Ok(body)
        }
        Err(err) => match cached {
            Some((body, record)) => {
                eprintln!(
                    "dv: could not reach {url} ({err:#}) — using release metadata cached {} ago",
                    describe_age(now.saturating_sub(record.checked_at))
                );
                note_check(record.checked_at);
                Ok(body)
            }
            None => Err(err),
        },
    }
}

/// Cache file names for a URL: readable, and unique per URL. The hash keeps
/// two mirrors' `releases.json` apart; the tail keeps `ls` meaningful.
fn files_for(dir: &Path, url: &str) -> (PathBuf, PathBuf) {
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
    (dir.join(&stem), dir.join(format!("{stem}.dv-meta.json")))
}

fn read_cached(body_path: &Path, record_path: &Path, url: &str) -> Option<(String, Record)> {
    let record: Record = serde_json::from_slice(&std::fs::read(record_path).ok()?).ok()?;
    if record.url != url {
        return None;
    }
    let body = std::fs::read_to_string(body_path).ok()?;
    Some((body, record))
}

#[allow(clippy::too_many_arguments)]
fn store(
    dir: &Path,
    body_path: &Path,
    record_path: &Path,
    url: &str,
    body: &str,
    etag: Option<String>,
    last_modified: Option<String>,
    now: u64,
) {
    // Caching is an optimization: a read-only or full disk must not turn a
    // successful fetch into a failed command.
    let result = (|| -> Result<()> {
        paths::ensure_dir(dir)?;
        write_atomic(body_path, body.as_bytes())?;
        let record = Record {
            url: url.to_string(),
            etag,
            last_modified,
            checked_at: now,
        };
        write_atomic(record_path, &serde_json::to_vec(&record)?)
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

    #[test]
    fn cache_files_are_unique_per_url_and_readable() {
        let dir = Path::new("/cache");
        let (a, a_meta) = files_for(
            dir,
            "https://builds.dotnet.microsoft.com/dotnet/release-metadata/10.0/releases.json",
        );
        let (b, _) = files_for(
            dir,
            "https://mirror.example.test/dotnet/release-metadata/10.0/releases.json",
        );
        assert_ne!(a, b, "two hosts' files must not collide");
        let name = a.file_name().unwrap().to_str().unwrap();
        assert!(name.ends_with("-10.0-releases.json"), "{name}");
        assert!(
            a_meta
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with(".dv-meta.json")
        );
    }

    #[test]
    fn a_record_for_another_url_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let body = dir.path().join("body");
        let record = dir.path().join("record");
        std::fs::write(&body, "{}").unwrap();
        std::fs::write(
            &record,
            r#"{"url":"https://a.test/x","etag":null,"last_modified":null,"checked_at":1}"#,
        )
        .unwrap();
        assert!(read_cached(&body, &record, "https://a.test/x").is_some());
        assert!(read_cached(&body, &record, "https://b.test/x").is_none());
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(describe_age(30), "30 seconds");
        assert_eq!(describe_age(600), "10 minutes");
        assert_eq!(describe_age(3 * 3600), "3 hours");
        assert_eq!(describe_age(3 * 86400), "3 days");
    }
}
