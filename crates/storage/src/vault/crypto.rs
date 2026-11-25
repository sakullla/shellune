//! The sealed vault as bytes: one data key, several ways to unseal it
//! (R2, ADR-2).
//!
//! A [`VaultCipher`] is the whole ciphertext of the vault, with no opinion
//! about where it lives: the same block is `vault.json` on this machine and
//! the `vault` section of a version 2 portable document. It holds
//!
//! * a random *data key* (DEK) that seals the record body, and
//! * one *wrap* per unseal secret — the DEK sealed again under a key derived
//!   from that secret.
//!
//! The wraps are the four scenarios R2 names: the random local secret of this
//! machine, the WebDAV password, the sync passphrase of a passwordless WebDAV,
//! and the sync passphrase a local export always requires. A passphrase key is
//! derived with Argon2id over a fresh salt with bounded cost, exactly the way
//! the version 1 password block already derives its key, so the two formats
//! age together.
//!
//! The authentication data binds what a block is: the format version, the
//! identity of the vault it belongs to, whether the bytes are a body or a
//! wrap, and — for a passphrase wrap — which passphrase it answers to. A wrap
//! therefore cannot open in another vault, a body cannot be spliced under a
//! wrap it was not sealed with, and the WebDAV password does not quietly
//! become the sync passphrase. Opening fails as a whole, and a wrong secret
//! and changed bytes are the same failure, because the authentication tag
//! cannot tell them apart and a guess would not help the user.
//!
//! Every sealing draws a fresh data key, a fresh salt and fresh nonces, so the
//! same content sealed twice shares no ciphertext.

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use shellune_core::SecretString;
use zeroize::Zeroize as _;
use zeroize::Zeroizing;

use crate::portable::KdfParams;

use super::content::{self, VaultContent};
use super::store::VaultStoreError;

/// The only key-derivation algorithm a wrap may name.
const KDF_ARGON2ID: &str = "argon2id";

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;
/// The shortest ciphertext that can still carry a tag.
const MIN_CIPHERTEXT_LEN: usize = 16;
/// The shortest salt Argon2 accepts and the longest worth reading.
const MIN_SALT_LEN: usize = 8;
const MAX_SALT_LEN: usize = 64;

/// What a passphrase wrap answers to, bound into the authentication data.
///
/// The WebDAV password and the sync passphrase are different secrets for
/// different uses; binding the purpose into the tag keeps one from quietly
/// opening the other's wrap, which is what makes "the WebDAV password was
/// reused" a decision about trust and not an accident of key handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassphrasePurpose {
    /// The WebDAV account password, reused as the unseal secret of the
    /// syncing vault.
    WebDavPassword,
    /// The sync passphrase: what a passwordless WebDAV and every local export
    /// require, and what they share.
    SyncPassphrase,
}

impl PassphrasePurpose {
    /// The name bound into the authentication data.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebDavPassword => "webdav-password",
            Self::SyncPassphrase => "sync-passphrase",
        }
    }

    fn which(self) -> &'static str {
        match self {
            Self::WebDavPassword => "WebDAV password",
            Self::SyncPassphrase => "sync passphrase",
        }
    }
}

/// How a wrap says the data key was sealed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WrapRecord {
    /// Sealed under the random local secret only this machine keeps in the
    /// system credential store.
    Local {
        /// The nonce of the wrap, base64.
        nonce: String,
        /// The sealed data key, base64.
        ciphertext: String,
    },
    /// Sealed under a key derived from a passphrase with Argon2id.
    Passphrase {
        /// Which passphrase this wrap answers to.
        purpose: PassphrasePurpose,
        /// The cost of the derivation, as written.
        kdf: WrapKdf,
        /// The random salt of the derivation, base64.
        salt: String,
        /// The nonce of the wrap, base64.
        nonce: String,
        /// The sealed data key, base64.
        ciphertext: String,
    },
}

/// The derivation cost of one wrap, as written in the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrapKdf {
    algorithm: String,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

