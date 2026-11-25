//! The sealed block of saved passwords a portable data file may carry.
//!
//! The passwords never appear in the file in the clear: they are serialized as one
//! small JSON object (`{host id: secret}`), sealed with AES-256-GCM, and the key is
//! derived from the user's passphrase with Argon2id over a random salt. A fresh
//! salt and a fresh nonce are drawn for every export, so two files made from the
//! same passwords and passphrase share no ciphertext.
//!
//! Opening the block either returns every password or fails as a whole: a wrong
//! passphrase and a changed ciphertext are the same failure, because the
//! authentication tag cannot tell them apart and the user is not helped by a
//! guess.

use std::collections::BTreeMap;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use shellune_core::{HostId, SecretString};
use zeroize::Zeroize as _;

use super::PortableError;

/// The only key-derivation function this format writes.
const KDF_ARGON2ID: &str = "argon2id";

/// The only cipher this format writes.
const CIPHER_AES_256_GCM: &str = "aes-256-gcm";

/// Bound into the authentication tag, so a block cannot be lifted into another
/// kind of document and opened there.
const ASSOCIATED_DATA: &[u8] = b"shellune-data/1 secrets";

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// The most memory a file may ask the key derivation to use, in KiB (256 MiB).
///
/// The parameters are read from the file, so an unbounded value would let a
/// crafted file exhaust the machine before the passphrase is even checked.
const MAX_MEMORY_KIB: u32 = 256 * 1024;
const MAX_ITERATIONS: u32 = 16;
const MAX_PARALLELISM: u32 = 8;

/// The cost of turning a passphrase into the key that seals the passwords.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

impl Default for KdfParams {
    /// Argon2id with 19 MiB, two passes and one lane: the OWASP baseline, which
    /// takes a fraction of a second on a current machine.
    fn default() -> Self {
        Self {
            memory_kib: 19 * 1024,
            iterations: 2,
            parallelism: 1,
        }
    }
}

impl KdfParams {
    /// Parameters of a chosen cost.
    ///
    /// Returns `None` for values Argon2 refuses or that exceed what this build
    /// will spend on opening a file. A cheaper cost than the default is allowed so
    /// tests stay fast; it is never chosen for the user.
    #[must_use]
    pub fn new(memory_kib: u32, iterations: u32, parallelism: u32) -> Option<Self> {
        let params = Self {
            memory_kib,
            iterations,
            parallelism,
        };
        params.check().ok().map(|()| params)
    }

    /// Memory in KiB.
    #[must_use]
    pub fn memory_kib(self) -> u32 {
        self.memory_kib
    }

    /// Number of passes.
    #[must_use]
    pub fn iterations(self) -> u32 {
        self.iterations
    }

    /// Number of lanes.
    #[must_use]
    pub fn parallelism(self) -> u32 {
        self.parallelism
    }

    fn check(self) -> Result<(), String> {
        if self.parallelism == 0 || self.parallelism > MAX_PARALLELISM {
            return Err(format!(
                "the key derivation asks for {} lanes; 1 to {MAX_PARALLELISM} can be used",
                self.parallelism
            ));
        }
        if self.iterations == 0 || self.iterations > MAX_ITERATIONS {
            return Err(format!(
                "the key derivation asks for {} passes; 1 to {MAX_ITERATIONS} can be used",
                self.iterations
            ));
        }
        if self.memory_kib < 8 * self.parallelism || self.memory_kib > MAX_MEMORY_KIB {
            return Err(format!(
                "the key derivation asks for {} KiB of memory; {} to {MAX_MEMORY_KIB} can be used",
                self.memory_kib,
                8 * self.parallelism
            ));
        }
        Ok(())
    }

