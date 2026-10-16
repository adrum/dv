//! Microsoft's signatures on .NET release metadata.
//!
//! Every metadata file dv reads — `releases-index.json` and each channel's
//! `releases.json` — names a detached signature beside it:
//!
//! ```json
//! "signature": { "file": "releases.json.20261013194811.p7s",
//!                "expiration": "2027-01-11T19:48:11.9190600+00:00" }
//! ```
//!
//! The `.p7s` is a CMS (PKCS #7) SignedData over the file's exact bytes,
//! signed by "Microsoft Corporation, .NET Release" with a code-signing
//! certificate issued under DigiCert Trusted Root G4. Verifying it is what
//! makes the archive hashes inside the metadata worth trusting: without it,
//! anyone able to rewrite the metadata could rewrite the hashes too.
//!
//! What is checked, and what fails closed:
//!
//! - the signature is detached and covers exactly the bytes dv will use
//! - the signer chains, signature by signature, to the root built into dv
//! - the signer is Microsoft Corporation, with the code-signing usage
//! - the `expiration` Microsoft signed into the file has not passed, which
//!   stops old, correctly signed metadata being replayed to hide newer
//!   releases — a freshly fetched file past it is refused
//!
//! What is not: certificate revocation, which would need network lookups
//! for CRLs or OCSP on every listing. Certificate validity dates are not
//! checked either; the signed expiration is the freshness bound, and it
//! falls within the certificate's lifetime by Microsoft's own process.

use anyhow::Result;
use cms::cert::CertificateChoices;
use cms::content_info::ContentInfo;
use cms::signed_data::{SignedData, SignerIdentifier};
use der::asn1::ObjectIdentifier;
use der::{Decode, Encode};
use rsa::Pkcs1v15Sign;
use rsa::pkcs8::DecodePublicKey;
use x509_cert::Certificate;

use crate::exit::{self, Failed};

/// DigiCert Trusted Root G4 — the root Microsoft's .NET release signing
/// certificate chains to. sha256 552f7bdc…ac899988, pinned by a test.
const ROOT: &[u8] = include_bytes!("certs/digicert-trusted-root-g4.der");

/// The organization the signing certificate must belong to.
const SIGNER_ORGANIZATION: &str = "O=Microsoft Corporation";

const SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");
const DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.1");
const MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const EXTENDED_KEY_USAGE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.37");
const BASIC_CONSTRAINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.19");
const CODE_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.3");

/// Where a metadata file's signature is, and until when it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The `.p7s` file name, relative to the metadata file.
    pub file: String,
    /// Unix seconds after which Microsoft no longer vouches for the file.
    pub expires_at: u64,
}

/// Read the signature reference out of a metadata body.
///
/// The reference sits inside the bytes it signs, so it can be trusted only
/// once the signature verifies — callers verify before acting on
/// `expires_at`.
pub fn reference(body: &str) -> Result<Reference> {
    #[derive(serde::Deserialize)]
    struct Body {
        signature: Option<Signature>,
    }
    #[derive(serde::Deserialize)]
    struct Signature {
        file: String,
        expiration: String,
    }
    let unsigned = || {
        Failed::new(
            exit::VERIFICATION,
            "it carries no signature — dv will not use release metadata Microsoft has not signed",
        )
    };
    let signature = serde_json::from_str::<Body>(body)
        .map_err(|_| unsigned())?
        .signature
        .ok_or_else(unsigned)?;
    if signature.file.contains('/') || signature.file.contains('\\') || signature.file.is_empty() {
        return Err(failure(format!(
            "it names `{}` as its signature, which is not a file name",
            signature.file
        )));
    }
    let expires_at = parse_timestamp(&signature.expiration).ok_or_else(|| {
        failure(format!(
            "its signature expiration `{}` is not a date",
            signature.expiration
        ))
    })?;
    Ok(Reference {
        file: signature.file,
        expires_at,
    })
}