impl WrapKdf {
    fn params(&self) -> Result<KdfParams, VaultStoreError> {
        KdfParams::new(self.memory_kib, self.iterations, self.parallelism).ok_or_else(|| {
            VaultStoreError::Corrupt {
                reason: "the wrap asks the key derivation for a cost this build will not spend"
                    .to_owned(),
            }
        })
    }
}

/// One way a vault can be unsealed, as the caller hands the secret to the
/// seal. The local secret arrives resolved out of the system credential store
/// (see [`super::store`]); the passphrases arrive as the user gave them.
#[derive(Clone, Debug)]
pub enum KeySource {
    /// The random local unseal secret of this machine.
    LocalMachine(SecretString),
    /// A passphrase and the use it answers to.
    Passphrase {
        /// Which passphrase this is.
        purpose: PassphrasePurpose,
        /// The passphrase itself.
        value: SecretString,
    },
}

/// The sealed vault: the record body under a data key, and the data key under
/// every wrap. No field can hold a secret in the clear.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultCipher {
    vault_id: String,
    wraps: Vec<WrapRecord>,
    nonce: String,
    ciphertext: String,
}

impl VaultCipher {
    /// The identity of the vault this ciphertext belongs to. It stays the same
    /// across saves of one vault and differs between vaults, which is part of
    /// what the authentication data binds.
    #[must_use]
    pub fn vault_id(&self) -> &str {
        &self.vault_id
    }

    /// The wraps the ciphertext carries.
    #[must_use]
    pub fn wraps(&self) -> &[WrapRecord] {
        &self.wraps
    }

    /// Whether this machine's local secret can open the ciphertext.
    #[must_use]
    pub fn has_local_wrap(&self) -> bool {
        self.wraps
            .iter()
            .any(|wrap| matches!(wrap, WrapRecord::Local { .. }))
    }

