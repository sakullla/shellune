//! Server identity: the algorithm, the SHA-256 fingerprint and the decision (R6).
//!
//! The fingerprint is computed here, from the key's own wire encoding, in the form
//! "ssh-keygen -lf" prints. That matters: the user compares the value on screen with
//! the one the server's administrator can run the same command for, so a fingerprint
//! this build made up, truncated or padded differently would be useless.
//!
//! The module also carries the three answers a trust store can give, so the decision
//! is made once in the domain and the transport only executes it.

use russh::keys::PublicKeyOrCertificate;
use russh::keys::ssh_key::PublicKey;
use sha2::{Digest, Sha256};

/// A server identity as it was presented during the handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedKey {
    /// Algorithm the server named, for example ssh-ed25519.
    pub algorithm: String,
    /// The SHA256 fingerprint, in the form ssh-keygen prints it.
    pub fingerprint: String,
    /// Base64 of the key's wire encoding, which is what the fingerprint covers.
    pub public_key: String,
}

impl PresentedKey {
    /// Reads a public key into the form the user can compare.
    ///
    /// Returns None when the key has no wire encoding or the digest is not a
    /// SHA-256, because a value that cannot be compared must not be shown as if it
    /// were verifiable.
    #[must_use]
    pub fn from_public_key(key: &PublicKey) -> Option<Self> {
        let bytes = key.to_bytes().ok()?;
        let digest = Sha256::digest(&bytes);
        let fingerprint = shellune_core::sha256_fingerprint(&digest)?;
        Some(Self {
            algorithm: key.algorithm().to_string(),
            fingerprint,
            public_key: base64(&bytes),
        })
    }

    /// Reads the key the server presented, whether it is a plain key or a
    /// certificate.
    #[must_use]
    pub fn from_server_key(key: &PublicKeyOrCertificate) -> Option<Self> {
        match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => Self::from_public_key(key),
            PublicKeyOrCertificate::Certificate(certificate) => {
                // A certificate answers on behalf of the key it certifies, and that
                // key is what the user compares.
                let key = PublicKey::new(certificate.public_key().clone(), "");
                Self::from_public_key(&key)
            }
        }
    }
}

/// What the stored trust records say about a host, before the connection is made.
///
/// The SSH layer only executes this decision: it never decides that a new identity
/// may replace a trusted one, which is why there is no "replace" variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KnownHostKey {
    /// The identity below was accepted earlier.
    Trusted {
        /// The fingerprint that was accepted.
        fingerprint: String,
    },
    /// Nothing was accepted for this host and port yet.
    Unknown,
    /// Another identity is trusted, and it is not the one that will be presented.
    Changed {
        /// Algorithm of the identity that was accepted earlier.
        algorithm: String,
        /// Fingerprint of the identity that was accepted earlier.
        fingerprint: String,
    },
}

impl KnownHostKey {
    /// The fingerprint that was trusted before, when there is one.
    #[must_use]
    pub fn trusted_fingerprint(&self) -> Option<&str> {
        match self {
            Self::Trusted { fingerprint } | Self::Changed { fingerprint, .. } => {
                Some(fingerprint.as_str())
            }
            Self::Unknown => None,
        }
    }

    /// Whether a connection may go ahead without asking the user.
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Trusted { .. })
    }
}

/// The question the shell has to ask before an unknown identity is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyPrompt {
    /// Host the identity belongs to.
    pub host: String,
    /// Port the identity belongs to.
    pub port: u16,
    /// The identity the server presented.
    pub presented: PresentedKey,
}

impl HostKeyPrompt {
    /// The key algorithm, for the dialog.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.presented.algorithm
    }

    /// The SHA-256 fingerprint, for the dialog.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.presented.fingerprint
    }
}

/// Base64 with the standard alphabet, unpadded like OpenSSH's own output.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut encoded = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map(u32::from);
        let third = chunk.get(2).copied().map(u32::from);

        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded
            .push(ALPHABET[(((first & 0b11) << 4) | (second.unwrap_or(0) >> 4)) as usize] as char);
        if let Some(second) = second {
            encoded.push(
                ALPHABET[(((second & 0b1111) << 2) | (third.unwrap_or(0) >> 6)) as usize] as char,
            );
        }
        if let Some(third) = third {
            encoded.push(ALPHABET[(third & 0b11_1111) as usize] as char);
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ed25519 key and the fingerprint OpenSSH printed for it:
    ///
    ///   ssh-keygen -lf -> 256 SHA256:8+HA+E5YsqRWqqxmpffeTaHgqhRIo5IyTrPplzBBMeY
    ///
    /// Keeping the pair here is what makes the formatting check a comparison with
    /// another implementation instead of with itself.
    const OPENSSH_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK6Lk1FqcnKXBppTRsW38vMVUrRnrLv6rQbxs6aDboAs vectorkey";
    const OPENSSH_FINGERPRINT: &str = "SHA256:8+HA+E5YsqRWqqxmpffeTaHgqhRIo5IyTrPplzBBMeY";

    fn vector_key() -> PublicKey {
        let openssh = OPENSSH_KEY
            .split_whitespace()
            .take(2)
            .collect::<Vec<&str>>()
            .join(" ");
        PublicKey::from_openssh(&openssh).expect("the vector key parses")
    }

    #[test]
    fn a_fingerprint_matches_what_ssh_keygen_prints() {
        let presented = PresentedKey::from_public_key(&vector_key()).expect("a comparable key");
        assert_eq!(presented.algorithm, "ssh-ed25519");
        assert_eq!(
            presented.fingerprint, OPENSSH_FINGERPRINT,
            "the fingerprint must be the one the user can check with ssh-keygen -lf"
        );
        assert!(!presented.fingerprint.contains('='), "no base64 padding");
    }

    #[test]
    fn the_public_key_is_the_wire_encoding_the_fingerprint_covers() {
        let presented = PresentedKey::from_public_key(&vector_key()).expect("a comparable key");
        assert_eq!(
            presented.public_key,
            OPENSSH_KEY.split_whitespace().nth(1).expect("the key body"),
            "the stored public key is exactly what the fingerprint hashes"
        );
    }

    #[test]
    fn a_prompt_names_the_algorithm_and_the_fingerprint() {
        let prompt = HostKeyPrompt {
            host: "web.example.org".to_owned(),
            port: 22,
            presented: PresentedKey::from_public_key(&vector_key()).expect("comparable"),
        };
        assert_eq!(prompt.algorithm(), "ssh-ed25519");
        assert_eq!(prompt.fingerprint(), OPENSSH_FINGERPRINT);
    }

    #[test]
    fn a_trusted_record_carries_the_fingerprint_that_was_accepted() {
        let trusted = KnownHostKey::Trusted {
            fingerprint: OPENSSH_FINGERPRINT.to_owned(),
        };
        assert!(trusted.is_trusted());
        assert_eq!(trusted.trusted_fingerprint(), Some(OPENSSH_FINGERPRINT));

        let changed = KnownHostKey::Changed {
            algorithm: "ssh-ed25519".to_owned(),
            fingerprint: OPENSSH_FINGERPRINT.to_owned(),
        };
        assert!(!changed.is_trusted());
        assert_eq!(changed.trusted_fingerprint(), Some(OPENSSH_FINGERPRINT));
        assert_eq!(KnownHostKey::Unknown.trusted_fingerprint(), None);
    }
}
