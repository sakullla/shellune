//! Device-local biometric and system user verification (R3, R4).
//!
//! The boundary is [`BiometricAuthenticator`]: the application asks the
//! operating system to verify the user and receives a result it can branch
//! on. It never sees, stores or syncs a fingerprint template. Two entry
//! points exist on purpose:
//!
//! * [`BiometricAuthenticator::capability`] is silent. It answers from system
//!   state alone and never raises a prompt, so a lock screen deciding whether
//!   it may clear, or an automatic background sync deciding whether to defer
//!   to the next unlock, can ask without surprising the user.
//! * [`BiometricAuthenticator::verify`] prompts. Every call is a fresh,
//!   independent verification: a success is never carried into the next
//!   operation, and nothing here caches or replays a result.
//!
//! The error model keeps apart the outcomes a caller must treat differently:
//! no capability, nothing enrolled, the user backing out, a verification that
//! did not match, a system that cannot ask right now, and an enrollment
//! change that invalidated this machine's protection ([`BiometricError`]).
//!
//! [`SystemAuthenticator`] is whichever implementation the current platform
//! provides; [`ScriptedAuthenticator`] is the programmable double the tests
//! (and the window tests of R3/R4) run against, so no test ever raises a real
//! system prompt. The lock policy itself is device-local state in
//! [`crate::SecurityStore`]; it never enters a portable document.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::SystemAuthenticator;
#[cfg(target_os = "macos")]
pub use macos::{BiometryProtectedEntry, SystemAuthenticator};
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub use unsupported::SystemAuthenticator;
#[cfg(target_os = "windows")]
pub use windows::SystemAuthenticator;

/// What the system can verify the user with, asked silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiometricCapability {
    /// The system verifies the user with biometrics it owns — Touch ID, or
    /// a Windows Hello sensor. The application learns only success or
    /// failure, never a template.
    Biometrics,
    /// The system can re-verify the user, but not with biometrics: a login
    /// password, a PIN, or the desktop's keyring unlock prompt.
    SystemUserVerification,
    /// Nothing the application can call.
    Unavailable,
}

impl BiometricCapability {
    /// Whether an operation may be gated on a system verification at all.
    #[must_use]
    pub const fn can_verify_user(&self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// Whether the stronger, biometric gate is available.
    #[must_use]
    pub const fn is_biometric(&self) -> bool {
        matches!(self, Self::Biometrics)
    }
}

/// How a machine without biometrics gates a high-risk operation in the
/// local-only, no-passphrase mode (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalOnlyFallback {
    /// The system can re-verify the user itself; ask it to.
    SystemUserVerification,
    /// The system offers nothing to call: continue on the already unlocked
    /// session, behind an explicit risk confirmation, and do not describe
    /// the result as biometric-strength.
    UnlockedSessionWithConfirmation,
}

impl LocalOnlyFallback {
    /// The fallback a capability allows. Biometrics and system verification
    /// both mean the system can ask; anything else means the unlocked
    /// session is all that is left.
    #[must_use]
    pub const fn for_capability(capability: BiometricCapability) -> Self {
        match capability {
            BiometricCapability::Biometrics | BiometricCapability::SystemUserVerification => {
                Self::SystemUserVerification
            }
            BiometricCapability::Unavailable => Self::UnlockedSessionWithConfirmation,
        }
    }
}

/// What the system prompt explains to the user.
///
/// The reason is the whole prompt: the system adds its own chrome (whose
/// fingerprint is being checked, how to cancel) and this build adds nothing
/// on top, least of all a promise the system did not make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationPrompt {
    reason: String,
}

impl VerificationPrompt {
    /// A prompt explaining why the user is being asked.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The localized reason handed to the system prompt.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Why a system verification did not succeed.
///
/// The variants are kept apart because callers treat them differently: a
/// cancellation asks again later, a missing capability switches the machine
/// to a fallback, and an enrollment change disables this machine's biometric
/// protection until the user re-enables it with the current passphrase.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BiometricError {
    /// The system offers no verification this application can call: no
    /// hardware, no such service, or the platform never grew one.
    #[error("the system offers no verification this application can call: {reason}")]
    Unavailable {
        /// What the system said, in the system's own terms.
        reason: String,
    },
    /// The hardware exists but the user has nothing enrolled.
    #[error("no biometrics are enrolled on this device: {reason}")]
    NotEnrolled {
        /// What the system said, in the system's own terms.
        reason: String,
    },
    /// The user, the system or the application cancelled before the user was
    /// verified, including the user choosing the system's own password
    /// fallback. Not a failure, and never a pass.
    #[error("verification was cancelled before the user was verified")]
    Cancelled,
    /// Verification ran and did not match the user.
    #[error("verification did not match the user")]
    Failed,
    /// The system could not ask right now: busy, locked out for a while, or
    /// not in a state where a prompt is allowed.
    #[error("verification could not run right now: {reason}")]
    Busy {
        /// What the system said, in the system's own terms.
        reason: String,
    },
    /// The enrolled biometrics changed, so the protection this machine saved
    /// (a keychain item under the previous set) no longer holds. Biometric
    /// unlocking must be disabled here until the user re-unlocks with the
    /// current passphrase and enables it again.
    #[error(
        "the enrolled biometrics changed; this machine's biometric protection was reset: {reason}"
    )]
    EnrollmentChanged {
        /// What the system said, in the system's own terms.
        reason: String,
    },
}

impl BiometricError {
    /// Whether this is the enrollment change that must disable the
    /// machine's biometric protection.
    #[must_use]
    pub const fn is_enrollment_changed(&self) -> bool {
        matches!(self, Self::EnrollmentChanged { .. })
    }

