//! Saved passwords and passphrases, kept in the system credential store.
//!
//! The host file never holds a secret: it rejects any field it does not know, and a
//! password is not one of them. A host whose password the user asked Shellune to
//! remember keeps only a flag; the secret itself goes to the store the operating system
//! provides for exactly this — the Windows Credential Manager, the macOS keychain, or
//! the Secret Service on Linux — under the host's id.
//!
//! [`SecretStore`] is the boundary, so the window can be tested with
//! [`MemorySecrets`] and never touches the real store of the machine running the tests.

use std::collections::HashMap;
use std::sync::Mutex;

use shellune_core::{HostId, SecretString};

/// The service name entries are filed under in the system store.
pub const SERVICE: &str = "com.shellune.Shellune";

/// Why the credential store could not do what was asked.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the system credential store {action}: {reason}")]
pub struct SecretError {
    action: &'static str,
    reason: String,
}

impl SecretError {
    fn new(action: &'static str, reason: impl Into<String>) -> Self {
        Self {
            action,
            reason: reason.into(),
        }
    }
}

/// Where host secrets are kept.
pub trait SecretStore: Send + Sync {
    /// Saves the secret of one host, replacing any earlier one.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused the write.
    fn save(&self, host: HostId, secret: &SecretString) -> Result<(), SecretError>;

    /// The secret saved for one host; `Ok(None)` only when the store confirms there is
    /// none. A store that cannot be read answers `Err`, never "nothing saved".
    ///
    /// # Errors
    ///
    /// The store is unavailable or could not be read.
    fn load(&self, host: HostId) -> Result<Option<SecretString>, SecretError>;

    /// Forgets the secret of one host. Forgetting one that was never saved succeeds.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused the deletion.
    fn forget(&self, host: HostId) -> Result<(), SecretError>;
}

/// The operating system's credential store.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSecrets;

impl SystemSecrets {
    fn entry(host: HostId) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, &format!("host-{host}"))
            .map_err(|error| SecretError::new("is unavailable", error.to_string()))
    }
}

impl SecretStore for SystemSecrets {
    fn save(&self, host: HostId, secret: &SecretString) -> Result<(), SecretError> {
        Self::entry(host)?
            .set_password(secret.expose())
            .map_err(|error| SecretError::new("could not save the password", error.to_string()))
    }

    fn load(&self, host: HostId) -> Result<Option<SecretString>, SecretError> {
        match Self::entry(host)?.get_password() {
            Ok(secret) => Ok(Some(SecretString::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError::new(
                "could not read the password",
                error.to_string(),
            )),
        }
    }

    fn forget(&self, host: HostId) -> Result<(), SecretError> {
        match Self::entry(host)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretError::new(
                "could not delete the password",
                error.to_string(),
            )),
        }
    }
}

/// Secrets kept in memory only, for tests.
#[derive(Debug, Default)]
pub struct MemorySecrets {
    secrets: Mutex<HashMap<HostId, String>>,
    /// When set, every call fails, as an unavailable system store does.
    unavailable: bool,
}

impl MemorySecrets {
    /// An empty, working store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that fails every call, like a machine without a credential service.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::default()
        }
    }

    /// How many secrets are saved.
    #[must_use]
    pub fn len(&self) -> usize {
        self.secrets.lock().map_or(0, |secrets| secrets.len())
    }

    /// Whether nothing is saved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check(&self, action: &'static str) -> Result<(), SecretError> {
        if self.unavailable {
            Err(SecretError::new(action, "no credential service is running"))
        } else {
            Ok(())
        }
    }
}

impl SecretStore for MemorySecrets {
    fn save(&self, host: HostId, secret: &SecretString) -> Result<(), SecretError> {
        self.check("could not save the password")?;
        self.secrets
            .lock()
            .map_err(|_| SecretError::new("could not save the password", "poisoned"))?
            .insert(host, secret.expose().to_owned());
        Ok(())
    }

    fn load(&self, host: HostId) -> Result<Option<SecretString>, SecretError> {
        self.check("could not read the password")?;
        Ok(self
            .secrets
            .lock()
            .map_err(|_| SecretError::new("could not read the password", "poisoned"))?
            .get(&host)
            .map(|secret| SecretString::new(secret.clone())))
    }

    fn forget(&self, host: HostId) -> Result<(), SecretError> {
        self.check("could not delete the password")?;
        self.secrets
            .lock()
            .map_err(|_| SecretError::new("could not delete the password", "poisoned"))?
            .remove(&host);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_secret_is_read_back_and_forgotten() {
        let store = MemorySecrets::new();
        let host = HostId::new();
        assert_eq!(store.load(host).map(|secret| secret.is_some()), Ok(false));
        store
            .save(host, &SecretString::new("hunter2"))
            .expect("saved");
        let loaded = store.load(host).expect("read").expect("present");
        assert_eq!(loaded.expose(), "hunter2");
        store.forget(host).expect("forgotten");
        assert_eq!(store.load(host).map(|secret| secret.is_some()), Ok(false));
        store
            .forget(host)
            .expect("forgetting twice is not an error");
    }

    #[test]
    fn an_unavailable_store_never_reads_as_empty() {
        let store = MemorySecrets::unavailable();
        let host = HostId::new();
        assert!(
            store.load(host).is_err(),
            "a failure is not \"nothing saved\""
        );
        assert!(store.save(host, &SecretString::new("x")).is_err());
    }
}