    /// The purposes of the passphrase wraps the ciphertext carries.
    pub fn passphrase_purposes(&self) -> impl Iterator<Item = PassphrasePurpose> + '_ {
        self.wraps.iter().filter_map(|wrap| match wrap {
            WrapRecord::Passphrase { purpose, .. } => Some(*purpose),
            WrapRecord::Local { .. } => None,
        })
    }

    /// Seals `content` under a fresh data key that every source can unseal.
    ///
    /// `vault_id` keeps the identity of an existing vault (a re-save); without
    /// it a fresh identity is drawn. The data key, every salt and every nonce
    /// are new, so sealing the same content twice produces different
    /// ciphertext.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Invalid`] without sources or with the local
    /// unseal secret named twice, [`VaultStoreError::EmptySecret`] for a blank
    /// passphrase, and [`VaultStoreError::RandomSource`] or
    /// [`VaultStoreError::Encrypt`] when the system random source or the
    /// cipher fails. Nothing is written by this function, so no failure leaves
    /// anything behind.
    pub fn seal(
        content: &VaultContent,
        vault_id: Option<&str>,
        sources: &[KeySource],
        kdf: KdfParams,
    ) -> Result<Self, VaultStoreError> {
        if sources.is_empty() {
            return Err(VaultStoreError::Invalid {
                reason: "a vault must be sealed for at least one way to unseal it".to_owned(),
            });
        }
        // One machine has one local secret, so one wrap: a second wrap of it
        // would be a file the reader refuses, and a caller handing the same
        // source twice is told so here rather than locked out later.
        // Passphrase wraps are not limited — several of one purpose are a
        // legitimate state.
        if sources
            .iter()
            .filter(|source| matches!(source, KeySource::LocalMachine(_)))
            .count()
            > 1
        {
            return Err(VaultStoreError::Invalid {
                reason: "the local unseal secret cannot seal the vault twice".to_owned(),
            });
        }
        let kdf = checked_kdf(kdf)?;
        let vault_id = match vault_id {
            Some(id) if !id.trim().is_empty() => id.to_owned(),
            _ => random_vault_id()?,
        };

        let mut plaintext = content::encode(content)?;
        let data_key = random(KEY_LEN)?;
        let body_nonce = random(NONCE_LEN)?;
        let ciphertext = seal_bytes(
            data_key.as_slice(),
            body_nonce.as_slice(),
            plaintext.as_slice(),
            &body_aad(&vault_id),
        )
        .map_err(|reason| VaultStoreError::Encrypt { reason })?;
        let mut wraps = Vec::with_capacity(sources.len());
        for source in sources {
            wraps.push(seal_wrap(source, data_key.as_slice(), &vault_id, kdf)?);
        }
        plaintext.zeroize();
        Ok(Self {
            vault_id,
            wraps,
            nonce: STANDARD.encode(body_nonce.as_slice()),
            ciphertext: STANDARD.encode(ciphertext.as_slice()),
        })
    }

    /// A standalone sealed copy of `content` for a local export, opened only
    /// by the sync passphrase (R9): independent of any WebDAV password, and
    /// never an unencrypted or keyless file.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::EmptySecret`] for a blank passphrase and the
    /// errors of [`Self::seal`].
    pub fn export_copy(
        content: &VaultContent,
        passphrase: &SecretString,
        kdf: KdfParams,
    ) -> Result<Self, VaultStoreError> {
        Self::seal(
            content,
            None,
            &[KeySource::Passphrase {
                purpose: PassphrasePurpose::SyncPassphrase,
                value: passphrase.clone(),
            }],
            kdf,
        )
    }

    /// Opens the vault with one source. Trying a source the ciphertext has no
    /// wrap for fails the same way a wrong secret does.
    ///
    /// # Errors
    ///
    /// Returns [`VaultStoreError::Sealed`] when the secret is wrong or the
    /// sealed bytes were changed — the two cannot be told apart — and
    /// [`VaultStoreError::Corrupt`] when the block is not one this build
    /// wrote.
    pub fn open(&self, source: &KeySource) -> Result<VaultContent, VaultStoreError> {
        self.validate()
            .map_err(|reason| VaultStoreError::Corrupt { reason })?;
        let data_key = self.unwrap_data_key(source)?;
        let corrupt = |reason: String| VaultStoreError::Corrupt { reason };
        let nonce = decode_field("nonce", &self.nonce, NONCE_LEN).map_err(corrupt)?;
        let ciphertext = decode("ciphertext", &self.ciphertext).map_err(corrupt)?;
        let mut plaintext = open_bytes(
            data_key.as_slice(),
            &nonce,
            &ciphertext,
            &body_aad(&self.vault_id),
        )
        .map_err(|_| VaultStoreError::Sealed)?;
        content::decode(plaintext.as_mut())
    }

    /// Checks the shape without any secret: the binary fields decode, the
    /// costs are bounded, the identity is usable, and there is at least one
    /// wrap and at most one local wrap.
    ///
    /// # Errors
    ///
    /// Returns the reason the block cannot be used.
    pub fn validate(&self) -> Result<(), String> {
        if self.vault_id.trim().is_empty() || self.vault_id.len() > 64 {
            return Err("the sealed vault has no usable identity".to_owned());
        }
        if self.wraps.is_empty() {
            return Err("the sealed vault names no way to unseal itself".to_owned());
        }
        let mut local_wraps = 0;
        for wrap in &self.wraps {
            match wrap {
                WrapRecord::Local { nonce, ciphertext } => {
                    local_wraps += 1;
                    decode_field("nonce of a local wrap", nonce, NONCE_LEN)?;
                    decode_min(
                        "sealed data key of a local wrap",
                        ciphertext,
                        MIN_CIPHERTEXT_LEN,
                    )?;
                }
                WrapRecord::Passphrase {
                    purpose: _,
                    kdf,
                    salt,
                    nonce,
                    ciphertext,
                } => {
                    if kdf.algorithm != KDF_ARGON2ID {
                        return Err(format!(
                            "a wrap uses the key derivation \"{}\", which this build does not read",
                            kdf.algorithm
                        ));
                    }
                    kdf.params().map_err(|error| error.to_string())?;
                    let salt = decode("salt of a passphrase wrap", salt)?;
                    if salt.len() < MIN_SALT_LEN || salt.len() > MAX_SALT_LEN {
                        return Err(
                            "the salt of a passphrase wrap has an unusable length".to_owned()
                        );
                    }
                    decode_field("nonce of a passphrase wrap", nonce, NONCE_LEN)?;
                    decode_min(
                        "sealed data key of a passphrase wrap",
                        ciphertext,
                        MIN_CIPHERTEXT_LEN,
                    )?;
                }
            }
        }
        if local_wraps > 1 {
            return Err("the sealed vault names its local secret twice".to_owned());
        }
        decode_field("nonce", &self.nonce, NONCE_LEN)?;
        decode_min("ciphertext", &self.ciphertext, MIN_CIPHERTEXT_LEN)?;
        Ok(())
    }

    /// Unseals the data key with one source, trying every wrap that could
    /// match it. A failure of a single wrap is not a failure of the vault:
    /// another wrap may still open it, which is the point of having several.
    fn unwrap_data_key(&self, source: &KeySource) -> Result<Zeroizing<Vec<u8>>, VaultStoreError> {
        let corrupt = |reason: String| VaultStoreError::Corrupt { reason };
        match source {
            KeySource::LocalMachine(secret) => {
                let kek = decode_local_key(secret)?;
                for wrap in &self.wraps {
                    let WrapRecord::Local { nonce, ciphertext } = wrap else {
                        continue;
                    };
                    if let Some(key) = self.try_wrap(
                        decode_field("nonce of a local wrap", nonce, NONCE_LEN)
                            .map_err(corrupt)?
                            .as_slice(),
                        decode("sealed data key of a local wrap", ciphertext)
                            .map_err(corrupt)?
                            .as_slice(),
                        kek.as_slice(),
                        &wrap_aad(&self.vault_id, WRAP_LOCAL, ""),
                    )? {
                        return Ok(key);
                    }
                }
            }
            KeySource::Passphrase { purpose, value } => {
                if value.expose().trim().is_empty() {
                    return Err(VaultStoreError::EmptySecret {
                        which: purpose.which(),
                    });
                }
                for wrap in &self.wraps {
                    let WrapRecord::Passphrase {
                        purpose: wrap_purpose,
                        kdf,
                        salt,
                        nonce,
                        ciphertext,
                    } = wrap
                    else {
                        continue;
                    };
                    // A wrap of another purpose is not this secret's business;
                    // its tag would fail anyway, and skipping it keeps the
                    // purposes from opening each other's wraps even by luck.
                    if wrap_purpose != purpose {
                        continue;
                    }
                    let params = kdf.params()?;
                    let salt = decode("salt of a passphrase wrap", salt).map_err(corrupt)?;
                    let kek = derive_kek(value, &salt, params)
                        .map_err(|reason| VaultStoreError::Encrypt { reason })?;
                    if let Some(key) = self.try_wrap(
                        decode_field("nonce of a passphrase wrap", nonce, NONCE_LEN)
                            .map_err(corrupt)?
                            .as_slice(),
                        decode("sealed data key of a passphrase wrap", ciphertext)
                            .map_err(corrupt)?
                            .as_slice(),
                        kek.as_slice(),
                        &wrap_aad(&self.vault_id, WRAP_PASSPHRASE, purpose.as_str()),
                    )? {
                        return Ok(key);
                    }
                }
            }
        }
        Err(VaultStoreError::Sealed)
    }

    /// One attempt at one wrap: `None` is a tag failure, which the caller
    /// treats as "not this wrap", and the errors are shape problems a
    /// validated block does not have.
    fn try_wrap(
        &self,
        nonce: &[u8],
        ciphertext: &[u8],
        kek: &[u8],
        aad: &[u8],
    ) -> Result<Option<Zeroizing<Vec<u8>>>, VaultStoreError> {
        match open_bytes(kek, nonce, ciphertext, aad) {
            Ok(key) => {
                if key.len() != KEY_LEN {
                    return Err(VaultStoreError::Corrupt {
                        reason: "a wrap opened to a data key of the wrong length".to_owned(),
                    });
                }
                Ok(Some(key))
            }
            Err(_) => Ok(None),
        }
    }
}