/// Verify `p7s` as Microsoft's detached signature over `content`.
pub fn verify(content: &[u8], p7s: &[u8]) -> Result<()> {
    let root = Certificate::from_der(ROOT).expect("the built-in root certificate parses");

    let info = ContentInfo::from_der(p7s)
        .map_err(|err| failure(format!("its signature is not valid CMS: {err}")))?;
    if info.content_type != SIGNED_DATA {
        return Err(failure("its signature is not CMS SignedData"));
    }
    let signed: SignedData = info
        .content
        .decode_as()
        .map_err(|err| failure(format!("its signature is not valid SignedData: {err}")))?;
    if signed.encap_content_info.econtent_type != DATA
        || signed.encap_content_info.econtent.is_some()
    {
        return Err(failure(
            "its signature is not a detached signature over the file",
        ));
    }

    let certificates: Vec<Certificate> = signed
        .certificates
        .iter()
        .flat_map(|set| set.0.iter())
        .filter_map(|choice| match choice {
            CertificateChoices::Certificate(certificate) => Some(certificate.clone()),
            _ => None,
        })
        .collect();

    let signers = signed.signer_infos.0.as_slice();
    let [signer] = signers else {
        return Err(failure(format!(
            "its signature has {} signers, not one",
            signers.len()
        )));
    };
    let SignerIdentifier::IssuerAndSerialNumber(id) = &signer.sid else {
        return Err(failure(
            "its signature identifies the signer in an unsupported way",
        ));
    };
    let leaf = certificates
        .iter()
        .find(|certificate| {
            certificate.tbs_certificate.issuer == id.issuer
                && certificate.tbs_certificate.serial_number == id.serial_number
        })
        .ok_or_else(|| failure("its signature does not include the signing certificate"))?;

    // Who signed: Microsoft, for code signing, chained to the pinned root.
    let subject = leaf.tbs_certificate.subject.to_string();
    if !subject
        .split(',')
        .any(|part| part.trim() == SIGNER_ORGANIZATION)
    {
        return Err(failure(format!(
            "it is signed by `{subject}`, not by Microsoft"
        )));
    }
    if !has_code_signing(leaf) {
        return Err(failure(
            "its signing certificate is not a code-signing certificate",
        ));
    }
    verify_chain(leaf, &certificates, &root)?;

    // What was signed: these exact bytes.
    let hash = HashAlgorithm::from_digest_oid(&signer.digest_alg.oid)
        .ok_or_else(|| failure("its signature uses an unsupported digest"))?;
    let content_digest = hash.digest(content);
    let signed_digest = match &signer.signed_attrs {
        None => content_digest,
        Some(attributes) => {
            // With signed attributes, the signature covers them, and they
            // carry the content digest.
            let message_digest = attributes
                .iter()
                .find(|attribute| attribute.oid == MESSAGE_DIGEST)
                .and_then(|attribute| attribute.values.iter().next())
                .and_then(|value| value.decode_as::<der::asn1::OctetString>().ok())
                .ok_or_else(|| failure("its signature has no message digest"))?;
            if message_digest.as_bytes() != content_digest.as_slice() {
                return Err(failure(
                    "its signature does not match its contents — it was modified after signing",
                ));
            }
            let encoded = attributes
                .to_der()
                .map_err(|err| failure(format!("its signed attributes do not encode: {err}")))?;
            hash.digest(&encoded)
        }
    };

    let key = public_key(leaf)?;
    key.verify(hash.padding(), &signed_digest, signer.signature.as_bytes())
        .map_err(|_| {
            failure("its signature does not match its contents — it was modified after signing")
        })
}

/// Walk from the signing certificate to the pinned root, verifying each
/// issuer's signature on the certificate below it.
fn verify_chain(leaf: &Certificate, pool: &[Certificate], root: &Certificate) -> Result<()> {
    let mut current = leaf.clone();
    // A real chain here is leaf → intermediate → root; the bound only stops
    // a hostile bundle from looping.
    for _ in 0..4 {
        if current.tbs_certificate.issuer == root.tbs_certificate.subject {
            return verify_issued_by(&current, root);
        }
        let issuer = pool
            .iter()
            .find(|candidate| {
                candidate.tbs_certificate.subject == current.tbs_certificate.issuer
                    && *candidate != &current
            })
            .ok_or_else(|| {
                failure(format!(
                    "its signing certificate does not chain to {} — if Microsoft has \
                     changed how it signs releases, `dv self update` brings a dv that knows",
                    root.tbs_certificate.subject
                ))
            })?;
        if !is_ca(issuer) {
            return Err(failure(format!(
                "{} issued the signing certificate but is not a certificate authority",
                issuer.tbs_certificate.subject
            )));
        }
        verify_issued_by(&current, issuer)?;
        current = issuer.clone();
    }
    Err(failure("its certificate chain is too long"))
}

