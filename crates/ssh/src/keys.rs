//! Managed OpenSSH keys: import, generation, export and public material (R1).
//!
//! A key the application vault looks after is born in one of three ways: generated
//! here, imported from an OpenSSH file the user picks, or handed over by the unlocked
//! vault as a one-time PEM. In every case the private material lives in memory for
//! the single operation that needs it, carried by [`SecretString`] or by ssh-key's
//! own zeroizing types, and never becomes a file on its way into an authentication
//! attempt - [AuthMethod::ManagedKey](shellune_core::AuthMethod::ManagedKey) parses
//! the PEM in memory and signs from there.
//!
//! The [`PrivateKey`] this module returns is the same crate instance russh re-exports
//! as `russh::keys::PrivateKey` (both depend on the same pinned release), so a key
//! parsed here reaches the transport as is, without a copy in between.
//!
//! Everything a caller can keep - the algorithm, the `authorized_keys` line, the
//! SHA-256 fingerprint - is public material. The fingerprint is computed from the
//! key's wire encoding and formatted the way `ssh-keygen -lf` prints it, which is
//! what makes it usable as the vault's duplicate-import key: the same public key
//! always produces the same string, whatever file it arrived in.

use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData, RsaKeypair};
use russh::keys::ssh_key::{self, LineEnding, PrivateKey};
use sha2::{Digest, Sha256};
use shellune_core::SecretString;

/// Which kind of managed key to generate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedKeyKind {
    /// An Ed25519 key, the default choice for new keys.
    Ed25519,
    /// An RSA key of 3072 bits, for servers that still ask for RSA.
    Rsa3072,
    /// An RSA key of 4096 bits, for the sites that demand the larger modulus.
    Rsa4096,
}

impl ManagedKeyKind {
    /// The kinds a chooser offers, in the order it shows them.
    pub const ALL: [Self; 3] = [Self::Ed25519, Self::Rsa3072, Self::Rsa4096];
}

/// The public material of a managed key, everything the vault keeps about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedKeyPublic {
    /// The algorithm as OpenSSH names it, for example `ssh-ed25519`.
    pub algorithm: String,
    /// The public key in `authorized_keys` format, comment included.
    pub authorized_key: String,
    /// SHA-256 fingerprint of the public key's wire encoding, in the form
    /// `ssh-keygen -lf` prints. Two imports of the same public key produce the
    /// same string, which is what the vault's duplicate check compares.
    pub fingerprint: String,
}

/// Why a managed key operation refused, said without a secret.
///
/// No variant carries key material or a passphrase: the reason strings come from
/// the parser and the codec, which report positions and algorithms, not values,
/// so these are safe to show, log and keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagedKeyError {
    /// The text is not an OpenSSH private key at all.
    NotAPrivateKey {
        /// What the parser reported.
        reason: String,
    },
    /// The key is encrypted and no passphrase was offered.
    EncryptedWithoutPassphrase,
    /// A passphrase was offered and did not decrypt the key.
    PassphraseRejected,
    /// The key parsed, but its algorithm is not one this build signs with.
    UnsupportedAlgorithm {
        /// The algorithm as the key named it.
        algorithm: String,
    },
    /// An encrypted export was asked for with an empty passphrase.
    EmptyExportPassphrase,
    /// A key could not be generated.
    GenerationFailed {
        /// What the generator reported.
        reason: String,
    },
    /// The key's public or private material could not be encoded.
    EncodingFailed {
        /// What the codec reported.
        reason: String,
    },
}

impl std::fmt::Display for ManagedKeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAPrivateKey { reason } => {
                write!(
                    formatter,
                    "the text is not an OpenSSH private key: {reason}"
                )
            }
            Self::EncryptedWithoutPassphrase => {
                formatter.write_str("the key is encrypted and needs its passphrase")
            }
            Self::PassphraseRejected => {
                formatter.write_str("the passphrase did not decrypt the key")
            }
            Self::UnsupportedAlgorithm { algorithm } => write!(
                formatter,
                "the {algorithm} key is not a kind this build signs with"
            ),
            Self::EmptyExportPassphrase => formatter.write_str(
                "an encrypted export needs a non-empty passphrase; a plain one needs none",
            ),
            Self::GenerationFailed { reason } => {
                write!(formatter, "the key could not be generated: {reason}")
            }
            Self::EncodingFailed { reason } => {
                write!(formatter, "the key could not be encoded: {reason}")
            }
        }
    }
}