    fn argon2(self) -> Result<Argon2<'static>, String> {
        let params = Params::new(
            self.memory_kib,
            self.iterations,
            self.parallelism,
            Some(KEY_LEN),
        )
        .map_err(|error| error.to_string())?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

/// How the block's key was derived, as written in the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KdfRecord {
    algorithm: String,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

impl KdfRecord {
    fn params(&self) -> KdfParams {
        KdfParams {
            memory_kib: self.memory_kib,
            iterations: self.iterations,
            parallelism: self.parallelism,
        }
    }
}

/// The sealed passwords, as stored in the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SecretBlock {
    kdf: KdfRecord,
    cipher: String,
    salt: String,
    nonce: String,
    ciphertext: String,
}

impl SecretBlock {
    /// Seals the given passwords under a key derived from `passphrase`.
    pub(crate) fn seal(
        secrets: &BTreeMap<HostId, SecretString>,
        passphrase: &SecretString,
        params: KdfParams,
    ) -> Result<Self, PortableError> {
        check_passphrase(passphrase)?;
        params
            .check()
            .map_err(|reason| PortableError::Encrypt { reason })?;
        let mut salt = [0_u8; SALT_LEN];
        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::fill(&mut salt)
            .and_then(|()| getrandom::fill(&mut nonce))
            .map_err(|error| PortableError::Encrypt {
                reason: format!("the system random source failed: {error}"),
            })?;

        let mut plaintext = {
            let map: BTreeMap<String, &str> = secrets
                .iter()
                .map(|(id, secret)| (id.to_string(), secret.expose()))
                .collect();
            serde_json::to_vec(&map).map_err(|error| PortableError::Encrypt {
                reason: error.to_string(),
            })?
        };
        let sealed = with_cipher(passphrase, &salt, params, |cipher| {
            let nonce = Nonce::<Aes256Gcm>::from(nonce);
            cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: &plaintext,
                        aad: ASSOCIATED_DATA,
                    },
                )
                .map_err(|_| PortableError::Encrypt {
                    reason: "the passwords could not be sealed".to_owned(),
                })
        });
        plaintext.zeroize();
        let ciphertext = sealed?;
        Ok(Self {
            kdf: KdfRecord {
                algorithm: KDF_ARGON2ID.to_owned(),
                memory_kib: params.memory_kib,
                iterations: params.iterations,
                parallelism: params.parallelism,
            },
            cipher: CIPHER_AES_256_GCM.to_owned(),
            salt: STANDARD.encode(salt),
            nonce: STANDARD.encode(nonce),
            ciphertext: STANDARD.encode(ciphertext),
        })
    }

    /// Checks the block's shape without the passphrase: the algorithms are the
    /// ones this build writes, the cost is bounded and the binary fields decode.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.kdf.algorithm != KDF_ARGON2ID {
            return Err(format!(
                "the saved passwords use the key derivation \"{}\", which this build does not read",
                self.kdf.algorithm
            ));
        }
        if self.cipher != CIPHER_AES_256_GCM {
            return Err(format!(
                "the saved passwords use the cipher \"{}\", which this build does not read",
                self.cipher
            ));
        }
        self.kdf.params().check()?;
        self.decoded().map(|_| ())
    }

    /// The parameters the block was sealed with.
    pub(crate) fn params(&self) -> KdfParams {
        self.kdf.params()
    }

    /// Opens the block.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::WrongPassphrase`] when the passphrase is wrong or the
    /// sealed bytes were changed — the two cannot be told apart — and
    /// [`PortableError::Corrupt`] when the block is not one this build wrote.
    pub(crate) fn open(&self, passphrase: &SecretString) -> Result<SecretBundle, PortableError> {
        check_passphrase(passphrase)?;
        self.validate()
            .map_err(|reason| PortableError::Corrupt { reason })?;
        let (salt, nonce, ciphertext) = self
            .decoded()
            .map_err(|reason| PortableError::Corrupt { reason })?;
        let mut plaintext = with_cipher(passphrase, &salt, self.kdf.params(), |cipher| {
            let nonce = Nonce::<Aes256Gcm>::try_from(nonce.as_slice())
                .map_err(|_| PortableError::WrongPassphrase)?;
            cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: &ciphertext,
                        aad: ASSOCIATED_DATA,
                    },
                )
                .map_err(|_| PortableError::WrongPassphrase)
        })?;
        let parsed: Result<BTreeMap<HostId, String>, _> = serde_json::from_slice(&plaintext);
        plaintext.zeroize();
        let parsed = parsed.map_err(|_| PortableError::Corrupt {
            reason: "the saved passwords opened but are not a list this build wrote".to_owned(),
        })?;
        Ok(SecretBundle {
            secrets: parsed
                .into_iter()
                .map(|(id, secret)| (id, SecretString::new(secret)))
                .collect(),
        })
    }

    #[allow(clippy::type_complexity)]
    fn decoded(&self) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), String> {
        let decode = |field: &str, text: &str| {
            STANDARD
                .decode(text)
                .map_err(|_| format!("the {field} of the saved passwords is not base64"))
        };
        let salt = decode("salt", &self.salt)?;
        let nonce = decode("nonce", &self.nonce)?;
        let ciphertext = decode("ciphertext", &self.ciphertext)?;
        if salt.len() < argon2::MIN_SALT_LEN || salt.len() > 64 {
            return Err("the salt of the saved passwords has an unusable length".to_owned());
        }
        if nonce.len() != NONCE_LEN {
            return Err("the nonce of the saved passwords has an unusable length".to_owned());
        }
        if ciphertext.len() < 16 {
            return Err("the saved passwords are shorter than their own tag".to_owned());
        }
        Ok((salt, nonce, ciphertext))
    }
}