/// The format version bound into every authentication tag. It is the version
/// the cipher was introduced with and stays 2 for good: the file envelope
/// moved on to format 3 for the host passwords in the body, but the cipher did
/// not change, and binding the envelope's newer number would make every
/// existing vault and every synced block fail its tag.
const CIPHER_BINDING_VERSION: u32 = 2;

const WRAP_LOCAL: &str = "local";
const WRAP_PASSPHRASE: &str = "passphrase";

/// What the body's tag is bound to: the format, the vault, and the fact that
/// these bytes are a record body and not a wrap.
fn body_aad(vault_id: &str) -> Vec<u8> {
    format!("shellune-vault/{CIPHER_BINDING_VERSION}/{vault_id}/body").into_bytes()
}

/// What a wrap's tag is bound to: the format, the vault, the wrap kind and the
/// purpose the wrap answers to.
fn wrap_aad(vault_id: &str, kind: &str, purpose: &str) -> Vec<u8> {
    format!("shellune-vault/{CIPHER_BINDING_VERSION}/{vault_id}/wrap/{kind}/{purpose}").into_bytes()
}

fn seal_wrap(
    source: &KeySource,
    data_key: &[u8],
    vault_id: &str,
    kdf: KdfParams,
) -> Result<WrapRecord, VaultStoreError> {
    match source {
        KeySource::LocalMachine(secret) => {
            let kek = decode_local_key(secret)?;
            let nonce = random(NONCE_LEN)?;
            let ciphertext = seal_bytes(
                kek.as_slice(),
                nonce.as_slice(),
                data_key,
                &wrap_aad(vault_id, WRAP_LOCAL, ""),
            )
            .map_err(|reason| VaultStoreError::Encrypt { reason })?;
            Ok(WrapRecord::Local {
                nonce: STANDARD.encode(nonce.as_slice()),
                ciphertext: STANDARD.encode(ciphertext.as_slice()),
            })
        }
        KeySource::Passphrase { purpose, value } => {
            if value.expose().trim().is_empty() {
                return Err(VaultStoreError::EmptySecret {
                    which: purpose.which(),
                });
            }
            let salt = random(SALT_LEN)?;
            let kek = derive_kek(value, salt.as_slice(), kdf)
                .map_err(|reason| VaultStoreError::Encrypt { reason })?;
            let nonce = random(NONCE_LEN)?;
            let ciphertext = seal_bytes(
                kek.as_slice(),
                nonce.as_slice(),
                data_key,
                &wrap_aad(vault_id, WRAP_PASSPHRASE, purpose.as_str()),
            )
            .map_err(|reason| VaultStoreError::Encrypt { reason })?;
            Ok(WrapRecord::Passphrase {
                purpose: *purpose,
                kdf: WrapKdf {
                    algorithm: KDF_ARGON2ID.to_owned(),
                    memory_kib: kdf.memory_kib(),
                    iterations: kdf.iterations(),
                    parallelism: kdf.parallelism(),
                },
                salt: STANDARD.encode(salt.as_slice()),
                nonce: STANDARD.encode(nonce.as_slice()),
                ciphertext: STANDARD.encode(ciphertext.as_slice()),
            })
        }
    }
}

