//! Exit codes, for callers that are programs rather than people.
//!
//! dv is driven by other software — a site manager asking which SDK a project
//! wants, a CI script, an editor plugin. Those callers need to tell "the user
//! has not installed 9.0 yet" (install it) from "the network is down" (retry)
//! from "the checksum did not match" (stop, loudly). Collapsing all of them
//! into 1 makes that impossible, and parsing the message text instead turns
//! every reworded sentence into a breaking change.
//!
//! **These numbers are interface.** They are asserted as literals in the tests
//! below, so changing one fails a test rather than silently breaking a caller.
//!
//! Only failures whose kind is actually known carry a code. Everything else
//! exits 1.

use std::fmt;

/// Anything without a more specific code.
pub const FAILURE: u8 = 1;
/// Bad arguments. clap exits with this itself; it is declared here so the
/// taxonomy is complete in one place and the test below pins it.
#[allow(dead_code)]
pub const USAGE: u8 = 2;

/// Resolved to a version, but it is not installed.
pub const NOT_INSTALLED: u8 = 10;
/// Nothing is installed at all, so there is nothing to fall back to.
pub const NOTHING_INSTALLED: u8 = 11;
/// No such version is published for this platform.
pub const NOT_PUBLISHED: u8 = 12;
/// Connection, DNS, TLS, or a non-success response.
pub const NETWORK: u8 = 13;
/// A checksum mismatched, or was missing where one is required.
pub const VERIFICATION: u8 = 14;
/// Another dv process holds the lock. Retryable, and nothing is wrong.
pub const BUSY: u8 = 15;

/// An error that knows how it should exit.
///
/// `Display` writes only the message, so `{err:#}` output is unchanged and the
/// code never leaks into what a person reads.
#[derive(Debug)]
pub struct Failed {
    pub code: u8,
    pub message: String,
}

impl Failed {
    /// Build a labeled error ready to return.
    ///
    /// Returns `anyhow::Error` rather than `Self` on purpose: every call site
    /// is a `return Err(...)`, and handing back `Self` would make each one
    /// wrap it by hand for no gain.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(code: u8, message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self {
            code,
            message: message.into(),
        })
    }
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failed {}

/// The code to exit with for this error.
///
/// Walks the whole chain, so a `Failed` wrapped in later `.context()` still
/// reports its own code.
pub fn code_for(error: &anyhow::Error) -> u8 {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<Failed>())
        .map_or(FAILURE, |failed| failed.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_numbers_are_interface_and_must_not_drift() {
        // Written as literals on purpose: importing the constants would make
        // this test agree with any change to them, which is the opposite of
        // what it is for.
        assert_eq!(FAILURE, 1);
        assert_eq!(USAGE, 2);
        assert_eq!(NOT_INSTALLED, 10);
        assert_eq!(NOTHING_INSTALLED, 11);
        assert_eq!(NOT_PUBLISHED, 12);
        assert_eq!(NETWORK, 13);
        assert_eq!(VERIFICATION, 14);
        assert_eq!(BUSY, 15);
    }

    #[test]
    fn an_unlabeled_error_exits_one() {
        assert_eq!(code_for(&anyhow::anyhow!("something went wrong")), FAILURE);
    }

    #[test]
    fn context_added_later_does_not_hide_the_code() {
        use anyhow::Context as _;
        let error = Err::<(), _>(Failed::new(NETWORK, "connection refused"))
            .context("could not fetch the release index")
            .unwrap_err();
        assert_eq!(code_for(&error), NETWORK);
    }

    #[test]
    fn the_code_never_leaks_into_the_message() {
        let error = Failed::new(VERIFICATION, "checksum mismatch for dotnet-sdk.tar.gz");
        assert_eq!(
            format!("{error}"),
            "checksum mismatch for dotnet-sdk.tar.gz"
        );
        assert!(!format!("{error:#}").contains("14"));
    }
}