    /// Whether the user backed out before being verified.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// The system's user verification, behind one interface.
///
/// The implementing type never sees a fingerprint template, and the results
/// it hands back are not cached anywhere: each [`verify`](Self::verify) call
/// stands on its own, so one success can never become the next operation's
/// pass.
pub trait BiometricAuthenticator: Send + Sync {
    /// Asks, silently, what this system could verify the user with.
    ///
    /// This never raises a prompt. It is the entry point for decisions a
    /// prompt must not interrupt — a lock screen clearing, and an automatic
    /// background sync that defers instead of surprising the user with a
    /// system verification while the vault is locked.
    #[must_use]
    fn capability(&self) -> BiometricCapability;

    /// Verifies the user, once, for one operation.
    ///
    /// Each call is independent: a fresh system verification with nothing
    /// carried over from the previous one. Callers on a window thread should
    /// hand this to a background task; some implementations wait for the
    /// user to answer and block until they do.
    ///
    /// # Errors
    ///
    /// See [`BiometricError`]: no capability, nothing enrolled, a
    /// cancellation, a mismatch, a system that cannot ask right now, or an
    /// enrollment change that reset this machine's protection.
    fn verify(&self, prompt: &VerificationPrompt) -> Result<(), BiometricError>;
}

/// A programmable stand-in for the system verifier, for tests.
///
/// The script is a list of verification outcomes. Each
/// [`verify`](BiometricAuthenticator::verify) call takes the next one, so a
/// test states exactly how many prompts the user answered and what each of
/// them did — which is the only way to show that a success is never replayed.
/// An exhausted script refuses rather than inventing a prompt, and
/// [`capability`](BiometricAuthenticator::capability) never consumes a step:
/// it is the silent query.
#[derive(Debug)]
pub struct ScriptedAuthenticator {
    capability: BiometricCapability,
    script: Mutex<VecDeque<Result<(), BiometricError>>>,
    verify_calls: AtomicUsize,
    capability_queries: AtomicUsize,
}

impl ScriptedAuthenticator {
    /// A system with this capability and an empty script.
    #[must_use]
    pub fn new(capability: BiometricCapability) -> Self {
        Self {
            capability,
            script: Mutex::new(VecDeque::new()),
            verify_calls: AtomicUsize::new(0),
            capability_queries: AtomicUsize::new(0),
        }
    }

    /// Queues the outcomes of the next verifications, in order.
    #[must_use]
    pub fn script(self, steps: impl IntoIterator<Item = Result<(), BiometricError>>) -> Self {
        *self
            .script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = steps.into_iter().collect();
        self
    }

    /// How many verifications were asked for.
    #[must_use]
    pub fn verify_calls(&self) -> usize {
        self.verify_calls.load(Ordering::Relaxed)
    }

    /// How many times the capability was queried silently.
    #[must_use]
    pub fn capability_queries(&self) -> usize {
        self.capability_queries.load(Ordering::Relaxed)
    }

    /// How many scripted verifications have not run yet.
    #[must_use]
    pub fn remaining_steps(&self) -> usize {
        self.lock_script().len()
    }

    fn lock_script(&self) -> std::sync::MutexGuard<'_, VecDeque<Result<(), BiometricError>>> {
        // A panicked test thread is the only way this lock is poisoned, and
        // the queue it guards is still consistent for the next step.
        self.script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl BiometricAuthenticator for ScriptedAuthenticator {
    fn capability(&self) -> BiometricCapability {
        self.capability_queries.fetch_add(1, Ordering::Relaxed);
        self.capability
    }

    fn verify(&self, _prompt: &VerificationPrompt) -> Result<(), BiometricError> {
        self.verify_calls.fetch_add(1, Ordering::Relaxed);
        self.lock_script().pop_front().unwrap_or_else(|| {
            Err(BiometricError::Unavailable {
                reason: "the scripted authenticator has no verification left to run".to_owned(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_enrollment_change_and_a_cancellation_are_recognizable() {
        assert!(
            BiometricError::EnrollmentChanged {
                reason: "fingerprints re-enrolled".to_owned()
            }
            .is_enrollment_changed()
        );
        assert!(!BiometricError::Cancelled.is_enrollment_changed());
        assert!(BiometricError::Cancelled.is_cancelled());
        assert!(!BiometricError::Failed.is_cancelled());
    }

    #[test]
    fn the_capability_order_knows_what_can_ask() {
        assert!(BiometricCapability::Biometrics.can_verify_user());
        assert!(BiometricCapability::Biometrics.is_biometric());
        assert!(BiometricCapability::SystemUserVerification.can_verify_user());
        assert!(!BiometricCapability::SystemUserVerification.is_biometric());
        assert!(!BiometricCapability::Unavailable.can_verify_user());
        assert!(!BiometricCapability::Unavailable.is_biometric());
    }

    #[test]
    fn a_local_only_machine_falls_back_by_what_the_system_can_ask() {
        assert_eq!(
            LocalOnlyFallback::for_capability(BiometricCapability::Biometrics),
            LocalOnlyFallback::SystemUserVerification
        );
        assert_eq!(
            LocalOnlyFallback::for_capability(BiometricCapability::SystemUserVerification),
            LocalOnlyFallback::SystemUserVerification
        );
        assert_eq!(
            LocalOnlyFallback::for_capability(BiometricCapability::Unavailable),
            LocalOnlyFallback::UnlockedSessionWithConfirmation
        );
    }
}