impl std::error::Error for ManagedKeyError {}

/// Parses OpenSSH private key text, decrypting it when a passphrase is offered.
///
/// The text is only borrowed: no copy of it outlives this call except the key the
/// parser built, which is zeroized on drop. An encrypted key is decrypted and
/// checked before it is delivered, so the key a caller receives is always usable;
/// a passphrase offered for an unencrypted key is simply not needed, which is not
/// a failure. Only Ed25519, ECDSA and RSA keys are accepted - the algorithms the
/// vault can re-export and the transport can sign with.
///
/// # Errors
///
/// Returns [`ManagedKeyError::NotAPrivateKey`] for text the parser refuses,
/// [`ManagedKeyError::EncryptedWithoutPassphrase`] and
/// [`ManagedKeyError::PassphraseRejected`] for the two ways an encrypted key is
/// refused, and [`ManagedKeyError::UnsupportedAlgorithm`] for a key that parses
/// but is of a kind the vault does not keep.
pub fn parse_private_key(
    pem: &str,
    passphrase: Option<&SecretString>,
) -> Result<PrivateKey, ManagedKeyError> {
    let key = PrivateKey::from_openssh(pem).map_err(|error| ManagedKeyError::NotAPrivateKey {
        reason: error.to_string(),
    })?;
    let key = if key.is_encrypted() {
        let Some(secret) = passphrase else {
            return Err(ManagedKeyError::EncryptedWithoutPassphrase);
        };
        // The passphrase is borrowed for this one call and zeroed with the
        // SecretString that owns it; the decrypt failure names nothing.
        key.decrypt(secret.expose())
            .map_err(|_| ManagedKeyError::PassphraseRejected)?
    } else {
        key
    };
    ensure_supported(&key)?;
    Ok(key)
}

/// Imports private key text the vault holds as a secret.
///
/// The same contract as [parse_private_key], for the moment the unlocked vault
/// hands a PEM over: the text stays inside a [`SecretString`] until the parser
/// borrows it.
///
/// # Errors
///
/// See [parse_private_key]; nothing here adds a failure of its own.
pub fn import_private_key(
    pem: &SecretString,
    passphrase: Option<&SecretString>,
) -> Result<PrivateKey, ManagedKeyError> {
    parse_private_key(pem.expose(), passphrase)
}

/// Generates a new key of the chosen kind with the library's secure random.
///
/// Ed25519 comes from the library's own generator; RSA names its bit size
/// explicitly, 3072 or 4096, because "an RSA key" without a size is not a
/// decision this function is allowed to make for the user.
///
/// # Errors
///
/// Returns [`ManagedKeyError::GenerationFailed`] when the generator refuses,
/// which is a random-source or prime-search failure and never a comment on the
/// caller's input.
pub fn generate(kind: ManagedKeyKind) -> Result<PrivateKey, ManagedKeyError> {
    // The OS random source behind the ssh-key generators; `UnwrapErr` turns its
    // fallible interface into the infallible one the generators ask for.
    let mut rng = ssh_key::rand_core::UnwrapErr(ssh_key::getrandom::SysRng);
    let keypair = match kind {
        ManagedKeyKind::Ed25519 => KeypairData::from(Ed25519Keypair::random(&mut rng)),
        ManagedKeyKind::Rsa3072 => {
            KeypairData::from(RsaKeypair::random(&mut rng, 3072).map_err(generation_failed)?)
        }
        ManagedKeyKind::Rsa4096 => {
            KeypairData::from(RsaKeypair::random(&mut rng, 4096).map_err(generation_failed)?)
        }
    };
    PrivateKey::new(keypair, "").map_err(|error| ManagedKeyError::GenerationFailed {
        reason: error.to_string(),
    })
}