/// Derives the key, hands a cipher to `use_cipher`, and wipes the key afterwards.
fn with_cipher<T>(
    passphrase: &SecretString,
    salt: &[u8],
    params: KdfParams,
    use_cipher: impl FnOnce(&Aes256Gcm) -> Result<T, PortableError>,
) -> Result<T, PortableError> {
    let argon2 = params
        .argon2()
        .map_err(|reason| PortableError::Corrupt { reason })?;
    let mut key = [0_u8; KEY_LEN];
    let derived = argon2
        .hash_password_into(passphrase.expose().as_bytes(), salt, &mut key)
        .map_err(|error| PortableError::Corrupt {
            reason: format!("the key could not be derived: {error}"),
        });
    let outcome = derived.and_then(|()| {
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| PortableError::Encrypt {
            reason: "the derived key has the wrong length".to_owned(),
        })?;
        use_cipher(&cipher)
    });
    key.zeroize();
    outcome
}

fn check_passphrase(passphrase: &SecretString) -> Result<(), PortableError> {
    if passphrase.expose().trim().is_empty() {
        Err(PortableError::EmptyPassphrase)
    } else {
        Ok(())
    }
}

/// The passwords read out of a sealed block, keyed by the record they belong to.
///
/// Each one is a [`SecretString`], so it is wiped when the bundle is dropped and
/// never printed by `Debug`.
#[derive(Clone, Debug, Default)]
pub struct SecretBundle {
    secrets: BTreeMap<HostId, SecretString>,
}

impl SecretBundle {
    /// How many passwords the bundle holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    /// Whether the bundle holds no password.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// The password of one record.
    #[must_use]
    pub fn get(&self, id: HostId) -> Option<&SecretString> {
        self.secrets.get(&id)
    }

    /// The records the bundle has a password for.
    pub fn ids(&self) -> impl Iterator<Item = HostId> + '_ {
        self.secrets.keys().copied()
    }

    /// The remembered host passwords a vault holds live, as the bundle a plan
    /// reads (ADR-6): a record whose password travels in the vault is marked
    /// as remembering it once applied, the same as one whose password came in
    /// a version 1 block. Tombstones bring nothing.
    #[must_use]
    pub fn from_vault(content: &crate::vault::VaultContent) -> Self {
        Self {
            secrets: content
                .host_secret_records()
                .filter_map(|record| {
                    record
                        .secret()
                        .map(|secret| (record.meta().host(), secret.clone()))
                })
                .collect(),
        }
    }
}
