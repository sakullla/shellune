//! The signing half: making update signatures and key pairs. Only the release
//! tool (`shellune-sign`, feature `release-tools`) and tests use it; the
//! application only verifies.
//!
//! The formats are the ones [`crate::verify_file`] reads:
//!
//! * a signature is the base64 of the minisign `.sig` text, and its trusted
//!   comment is `timestamp:<unix>\tfile:<name>\tversion:<v>`;
//! * the secret key (`SHELLUNE_UPDATER_KEY`) is the base64 of the minisign secret
//!   key file's text, encrypted with a password (`SHELLUNE_UPDATER_KEY_PASSWORD`);
//! * the public key ([`crate::PUBLIC_KEY`]) is the base64 key line of the public
//!   key file.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use crate::manifest::parse_version;

/// The environment variable holding the secret key.
pub const KEY_VARIABLE: &str = "SHELLUNE_UPDATER_KEY";

/// The environment variable holding the secret key's password.
pub const PASSWORD_VARIABLE: &str = "SHELLUNE_UPDATER_KEY_PASSWORD";

/// Why signing or key handling failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignError {
    /// The secret key cannot be read, or the password does not open it.
    #[error("the signing key cannot be used: {0}")]
    Key(String),
    /// The version is not a semantic version.
    #[error("\"{0}\" is not a version")]
    Version(String),
    /// Signing (or reading the data) failed.
    #[error("signing failed: {0}")]
    Sign(String),
}

/// A secret key ready to sign.
pub struct SigningKey {
    secret: minisign::SecretKey,
    public: minisign::PublicKey,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SigningKey")
            .field("public", &self.public.to_base64())
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Opens a secret key: `encoded` is the base64 of the secret key file's text
    /// (or that text itself). An empty `password` opens an unencrypted key.
    ///
    /// # Errors
    ///
    /// [`SignError::Key`] when the key cannot be read or the password is wrong.
    pub fn open(encoded: &str, password: &str) -> Result<Self, SignError> {
        let encoded = encoded.trim();
        let text = if encoded.starts_with("untrusted comment:") {
            encoded.to_owned()
        } else {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .map_err(|_| SignError::Key("it is not base64".to_owned()))?;
            String::from_utf8(bytes).map_err(|_| SignError::Key("it is not text".to_owned()))?
        };
        let boxed = minisign::SecretKeyBox::from_string(&text)
            .map_err(|error| SignError::Key(error.to_string()))?;
        let secret = if password.is_empty() {
            minisign::SecretKey::from_unencrypted_box(boxed)
        } else {
            minisign::SecretKey::from_box(boxed, Some(password.to_owned()))
        }
        .map_err(|error| SignError::Key(error.to_string()))?;
        let public = minisign::PublicKey::from_secret_key(&secret)
            .map_err(|error| SignError::Key(error.to_string()))?;
        Ok(Self { secret, public })
    }

    /// The public key (base64 key line) that verifies this key's signatures.
    #[must_use]
    pub fn public_key(&self) -> String {
        self.public.to_base64()
    }

    /// Signs `data` (the file named `file_name`) for `version`; returns the
    /// base64 of the `.sig` text, as the manifest carries it.
    ///
    /// # Errors
    ///
    /// [`SignError::Version`] for a version that is not one, [`SignError::Sign`]
    /// when the data cannot be read.
    pub fn sign(
        &self,
        data: impl Read,
        file_name: &str,
        version: &str,
    ) -> Result<String, SignError> {
        let version = parse_version(version).map_err(|_| SignError::Version(version.to_owned()))?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        // The comment is one line; a tab or newline in the name would break it.
        let file_name: String = file_name
            .chars()
            .map(|c| if c.is_control() { '_' } else { c })
            .collect();
        self.sign_with_comment(
            data,
            &format!("timestamp:{timestamp}\tfile:{file_name}\tversion:{version}"),
        )
    }

    /// Signs `data` with an arbitrary trusted comment (tests use it for
    /// signatures that name the wrong version or none).
    ///
    /// # Errors
    ///
    /// [`SignError::Sign`] when the data cannot be read.
    pub fn sign_with_comment(
        &self,
        data: impl Read,
        trusted_comment: &str,
    ) -> Result<String, SignError> {
        let signature = minisign::sign(
            Some(&self.public),
            &self.secret,
            data,
            Some(trusted_comment),
            Some("signature from shellune-sign"),
        )
        .map_err(|error| SignError::Sign(error.to_string()))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(signature.into_string()))
    }
}

/// A new key pair, as text.
#[derive(Clone)]
pub struct GeneratedKeys {
    /// The public key's base64 key line (what [`crate::PUBLIC_KEY`] holds).
    pub public_key: String,
    /// The public key file's text.
    pub public_key_file: String,
    /// The encrypted secret key file's text.
    pub secret_key_file: String,
    /// Its base64 (what `SHELLUNE_UPDATER_KEY` holds).
    pub secret_key_base64: String,
}

impl std::fmt::Debug for GeneratedKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeneratedKeys")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

/// Makes a key pair whose secret key is encrypted with `password` (an empty
/// password makes an unencrypted key, for tests).
///
/// # Errors
///
/// [`SignError::Key`] when generation fails.
pub fn generate_keys(password: &str) -> Result<GeneratedKeys, SignError> {
    let key = |error: minisign::PError| SignError::Key(error.to_string());
    let pair = if password.is_empty() {
        minisign::KeyPair::generate_unencrypted_keypair()
    } else {
        minisign::KeyPair::generate_encrypted_keypair(Some(password.to_owned()))
    }
    .map_err(key)?;
    let public_key = pair.pk.to_base64();
    let public_key_file = pair.pk.to_box().map_err(key)?.into_string();
    let secret_key_file = pair
        .sk
        .to_box(Some("shellune update signing key"))
        .map_err(key)?
        .into_string();
    let secret_key_base64 = base64::engine::general_purpose::STANDARD.encode(&secret_key_file);
    Ok(GeneratedKeys {
        public_key,
        public_key_file,
        secret_key_file,
        secret_key_base64,
    })
}
