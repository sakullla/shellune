//! Linux: the Secret Service is the system's user verification, and no
//! biometric claim is made.
//!
//! Reading a locked collection is what asks the user to unlock — the prompt
//! the desktop shows may be a password or, where the desktop wired it into
//! that flow, a fingerprint. That prompt belongs to the credential store
//! ([`crate::SecretStore`]), not to a standalone verification this module
//! could raise, so [`verify`](SystemAuthenticator::verify) reports
//! [`BiometricError::Unavailable`] and the caller falls back to the current
//! passphrase or the store's own unlock. This build reads no fprintd device,
//! stores no fingerprint, and does not present the three platforms as having
//! the same strength.

use super::{BiometricAuthenticator, BiometricCapability, BiometricError, VerificationPrompt};

/// The desktop's own unlock prompt, reached through the credential store.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAuthenticator;

impl BiometricAuthenticator for SystemAuthenticator {
    fn capability(&self) -> BiometricCapability {
        // The desktop's keyring unlock prompt is the user verification this
        // platform offers. Whether it actually appears is the desktop's
        // decision at read time; a machine without a Secret Service fails in
        // the credential store, where the passphrase fallback already lives.
        BiometricCapability::SystemUserVerification
    }

    fn verify(&self, _prompt: &VerificationPrompt) -> Result<(), BiometricError> {
        Err(BiometricError::Unavailable {
            reason: "the Secret Service asks for the user itself when a locked collection \
                     is read; there is no separate prompt to raise"
                .to_owned(),
        })
    }
}