/// Draws a fresh local unseal secret: 32 bytes from the system random source,
/// base64 for the system credential store.
pub(super) fn generate_local_secret() -> Result<SecretString, VaultStoreError> {
    let raw = random(KEY_LEN)?;
    Ok(SecretString::new(STANDARD.encode(raw.as_slice())))
}

/// Draws a fresh vault identity: 16 bytes of the system random source, hex.
fn random_vault_id() -> Result<String, VaultStoreError> {
    let raw = random(16)?;
    Ok(raw.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn random(len: usize) -> Result<Zeroizing<Vec<u8>>, VaultStoreError> {
    let mut bytes = Zeroizing::new(vec![0_u8; len]);
    getrandom::fill(bytes.as_mut()).map_err(|error| VaultStoreError::RandomSource {
        reason: error.to_string(),
    })?;
    Ok(bytes)
}

fn checked_kdf(kdf: KdfParams) -> Result<KdfParams, VaultStoreError> {
    KdfParams::new(kdf.memory_kib(), kdf.iterations(), kdf.parallelism()).ok_or_else(|| {
        VaultStoreError::Encrypt {
            reason: "the key derivation cost is outside what this build will spend".to_owned(),
        }
    })
}

fn derive_kek(
    passphrase: &SecretString,
    salt: &[u8],
    params: KdfParams,
) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    let params = Params::new(
        params.memory_kib(),
        params.iterations(),
        params.parallelism(),
        Some(KEY_LEN),
    )
    .map_err(|error| error.to_string())?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0_u8; KEY_LEN]);
    argon2
        .hash_password_into(passphrase.expose().as_bytes(), salt, key.as_mut())
        .map_err(|error| error.to_string())?;
    Ok(key)
}

