//! HTTP, kept deliberately small: fetch a document, or stream a file to disk
//! while hashing it.

use std::io::{IsTerminal, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256, Sha512};

use crate::exit::{self, Failed};

const USER_AGENT: &str = concat!("dv/", env!("CARGO_PKG_VERSION"));

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build()
        .into()
}

pub fn fetch_text(url: &str) -> Result<String> {
    let mut response = agent()
        .get(url)
        .call()
        .map_err(|err| Failed::new(exit::NETWORK, format!("GET {url} failed: {err}")))?;
    response
        .body_mut()
        .read_to_string()
        .with_context(|| format!("could not read the response from {url}"))
}

/// The answer to a conditional GET.
pub enum Conditional {
    /// The copy the validators came from is still current.
    NotModified,
    /// A new body, with the validators to send next time.
    Modified {
        body: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// GET `url` unless it still matches `etag` / `last_modified`.
///
/// A 304 costs one small round trip instead of a megabyte of release
/// metadata, which is the difference between a version picker that opens
/// instantly and one that visibly loads.
pub fn fetch_conditional(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<Conditional> {
    let mut request = agent().get(url);
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    if let Some(last_modified) = last_modified {
        request = request.header("If-Modified-Since", last_modified);
    }
    let mut response = request
        .call()
        .map_err(|err| Failed::new(exit::NETWORK, format!("GET {url} failed: {err}")))?;
    if response.status() == 304 {
        return Ok(Conditional::NotModified);
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let (etag, last_modified) = (header("etag"), header("last-modified"));
    let body = response
        .body_mut()
        .read_to_string()
        .with_context(|| format!("could not read the response from {url}"))?;
    Ok(Conditional::Modified {
        body,
        etag,
        last_modified,
    })
}

/// Hashes of a file, computed in one pass.
///
/// Microsoft publishes sha512 for SDK archives, and dv's own releases carry a
/// sha256 sidecar — so a download is hashed both ways rather than read twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digests {
    pub sha256: String,
    pub sha512: String,
}

/// Stream `url` into `dest`, returning the hashes of what was written.
///
/// The download lands on a `.part` file and is only renamed into place once
/// the whole body has been read, so an interrupted download can never be
/// mistaken for a complete one.
pub fn download(url: &str, dest: &Path) -> Result<Digests> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let partial = dest.with_extension("part");

    let mut response = agent()
        .get(url)
        .call()
        .map_err(|err| Failed::new(exit::NETWORK, format!("GET {url} failed: {err}")))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());

    let mut reader = response.body_mut().as_reader();
    let mut file = std::fs::File::create(&partial)
        .with_context(|| format!("could not create {}", partial.display()))?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut written: u64 = 0;
    let mut progress = Progress::new(dest.file_name().unwrap_or_default().to_string_lossy());

    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("the download of {url} was interrupted"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .with_context(|| format!("could not write {}", partial.display()))?;
        written += read as u64;
        progress.update(written, total);
    }
    file.flush()?;
    drop(file);
    progress.finish(written);

    if let Some(total) = total
        && written != total
    {
        let _ = std::fs::remove_file(&partial);
        bail!("{url} ended early — got {written} bytes of {total}, try again");
    }

    std::fs::rename(&partial, dest).with_context(|| {
        format!(
            "could not move {} into place at {}",
            partial.display(),
            dest.display()
        )
    })?;
    Ok(hasher.finish())
}

pub fn digest_file(path: &Path) -> Result<Digests> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finish())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

struct Hasher {
    sha256: Sha256,
    sha512: Sha512,
}

impl Hasher {
    fn new() -> Self {
        Self {
            sha256: Sha256::new(),
            sha512: Sha512::new(),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.sha256.update(bytes);
        self.sha512.update(bytes);
    }

    fn finish(self) -> Digests {
        Digests {
            sha256: hex(self.sha256.finalize().as_slice()),
            sha512: hex(self.sha512.finalize().as_slice()),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A one-line, redraw-in-place byte counter. Silent when stderr is not a
/// terminal so CI logs stay readable.
struct Progress {
    label: String,
    interactive: bool,
    last_tick: u64,
}

impl Progress {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            interactive: std::io::stderr().is_terminal(),
            last_tick: 0,
        }
    }

    fn update(&mut self, written: u64, total: Option<u64>) {
        if !self.interactive {
            return;
        }
        // Redraw every megabyte rather than every chunk.
        if written - self.last_tick < 1024 * 1024 {
            return;
        }
        self.last_tick = written;
        let progress = match total {
            Some(total) => format!("{} / {}", megabytes(written), megabytes(total)),
            None => megabytes(written),
        };
        eprint!("\r  downloading {} {progress}   ", self.label);
        let _ = std::io::stderr().flush();
    }

    fn finish(&self, written: u64) {
        if self.interactive {
            // The older Windows console prints escape sequences literally
            // unless a program asks it not to, so the line is blanked there.
            if cfg!(windows) {
                eprint!("\r{:79}\r", "");
            } else {
                eprint!("\r\u{1b}[2K");
            }
        }
        eprintln!("  downloaded {} ({})", self.label, megabytes(written));
    }
}

pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_the_reference_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty");
        std::fs::write(&file, b"").unwrap();
        let digests = digest_file(&file).unwrap();
        assert_eq!(
            digests.sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digests.sha512,
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(sha256_file(&file).unwrap(), digests.sha256);
    }
}