/// The public material of a key: what the vault stores, what the user compares.
///
/// # Errors
///
/// Returns [`ManagedKeyError::EncodingFailed`] when the key cannot be turned
/// into its wire or `authorized_keys` form, which the supported algorithms
/// never produce but an opaque key would.
pub fn public_data(key: &PrivateKey) -> Result<ManagedKeyPublic, ManagedKeyError> {
    let public = key.public_key();
    // The fingerprint covers the same bytes ssh-keygen hashes, and is formatted
    // by the one formatter the workspace uses for SSH fingerprints.
    let bytes = public.to_bytes().map_err(encoding_failed)?;
    let fingerprint =
        shellune_core::sha256_fingerprint(&Sha256::digest(&bytes)).ok_or_else(|| {
            ManagedKeyError::EncodingFailed {
                reason: "the public key digest is not a SHA-256".to_owned(),
            }
        })?;
    let authorized_key = public.to_openssh().map_err(encoding_failed)?;
    Ok(ManagedKeyPublic {
        algorithm: public.algorithm().to_string(),
        authorized_key,
        fingerprint,
    })
}

/// Exports a private key as OpenSSH text: encrypted with a passphrase, or plain.
///
/// An encrypted export always asks for a non-empty passphrase - an encrypted file
/// with an empty one would only look protected - and the confirmation that a plain
/// export is wanted belongs to the caller, who can see the risk. The result is a
/// [`SecretString`], so the text is zeroized the moment the caller is done with it.
///
/// # Errors
///
/// Returns [`ManagedKeyError::EmptyExportPassphrase`] for an encrypted export
/// with an empty passphrase and [`ManagedKeyError::EncodingFailed`] when either
/// form cannot be encoded.
pub fn export_private_key(
    key: &PrivateKey,
    passphrase: Option<&SecretString>,
) -> Result<SecretString, ManagedKeyError> {
    let pem = match passphrase {
        None => key.to_openssh(LineEnding::LF).map_err(encoding_failed)?,
        Some(secret) => {
            if secret.is_empty() {
                return Err(ManagedKeyError::EmptyExportPassphrase);
            }
            key.encrypt(&mut ssh_key::getrandom::SysRng, secret.expose())
                .map_err(encoding_failed)?
                .to_openssh(LineEnding::LF)
                .map_err(encoding_failed)?
        }
    };
    Ok(SecretString::new(pem.as_str().to_owned()))
}

/// The key parsed, but of a kind the vault does not keep.
fn ensure_supported(key: &PrivateKey) -> Result<(), ManagedKeyError> {
    let supported = matches!(
        key.key_data(),
        KeypairData::Ed25519(_) | KeypairData::Ecdsa(_) | KeypairData::Rsa(_)
    );
    if supported {
        Ok(())
    } else {
        Err(ManagedKeyError::UnsupportedAlgorithm {
            algorithm: key.algorithm().to_string(),
        })
    }
}

fn generation_failed(error: ssh_key::Error) -> ManagedKeyError {
    ManagedKeyError::GenerationFailed {
        reason: error.to_string(),
    }
}