fn verify_issued_by(certificate: &Certificate, issuer: &Certificate) -> Result<()> {
    let hash = HashAlgorithm::from_signature_oid(&certificate.signature_algorithm.oid)
        .ok_or_else(|| failure("a certificate in its chain uses an unsupported signature"))?;
    let tbs = certificate
        .tbs_certificate
        .to_der()
        .map_err(|err| failure(format!("a certificate in its chain does not encode: {err}")))?;
    let signature = certificate
        .signature
        .as_bytes()
        .ok_or_else(|| failure("a certificate in its chain has a malformed signature"))?;
    public_key(issuer)?
        .verify(hash.padding(), &hash.digest(&tbs), signature)
        .map_err(|_| {
            failure(format!(
                "the certificate for {} was not issued by {}",
                certificate.tbs_certificate.subject, issuer.tbs_certificate.subject
            ))
        })
}

fn public_key(certificate: &Certificate) -> Result<rsa::RsaPublicKey> {
    let spki = certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|err| failure(format!("a certificate key does not encode: {err}")))?;
    rsa::RsaPublicKey::from_public_key_der(&spki)
        .map_err(|_| failure("a certificate in its chain does not have an RSA key"))
}

fn has_code_signing(certificate: &Certificate) -> bool {
    extension(certificate, EXTENDED_KEY_USAGE)
        .and_then(|value| x509_cert::ext::pkix::ExtendedKeyUsage::from_der(value).ok())
        .is_some_and(|usage| usage.0.contains(&CODE_SIGNING))
}

fn is_ca(certificate: &Certificate) -> bool {
    extension(certificate, BASIC_CONSTRAINTS)
        .and_then(|value| x509_cert::ext::pkix::BasicConstraints::from_der(value).ok())
        .is_some_and(|constraints| constraints.ca)
}

fn extension(certificate: &Certificate, oid: ObjectIdentifier) -> Option<&[u8]> {
    certificate
        .tbs_certificate
        .extensions
        .as_ref()?
        .iter()
        .find(|extension| extension.extn_id == oid)
        .map(|extension| extension.extn_value.as_bytes())
}