fn seal_bytes(
    key: &[u8],
    nonce: &[u8],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|error| error.to_string())?;
    let nonce = Nonce::<Aes256Gcm>::try_from(nonce).map_err(|error| error.to_string())?;
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| "the bytes could not be sealed".to_owned())
}

fn open_bytes(
    key: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|error| error.to_string())?;
    let nonce = Nonce::<Aes256Gcm>::try_from(nonce).map_err(|error| error.to_string())?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| "the authentication tag did not match".to_owned())
}

/// The local unseal secret as key bytes. A stored secret of another shape is a
/// machine problem, not a wrong secret: it is named instead of guessed at.
fn decode_local_key(secret: &SecretString) -> Result<Zeroizing<Vec<u8>>, VaultStoreError> {
    let raw =
        Zeroizing::new(
            STANDARD
                .decode(secret.expose())
                .map_err(|_| VaultStoreError::Invalid {
                    reason: "the local unseal secret kept by the system store is not usable"
                        .to_owned(),
                })?,
        );
    if raw.len() != KEY_LEN {
        return Err(VaultStoreError::Invalid {
            reason: "the local unseal secret kept by the system store is not usable".to_owned(),
        });
    }
    Ok(raw)
}

fn decode(field: &str, text: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let bytes = STANDARD
        .decode(text)
        .map_err(|_| format!("the {field} of the sealed vault is not base64"))?;
    Ok(Zeroizing::new(bytes))
}

fn decode_field(field: &str, text: &str, len: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    let bytes = decode(field, text)?;
    if bytes.len() != len {
        return Err(format!(
            "the {field} of the sealed vault has an unusable length"
        ));
    }
    Ok(bytes)
}