fn encoding_failed(error: ssh_key::Error) -> ManagedKeyError {
    ManagedKeyError::EncodingFailed {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::PublicKey;

    /// An Ed25519 key made with ssh-keygen, and the fingerprint it printed:
    ///
    ///   ssh-keygen -lf -> 256 SHA256:A69VO5Ev0/1SwrTy/JnjJevc/Hr72qDYR9UiTpSDpFc
    ///
    /// Keeping the pair here is what makes the checks below comparisons with
    /// another implementation instead of with this one.
    const ED25519_PEM: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCHpLwuPcynVqjW7eo9aQlJB2w8kQGBPcqU4YWFAhdjFAAAAJj28SxQ9vEs
UAAAAAtzc2gtZWQyNTUxOQAAACCHpLwuPcynVqjW7eo9aQlJB2w8kQGBPcqU4YWFAhdjFA
AAAEAUVZTUj4K5qfW4+a2XXIZu7sLzverUh2XiJ3S8SJIuR4ekvC49zKdWqNbt6j1pCUkH
bDyRAYE9ypThhYUCF2MUAAAADnZlY3Rvci1lZDI1NTE5AQIDBAUGBw==
-----END OPENSSH PRIVATE KEY-----";
    const ED25519_FINGERPRINT: &str = "SHA256:A69VO5Ev0/1SwrTy/JnjJevc/Hr72qDYR9UiTpSDpFc";
    const ED25519_PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIekvC49zKdWqNbt6j1pCUkHbDyRAYE9ypThhYUCF2MU vector-ed25519";

    /// The same key, protected by a passphrase, as ssh-keygen -p wrote it.
    const ENCRYPTED_PEM: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABAiebIZlL
NrnKmBC3ZdXsd8AAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIIekvC49zKdWqNbt
6j1pCUkHbDyRAYE9ypThhYUCF2MUAAAAoJ0HpRMuiMmXATiCbAmp+lmnMNUDc4Wv883lbg
E1HyWcKTGANag73krXMhrrefd95zfxoE2gMiM52vgGUWg1JNVAFFqgU0EDq44KdVww+dJY
7UfkFpXGGuMfJQoDXM7DpxAZhkB5FhxFHQJT9xeMJbzvaxIzW3UDloxwuhLdkVoGWBUuFr
/ja+I0Xt3Ba4a4ulmc6bE/oBrW9TLnh3v+PQs=
-----END OPENSSH PRIVATE KEY-----";
    const PASSPHRASE: &str = "vector-passphrase";

    /// An ECDSA P-256 key and its ssh-keygen fingerprint:
    ///
    ///   ssh-keygen -lf -> 256 SHA256:8wsSidS2xrIehHbZ6f7A3cTMIplrcbD0FfRI2v1km3E
    const ECDSA_PEM: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAaAAAABNlY2RzYS
1zaGEyLW5pc3RwMjU2AAAACG5pc3RwMjU2AAAAQQTv2BJqvFYruj3XWQ87HwXCoN6oxo+1
zm3c0DVeofNb5xS+ArYSUaS7zehwi2PbuNPOr1pFHoM4/Wwm5h1Na00mAAAAqNkiMqjZIj
KoAAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBO/YEmq8Viu6PddZ
DzsfBcKg3qjGj7XObdzQNV6h81vnFL4CthJRpLvN6HCLY9u4086vWkUegzj9bCbmHU1rTS
YAAAAhAMz2zs04VGeeK8rA3+3oNdMpaFDv3jMcQzD3o635S+PrAAAADHZlY3Rvci1lY2Rz
YQECAw==
-----END OPENSSH PRIVATE KEY-----";
    const ECDSA_FINGERPRINT: &str = "SHA256:8wsSidS2xrIehHbZ6f7A3cTMIplrcbD0FfRI2v1km3E";

    /// An RSA 3072 key and its ssh-keygen fingerprint:
    ///
    ///   ssh-keygen -lf -> 3072 SHA256:6VJPEAADl8nQCVXbh+9oiV+awk46UhxlpzJiOjl52ZY
    const RSA_PEM: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
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
NZRdk6+zKFeuUAAAAKdmVjdG9yLXJzYQE=
-----END OPENSSH PRIVATE KEY-----";
    const RSA_FINGERPRINT: &str = "SHA256:6VJPEAADl8nQCVXbh+9oiV+awk46UhxlpzJiOjl52ZY";

    /// Structurally valid OpenSSH private key text whose algorithm names no
    /// decoder this build knows, which is what a key of a kind the vault does
    /// not keep looks like on arrival.
    const UNKNOWN_PEM: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZn
V0dXJlOQAAACCHpLwuPcynVqjW7eo9aQlJB2w8kQGBPcqU4YWFAhdjFAAAAJj28SxQ9vEs
UAAAAAtzc2gtZnV0dXJlOQAAACCHpLwuPcynVqjW7eo9aQlJB2w8kQGBPcqU4YWFAhdjFA
AAAEAUVZTUj4K5qfW4+a2XXIZu7sLzverUh2XiJ3S8SJIuR4ekvC49zKdWqNbt6j1pCUkH
bDyRAYE9ypThhYUCF2MUAAAADnZlY3Rvci1lZDI1NTE5AQIDBAUGBw==
-----END OPENSSH PRIVATE KEY-----";

    #[test]
    fn an_ed25519_key_imports_with_what_ssh_keygen_prints_for_it() {
        let key = parse_private_key(ED25519_PEM, None).expect("a plaintext key parses");
        assert!(!key.is_encrypted(), "a plaintext key arrives decrypted");
        let public = public_data(&key).expect("public material");
        assert_eq!(public.algorithm, "ssh-ed25519");
        assert_eq!(public.fingerprint, ED25519_FINGERPRINT);
        assert_eq!(public.authorized_key, ED25519_PUBLIC);
    }

    #[test]
    fn an_ecdsa_key_imports_with_its_algorithm_and_fingerprint() {
        let key = parse_private_key(ECDSA_PEM, None).expect("an ECDSA key parses");
        let public = public_data(&key).expect("public material");
        assert_eq!(public.algorithm, "ecdsa-sha2-nistp256");
        assert_eq!(public.fingerprint, ECDSA_FINGERPRINT);
    }

    #[test]
    fn an_rsa_key_imports_with_its_algorithm_and_fingerprint() {
        let key = parse_private_key(RSA_PEM, None).expect("an RSA key parses");
        let public = public_data(&key).expect("public material");
        assert_eq!(public.algorithm, "ssh-rsa");
        assert_eq!(public.fingerprint, RSA_FINGERPRINT);
    }

    #[test]
    fn the_same_public_key_from_two_files_deduplicates_on_one_fingerprint() {
        // One key in two files - one plain, one passphrase protected - is the
        // shape a duplicate import really arrives in, and the fingerprint is
        // what the vault compares.
        let plain = parse_private_key(ED25519_PEM, None).expect("the plain copy parses");
        let encrypted = parse_private_key(ENCRYPTED_PEM, Some(&SecretString::new(PASSPHRASE)))
            .expect("the encrypted copy decrypts");
        let first = public_data(&plain).expect("public material");
        let second = public_data(&encrypted).expect("public material");
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.fingerprint, ED25519_FINGERPRINT);
    }

    #[test]
    fn an_encrypted_key_is_delivered_only_after_its_passphrase_decrypts_it() {
        let error = parse_private_key(ENCRYPTED_PEM, None).expect_err("no passphrase was offered");
        assert_eq!(error, ManagedKeyError::EncryptedWithoutPassphrase);

        let key = parse_private_key(ENCRYPTED_PEM, Some(&SecretString::new(PASSPHRASE)))
            .expect("the right passphrase decrypts");
        assert!(!key.is_encrypted(), "the delivered key is usable as is");
    }

    #[test]
    fn a_wrong_passphrase_is_refused_without_repeating_it() {
        let wrong = SecretString::new("not-the-passphrase");
        let error = parse_private_key(ENCRYPTED_PEM, Some(&wrong))
            .expect_err("the wrong passphrase cannot decrypt");
        assert_eq!(error, ManagedKeyError::PassphraseRejected);
        assert!(
            !error.to_string().contains("not-the-passphrase"),
            "an error must never repeat the passphrase: {error}"
        );
    }

    #[test]
    fn a_key_of_an_unrecognized_algorithm_is_refused_by_name() {
        // The parser refuses an algorithm this build knows nothing about while
        // naming it, so a key of a kind no vault keeps is told apart from text
        // that is not a key at all.
        let error = parse_private_key(UNKNOWN_PEM, None).expect_err("no vault for this kind");
        assert!(matches!(error, ManagedKeyError::NotAPrivateKey { .. }));
        assert!(
            error.to_string().contains("ssh-future9"),
            "the refusal names the algorithm: {error}"
        );
    }

    #[test]
    fn text_that_is_not_a_key_is_refused() {
        let error = parse_private_key("definitely not a key", None).expect_err("not a key");
        assert!(matches!(error, ManagedKeyError::NotAPrivateKey { .. }));
    }

    #[test]
    fn a_generated_ed25519_key_round_trips_and_reports_public_material() {
        let key = generate(ManagedKeyKind::Ed25519).expect("an Ed25519 generates");
        let public = public_data(&key).expect("public material");
        assert_eq!(public.algorithm, "ssh-ed25519");
        // The exact fingerprint differs every run - the key is random - so what
        // is checked is the form ssh-keygen prints and nothing padded.
        assert!(public.fingerprint.starts_with("SHA256:"));
        assert!(!public.fingerprint.contains('='));

        // The authorized_keys line is one a real server reads back.
        let parsed = PublicKey::from_openssh(&public.authorized_key)
            .expect("the public line parses as authorized_keys");
        assert_eq!(
            parsed.fingerprint(ssh_key::HashAlg::Sha256).to_string(),
            public.fingerprint,
            "the fingerprint covers exactly the line that is stored"
        );

        // A plaintext export re-imports to the very same key.
        let exported = export_private_key(&key, None).expect("a plain export");
        let reimported = import_private_key(&exported, None).expect("the export re-imports");
        assert_eq!(
            public_data(&reimported)
                .expect("public material")
                .fingerprint,
            public.fingerprint
        );
    }

    #[test]
    fn an_rsa_key_comes_in_both_offered_sizes() {
        for (kind, bits) in [
            (ManagedKeyKind::Rsa3072, 3072),
            (ManagedKeyKind::Rsa4096, 4096),
        ] {
            let key = generate(kind).expect("the key generates");
            let KeypairData::Rsa(keypair) = key.key_data() else {
                panic!("{kind:?} generated something else");
            };
            assert_eq!(
                keypair.key_size(),
                bits,
                "{kind:?} must be generated at its named size"
            );
            let public = public_data(&key).expect("public material");
            assert_eq!(public.algorithm, "ssh-rsa");
        }
    }

    #[test]
    fn an_encrypted_export_round_trips_and_keeps_the_wrong_passphrase_out() {
        let key = generate(ManagedKeyKind::Ed25519).expect("an Ed25519 generates");
        let passphrase = SecretString::new("export-passphrase");
        let exported = export_private_key(&key, Some(&passphrase)).expect("an encrypted export");
        let reimported = import_private_key(&exported, None)
            .expect_err("an encrypted export needs its passphrase");
        assert_eq!(reimported, ManagedKeyError::EncryptedWithoutPassphrase);

        let wrong = import_private_key(&exported, Some(&SecretString::new("wrong")))
            .expect_err("the wrong passphrase is refused");
        assert_eq!(wrong, ManagedKeyError::PassphraseRejected);

        let opened = import_private_key(&exported, Some(&passphrase))
            .expect("the right passphrase opens the export");
        assert!(!opened.is_encrypted());
        assert_eq!(
            public_data(&opened).expect("public material"),
            public_data(&key).expect("public material"),
            "an export and re-import is the same key"
        );

        let empty = export_private_key(&key, Some(&SecretString::new("")))
            .expect_err("an empty export passphrase protects nothing");
        assert_eq!(empty, ManagedKeyError::EmptyExportPassphrase);
    }

    #[test]
    fn a_key_parsed_here_is_the_private_key_the_transport_uses() {
        // russh re-exports the very crate pinned beside it, and this assignment
        // is the proof the two are one type: a key parsed for the vault is
        // handed to a session without conversion or copy.
        let parsed = parse_private_key(ED25519_PEM, None).expect("the vector key parses");
        let transport: russh::keys::PrivateKey = parsed;
        assert_eq!(transport.algorithm().as_str(), "ssh-ed25519");
    }
}