#[derive(Debug, Clone, Copy)]
enum HashAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl HashAlgorithm {
    fn from_digest_oid(oid: &ObjectIdentifier) -> Option<Self> {
        match oid.to_string().as_str() {
            "2.16.840.1.101.3.4.2.1" => Some(Self::Sha256),
            "2.16.840.1.101.3.4.2.2" => Some(Self::Sha384),
            "2.16.840.1.101.3.4.2.3" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn from_signature_oid(oid: &ObjectIdentifier) -> Option<Self> {
        match oid.to_string().as_str() {
            "1.2.840.113549.1.1.11" => Some(Self::Sha256),
            "1.2.840.113549.1.1.12" => Some(Self::Sha384),
            "1.2.840.113549.1.1.13" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn digest(self, bytes: &[u8]) -> Vec<u8> {
        use sha2::Digest;
        match self {
            Self::Sha256 => sha2::Sha256::digest(bytes).to_vec(),
            Self::Sha384 => sha2::Sha384::digest(bytes).to_vec(),
            Self::Sha512 => sha2::Sha512::digest(bytes).to_vec(),
        }
    }

    fn padding(self) -> Pkcs1v15Sign {
        match self {
            Self::Sha256 => Pkcs1v15Sign::new::<rsa::sha2::Sha256>(),
            Self::Sha384 => Pkcs1v15Sign::new::<rsa::sha2::Sha384>(),
            Self::Sha512 => Pkcs1v15Sign::new::<rsa::sha2::Sha512>(),
        }
    }
}

fn failure(message: impl Into<String>) -> anyhow::Error {
    Failed::new(exit::VERIFICATION, message.into())
}

/// Unix seconds for an ISO 8601 timestamp such as
/// `2027-01-11T19:48:11.9190600+00:00`, the form Microsoft writes.
fn parse_timestamp(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (date, rest) = raw.split_once('T')?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;

    // Time, then an optional fraction, then Z or ±HH:MM.
    let offset_at = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(offset_at);
    let time = time.split('.').next()?;
    let mut parts = time.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next().unwrap_or("0").parse().ok()?;
    let offset = match zone {
        "" | "Z" => 0,
        zone => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let (hours, minutes) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
            sign * (hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60)
        }
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let seconds =
        days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(seconds).ok()
}

/// A Unix time as a calendar date, `2027-01-11`, for messages.
pub fn format_date(unix: u64) -> String {
    // The inverse of `days_from_civil` (Howard Hinnant's algorithm).
    let days = (unix / 86400) as i64 + 719468;
    let era = days.div_euclid(146097);
    let day_of_era = days - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real releases-index.json and the signature Microsoft published for it.
    const INDEX: &[u8] = include_bytes!("testdata/releases-index.json");
    const INDEX_SIGNATURE: &[u8] = include_bytes!("testdata/releases-index.json.p7s");

    #[test]
    fn the_pinned_root_is_digicert_trusted_root_g4() {
        assert_eq!(
            crate::net::hex(&{
                use sha2::Digest;
                sha2::Sha256::digest(ROOT)
            }),
            "552f7bdcf1a7af9e6ce672017f4f12abf77240c78e761ac203d1d9d20ac89988"
        );
    }

    #[test]
    fn microsofts_signature_verifies() {
        verify(INDEX, INDEX_SIGNATURE).unwrap();
    }

    #[test]
    fn one_changed_byte_fails() {
        let mut tampered = INDEX.to_vec();
        let at = tampered.iter().position(|b| *b == b'4').unwrap();
        tampered[at] = b'9';
        let error = verify(&tampered, INDEX_SIGNATURE).unwrap_err();
        assert_eq!(exit::code_for(&error), exit::VERIFICATION);
        assert!(
            error.to_string().contains("modified after signing"),
            "{error}"
        );
    }

    #[test]
    fn a_damaged_signature_fails() {
        // Flip a byte of the signature value itself. (Not just any byte: the
        // timestamp token at the end of the file is deliberately unsigned.)
        let info = ContentInfo::from_der(INDEX_SIGNATURE).unwrap();
        let signed: SignedData = info.content.decode_as().unwrap();
        let value = signed.signer_infos.0.as_slice()[0]
            .signature
            .as_bytes()
            .to_vec();
        let at = INDEX_SIGNATURE
            .windows(value.len())
            .position(|window| window == value.as_slice())
            .unwrap()
            + value.len() / 2;
        let mut damaged = INDEX_SIGNATURE.to_vec();
        damaged[at] ^= 0xff;
        let error = verify(INDEX, &damaged).unwrap_err().to_string();
        assert!(error.contains("modified after signing"), "{error}");
        assert!(verify(INDEX, b"not a signature").is_err());
        assert!(verify(INDEX, &[]).is_err());
    }

    #[test]
    fn a_chain_to_another_root_fails() {
        // Strip the intermediate: the leaf no longer reaches the pinned root.
        let info = ContentInfo::from_der(INDEX_SIGNATURE).unwrap();
        let mut signed: SignedData = info.content.decode_as().unwrap();
        let only_leaf: Vec<CertificateChoices> = signed
            .certificates
            .as_ref()
            .unwrap()
            .0
            .iter()
            .filter(|choice| match choice {
                CertificateChoices::Certificate(c) => {
                    !c.tbs_certificate.subject.to_string().contains("DigiCert")
                }
                _ => true,
            })
            .cloned()
            .collect();
        signed.certificates = Some(cms::signed_data::CertificateSet(
            der::asn1::SetOfVec::try_from(only_leaf).unwrap(),
        ));
        let rebuilt = ContentInfo {
            content_type: SIGNED_DATA,
            content: der::Any::encode_from(&signed).unwrap(),
        }
        .to_der()
        .unwrap();
        let error = verify(INDEX, &rebuilt).unwrap_err().to_string();
        assert!(error.contains("does not chain"), "{error}");
    }

    #[test]
    fn the_reference_is_read_from_the_body() {
        let body = std::str::from_utf8(INDEX).unwrap();
        let reference = reference(body).unwrap();
        assert!(reference.file.ends_with(".p7s"), "{}", reference.file);
        assert!(reference.expires_at > 1_790_000_000);
    }

    #[test]
    fn unsigned_or_odd_metadata_is_refused() {
        assert!(reference(r#"{"releases":[]}"#).is_err());
        assert!(
            reference(r#"{"signature":{"file":"../x.p7s","expiration":"2027-01-11T00:00:00Z"}}"#)
                .is_err()
        );
        assert!(reference(r#"{"signature":{"file":"x.p7s","expiration":"soon"}}"#).is_err());
    }

    #[test]
    fn timestamps_parse_like_microsoft_writes_them() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_timestamp("2027-01-11T19:48:11.9190600+00:00"),
            Some(1_799_696_891)
        );
        assert_eq!(
            parse_timestamp("2027-01-11T21:48:11+02:00"),
            parse_timestamp("2027-01-11T19:48:11Z")
        );
        assert_eq!(parse_timestamp("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_timestamp("not a date"), None);
        assert_eq!(format_date(1_799_696_891), "2027-01-11");
        assert_eq!(format_date(1_709_164_800), "2024-02-29");
        assert_eq!(format_date(0), "1970-01-01");
    }
}