fn decode_min(field: &str, text: &str, len: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    let bytes = decode(field, text)?;
    if bytes.len() < len {
        return Err(format!(
            "the {field} of the sealed vault is shorter than its own tag"
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shellune_core::{CredentialId, CredentialMeta, VaultRecordVersion};

    use super::super::content::VaultCredentialRecord;

    /// Opens fail alike; asserted once here so the cases below can be short.
    fn assert_sealed(cipher: &VaultCipher, source: &KeySource) {
        assert!(
            matches!(cipher.open(source), Err(VaultStoreError::Sealed)),
            "the wrong secret and the changed bytes fail alike"
        );
    }

    fn cheap_kdf() -> KdfParams {
        KdfParams::new(64, 1, 1).expect("valid parameters")
    }

    fn content() -> VaultContent {
        let mut content = VaultContent::new();
        content
            .insert_credential(VaultCredentialRecord::new(
                CredentialMeta::new(
                    CredentialId::new(),
                    "Prod login",
                    Some("deploy".to_owned()),
                    VaultRecordVersion::new(1, "device-a"),
                ),
                SecretString::new("hunter2"),
            ))
            .expect("inserts");
        content
    }

    fn assert_opens(cipher: &VaultCipher, source: &KeySource) {
        let opened = cipher.open(source).expect("opens");
        assert_eq!(opened.credentials().count(), 1);
    }

    #[test]
    fn every_source_seals_and_only_its_own_secret_opens() {
        let content = content();
        let sources = vec![
            KeySource::LocalMachine(generate_local_secret().expect("a local secret")),
            KeySource::Passphrase {
                purpose: PassphrasePurpose::WebDavPassword,
                value: SecretString::new("the webdav password"),
            },
            KeySource::Passphrase {
                purpose: PassphrasePurpose::SyncPassphrase,
                value: SecretString::new("the sync passphrase"),
            },
        ];
        let cipher = VaultCipher::seal(&content, Some("0123456789abcdef"), &sources, cheap_kdf())
            .expect("seals");
        assert_eq!(cipher.wraps().len(), 3);

        for source in &sources {
            assert_opens(&cipher, source);
        }

        // A wrong secret, a secret of the right kind and wrong value, and a
        // source with no matching wrap at all all fail the same way.
        let wrong = KeySource::Passphrase {
            purpose: PassphrasePurpose::WebDavPassword,
            value: SecretString::new("not the webdav password"),
        };
        assert_sealed(&cipher, &wrong);
        // The WebDAV password does not open the sync wrap: the purposes are
        // bound into the tag.
        let crossed = KeySource::Passphrase {
            purpose: PassphrasePurpose::SyncPassphrase,
            value: SecretString::new("the webdav password"),
        };
        assert_sealed(&cipher, &crossed);
    }

    #[test]
    fn two_sealings_of_the_same_content_share_no_ciphertext() {
        let content = content();
        let source = KeySource::Passphrase {
            purpose: PassphrasePurpose::SyncPassphrase,
            value: SecretString::new("shared"),
        };
        let first = VaultCipher::seal(
            &content,
            Some("id"),
            std::slice::from_ref(&source),
            cheap_kdf(),
        )
        .expect("seals");
        let second =
            VaultCipher::seal(&content, Some("id"), &[source], cheap_kdf()).expect("seals");
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_ne!(first.nonce, second.nonce);
        if let WrapRecord::Passphrase { salt, .. } = &second.wraps[0] {
            assert_ne!(salt, first.wraps[0].as_passphrase_salt());
        } else {
            panic!("the wrap is a passphrase wrap");
        }
    }

    #[test]
    fn an_empty_passphrase_is_refused_and_a_blank_identity_is_replaced() {
        let content = content();
        let blank = KeySource::Passphrase {
            purpose: PassphrasePurpose::SyncPassphrase,
            value: SecretString::new("   "),
        };
        assert_eq!(
            VaultCipher::seal(&content, None, &[blank], cheap_kdf()),
            Err(VaultStoreError::EmptySecret {
                which: "sync passphrase"
            })
        );
        assert!(
            matches!(
                VaultCipher::seal(&content, None, &[], cheap_kdf()),
                Err(VaultStoreError::Invalid { .. })
            ),
            "a vault with no way to unseal it is refused"
        );

        // Without an identity a fresh one is drawn and both are usable.
        let source = KeySource::LocalMachine(generate_local_secret().expect("secret"));
        let cipher = VaultCipher::seal(&content, None, &[source], cheap_kdf()).expect("seals");
        assert_eq!(cipher.vault_id().len(), 32);
    }

    #[test]
    fn a_changed_body_or_wrap_fails_and_the_costs_are_bounded() {
        let content = content();
        let source = KeySource::Passphrase {
            purpose: PassphrasePurpose::WebDavPassword,
            value: SecretString::new("webdav"),
        };
        let mut cipher = VaultCipher::seal(
            &content,
            Some("id"),
            std::slice::from_ref(&source),
            cheap_kdf(),
        )
        .expect("seals");

        // A flipped bit in the body fails the whole open.
        let mut bytes = STANDARD
            .decode(&cipher.ciphertext)
            .expect("the ciphertext is base64");
        bytes[0] ^= 0xff;
        cipher.ciphertext = STANDARD.encode(bytes);
        assert_sealed(&cipher, &source);

        // A wrap that asks for an unbounded derivation is refused before any
        // secret is tried.
        let mut greedy = cipher.clone();
        if let WrapRecord::Passphrase { kdf, .. } = &mut greedy.wraps[0] {
            kdf.memory_kib = 512 * 1024;
        }
        assert!(greedy.validate().is_err());
        assert!(matches!(
            greedy.open(&source),
            Err(VaultStoreError::Corrupt { .. })
        ));
    }

    /// Test-only reach into the wrap's salt, for the ciphertext comparison.
    impl WrapRecord {
        fn as_passphrase_salt(&self) -> &str {
            match self {
                WrapRecord::Passphrase { salt, .. } => salt,
                WrapRecord::Local { .. } => "",
            }
        }
    }
}
