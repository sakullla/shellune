//! Platforms this build ships no system verification adapter for: it says so
//! instead of pretending one exists.

use super::{BiometricAuthenticator, BiometricCapability, BiometricError, VerificationPrompt};

/// A system with no verification this build can call.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAuthenticator;

impl BiometricAuthenticator for SystemAuthenticator {
    fn capability(&self) -> BiometricCapability {
        BiometricCapability::Unavailable
    }

    fn verify(&self, _prompt: &VerificationPrompt) -> Result<(), BiometricError> {
        Err(BiometricError::Unavailable {
            reason: "this platform has no system verification adapter".to_owned(),
        })
    }
}
