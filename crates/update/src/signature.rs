//! Signature checks: minisign over the file, and the version the signature names.
//!
//! A manifest signature is the base64 of a minisign `.sig` file's text. The file
//! is accepted only when
//!
//! * the signature was made by the given public key and matches the bytes, and
//! * its trusted comment (covered by the signature) has a `version:<v>` field equal
//!   to the version the manifest announces.
//!
//! The second rule stops an old, genuinely signed file from being served as a new
//! version. Only pre-hashed (current minisign) signatures are accepted.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};
use semver::Version;

use crate::UpdateError;
use crate::manifest::parse_version;

/// Checks `data` against `signature` (base64 `.sig` text) and the version it
/// names.
///
/// # Errors
///
/// [`UpdateError::SignatureMissing`] for an empty signature,
/// [`UpdateError::SignatureInvalid`] when it cannot be read or does not match the
/// key and bytes, [`UpdateError::VersionMismatch`] when it names another version
/// or none, and [`UpdateError::InvalidConfig`] for an unusable public key.
pub fn verify_bytes(
    public_key: &str,
    signature: &str,
    data: &[u8],
    version: &Version,
) -> Result<(), UpdateError> {
    let key = decode_public_key(public_key)?;
    let signature = decode_signature(signature)?;
    key.verify(data, &signature, false).map_err(invalid)?;
    check_version(&signature, version)
}

/// Checks the file at `path` like [`verify_bytes`], reading it in chunks.
///
/// # Errors
///
/// As [`verify_bytes`], and [`UpdateError::Io`] when the file cannot be read.
pub fn verify_file(
    public_key: &str,
    signature: &str,
    path: &Path,
    version: &Version,
) -> Result<(), UpdateError> {
    let key = decode_public_key(public_key)?;
    let signature = decode_signature(signature)?;
    let mut verifier = key.verify_stream(&signature).map_err(invalid)?;
    let mut file = File::open(path)
        .map_err(|error| UpdateError::io("the update file cannot be read", &error))?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| UpdateError::io("the update file cannot be read", &error))?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier.finalize().map_err(invalid)?;
    check_version(&signature, version)
}

/// The public key from its base64 key line, or from a whole `.pub` file's text.
pub(crate) fn decode_public_key(public_key: &str) -> Result<PublicKey, UpdateError> {
    let text = public_key.trim();
    let decoded = if text.contains('\n') {
        PublicKey::decode(text)
    } else {
        PublicKey::from_base64(text)
    };
    decoded.map_err(|error| UpdateError::InvalidConfig {
        reason: format!("the update public key cannot be read: {error}"),
    })
}

/// The signature from its base64 form (or, leniently, the `.sig` text itself).
fn decode_signature(signature: &str) -> Result<Signature, UpdateError> {
    let text = signature.trim();
    if text.is_empty() {
        return Err(UpdateError::SignatureMissing);
    }
    let decoded;
    let text = if text.starts_with("untrusted comment:") {
        text
    } else {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(|_| UpdateError::SignatureInvalid {
                reason: "the signature is not base64".to_owned(),
            })?;
        decoded = String::from_utf8(bytes).map_err(|_| UpdateError::SignatureInvalid {
            reason: "the signature is not text".to_owned(),
        })?;
        decoded.as_str()
    };
    Signature::decode(text).map_err(invalid)
}

fn check_version(signature: &Signature, version: &Version) -> Result<(), UpdateError> {
    let signed = signed_version(signature.trusted_comment());
    match &signed {
        Some(Ok(signed)) if signed == version => Ok(()),
        _ => Err(UpdateError::VersionMismatch {
            expected: version.to_string(),
            signed: match signed {
                Some(Ok(signed)) => signed.to_string(),
                Some(Err(raw)) => raw,
                None => "no version".to_owned(),
            },
        }),
    }
}

/// The `version:` field of a trusted comment (`timestamp:…\tfile:…\tversion:…`):
/// the parsed version, the raw text when it is not a version, or `None` when the
/// comment has no such field.
pub(crate) fn signed_version(trusted_comment: &str) -> Option<Result<Version, String>> {
    trusted_comment
        .split(['\t', ' '])
        .find_map(|field| field.strip_prefix("version:"))
        .map(|raw| parse_version(raw).map_err(|_| raw.to_owned()))
}

fn invalid(error: minisign_verify::Error) -> UpdateError {
    let reason = match error {
        minisign_verify::Error::UnexpectedKeyId => {
            "it was made with another key than the one this build trusts".to_owned()
        }
        minisign_verify::Error::InvalidSignature => {
            "the file does not match its signature".to_owned()
        }
        minisign_verify::Error::UnsupportedLegacyMode
        | minisign_verify::Error::UnexpectedAlgorithm => {
            "it uses the legacy signature format".to_owned()
        }
        other => other.to_string(),
    };
    UpdateError::SignatureInvalid { reason }
}
