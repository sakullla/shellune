//! macOS: LocalAuthentication is the verification gate, and the keychain
//! holds the local unseal secret behind an access control tied to the
//! currently enrolled biometric set.
//!
//! The application never sees a fingerprint template: it asks the system to
//! verify the user and receives only a result. Every verification creates a
//! fresh `LAContext`, so a successful verification can never be replayed for
//! the next operation.
//!
//! [`BiometryProtectedEntry`] is how an enrollment change surfaces: an item
//! saved under `biometryCurrentSet` stops working once the enrolled set
//! changes, and the failed read is reported as
//! [`BiometricError::EnrollmentChanged`] so the caller disables biometric
//! unlocking on this machine and asks for the current passphrase instead.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool as ObjCBool;
use objc2::{ClassType, extern_class, msg_send};
use objc2_foundation::{NSError, NSString};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password_options,
};
use security_framework::passwords_options::{AccessControlOptions, PasswordOptions};
use shellune_core::SecretString;

use super::{BiometricAuthenticator, BiometricCapability, BiometricError, VerificationPrompt};
use crate::credentials::SERVICE;

// LAContext lives in LocalAuthentication; linking the framework is what makes
// the class visible to the runtime lookup below.
#[link(name = "LocalAuthentication", kind = "framework")]
unsafe extern "C" {}

extern_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[name = "LAContext"]
    struct LAContext;
);

// LAPolicy values (LocalAuthentication/LAPolicy.h).
/// `kLAPolicyDeviceOwnerAuthenticationWithBiometrics`: biometrics only.
const POLICY_BIOMETRICS: isize = 1;
/// `kLAPolicyDeviceOwnerAuthentication`: biometrics or the account password.
const POLICY_DEVICE_OWNER: isize = 2;

// LAError codes (LocalAuthentication/LAError.h), and the domain they arrive
// in.
const LA_ERROR_DOMAIN: &str = "com.apple.LocalAuthentication";
const LA_ERROR_AUTHENTICATION_FAILED: isize = -1;
const LA_ERROR_USER_CANCEL: isize = -2;
const LA_ERROR_USER_FALLBACK: isize = -3;
const LA_ERROR_SYSTEM_CANCEL: isize = -4;
const LA_ERROR_PASSCODE_NOT_SET: isize = -5;
const LA_ERROR_BIOMETRY_NOT_AVAILABLE: isize = -6;
const LA_ERROR_BIOMETRY_NOT_ENROLLED: isize = -7;
const LA_ERROR_BIOMETRY_LOCKOUT: isize = -8;
const LA_ERROR_APP_CANCEL: isize = -9;
const LA_ERROR_INVALID_CONTEXT: isize = -10;
const LA_ERROR_NOT_INTERACTIVE: isize = -11;

// Keychain status codes (Security/SecBase.h) that decide what a failed read
// of the protected item means.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

/// The system's own verification, by way of LocalAuthentication.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAuthenticator;

impl SystemAuthenticator {
    fn new_context() -> Option<Retained<LAContext>> {
        let context: Option<Retained<LAContext>> = unsafe { msg_send![LAContext::class(), new] };
        context
    }

    /// `canEvaluatePolicy:error:` answers from state alone; it shows nothing.
    fn can_evaluate(policy: isize) -> bool {
        Self::new_context().is_some_and(|context| {
            objc2::rc::autoreleasepool(|_| {
                // The error out-parameter is only read for the log this call
                // never writes: the answer is the boolean.
                let mut error: *mut NSError = std::ptr::null_mut();
                let can_evaluate: bool = unsafe {
                    msg_send![
                        &context,
                        canEvaluatePolicy: policy,
                        error: &mut error,
                    ]
                };
                can_evaluate
            })
        })
    }

    fn strongest_policy() -> Option<isize> {
        if Self::can_evaluate(POLICY_BIOMETRICS) {
            Some(POLICY_BIOMETRICS)
        } else if Self::can_evaluate(POLICY_DEVICE_OWNER) {
            Some(POLICY_DEVICE_OWNER)
        } else {
            None
        }
    }
}

impl BiometricAuthenticator for SystemAuthenticator {
    fn capability(&self) -> BiometricCapability {
        match Self::strongest_policy() {
            Some(POLICY_BIOMETRICS) => BiometricCapability::Biometrics,
            Some(_) => BiometricCapability::SystemUserVerification,
            None => BiometricCapability::Unavailable,
        }
    }

    fn verify(&self, prompt: &VerificationPrompt) -> Result<(), BiometricError> {
        let Some(policy) = Self::strongest_policy() else {
            return Err(BiometricError::Unavailable {
                reason: "this system can evaluate no user-verification policy".to_owned(),
            });
        };
        let context = Self::new_context().ok_or_else(|| BiometricError::Unavailable {
            reason: "LocalAuthentication is not present on this system".to_owned(),
        })?;
        let reason = NSString::from_str(prompt.reason());
        let (reply, receiver) = std::sync::mpsc::channel::<(bool, isize, String)>();
        let block = RcBlock::new(move |success: ObjCBool, error: *mut NSError| {
            // The reply arrives on one of the system's queues, so only
            // plain Rust values cross back to the caller's thread.
            let outcome = if success.as_bool() {
                (true, 0, String::new())
            } else if error.is_null() {
                (false, 0, String::new())
            } else {
                let error = unsafe { &*error };
                (false, error.code(), error.domain().to_string())
            };
            let _ = reply.send(outcome);
        });
        // A fresh context and a fresh evaluation: nothing about this call can
        // satisfy the next one.
        let _: () = unsafe {
            msg_send![
                &context,
                evaluatePolicy: policy,
                localizedReason: &*reason,
                reply: &*block,
            ]
        };
        let (success, code, domain) = receiver.recv().map_err(|_| BiometricError::Busy {
            reason: "the system never delivered the verification reply".to_owned(),
        })?;
        if success {
            return Ok(());
        }
        Err(map_error(code, &domain))
    }
}

fn map_error(code: isize, domain: &str) -> BiometricError {
    if !domain.is_empty() && domain != LA_ERROR_DOMAIN {
        return BiometricError::Unavailable {
            reason: format!("verification failed with {code} from {domain}"),
        };
    }
    match code {
        LA_ERROR_USER_CANCEL | LA_ERROR_APP_CANCEL | LA_ERROR_USER_FALLBACK => {
            BiometricError::Cancelled
        }
        LA_ERROR_AUTHENTICATION_FAILED => BiometricError::Failed,
        LA_ERROR_SYSTEM_CANCEL | LA_ERROR_BIOMETRY_LOCKOUT | LA_ERROR_NOT_INTERACTIVE => {
            BiometricError::Busy {
                reason: format!("LocalAuthentication error {code}"),
            }
        }
        LA_ERROR_PASSCODE_NOT_SET | LA_ERROR_BIOMETRY_NOT_AVAILABLE | LA_ERROR_INVALID_CONTEXT => {
            BiometricError::Unavailable {
                reason: format!("LocalAuthentication error {code}"),
            }
        }
        LA_ERROR_BIOMETRY_NOT_ENROLLED => BiometricError::NotEnrolled {
            reason: "no biometrics are enrolled for this user".to_owned(),
        },
        other => BiometricError::Unavailable {
            reason: format!("LocalAuthentication error {other}"),
        },
    }
}

/// The vault's local unseal secret as a keychain item only the current
/// biometric set can open.
///
/// Saved with `kSecAccessControlBiometryCurrentSet`, and that flag alone: the
/// item is valid only while the enrolled set stays as it was, so a
/// re-enrollment breaks this machine's protection on purpose instead of
/// quietly falling back to the device passcode. Reading it is what raises the
/// system's own biometric prompt; when the protection no longer holds, the
/// read fails and is reported as [`BiometricError::EnrollmentChanged`].
pub struct BiometryProtectedEntry {
    account: &'static str,
}

impl BiometryProtectedEntry {
    /// The biometry-protected copy of the vault's local unseal secret. A
    /// separate account from the plain [`crate::AppSecret::VaultLocalUnseal`]
    /// entry, so enabling biometrics never disturbs the passphraseless path.
    #[must_use]
    pub const fn vault_local_unseal() -> Self {
        Self {
            account: "vault-local-unseal-biometry",
        }
    }

    /// Saves the secret under the current biometric set, replacing any
    /// earlier one.
    ///
    /// # Errors
    ///
    /// The keychain refused the write — above all because no biometrics are
    /// enrolled, in which case the caller keeps biometric unlocking off and
    /// the plain local entry in use.
    pub fn save(&self, secret: &SecretString) -> Result<(), BiometricError> {
        let mut options = PasswordOptions::new_generic_password(SERVICE, self.account);
        options.set_access_control_options(AccessControlOptions::BIOMETRY_CURRENT_SET);
        set_generic_password_options(secret.expose().as_bytes(), options).map_err(|error| {
            BiometricError::Unavailable {
                reason: format!("the keychain refused the protected entry: {error}"),
            }
        })
    }

    /// Reads the secret; `Ok(None)` only when no protected entry exists.
    ///
    /// Reading is what asks the user: the keychain raises the system's
    /// biometric prompt for an item under `biometryCurrentSet`.
    ///
    /// # Errors
    ///
    /// A read that cannot ask the user right now is [`BiometricError::Busy`];
    /// any other failure means the protection this entry was saved under no
    /// longer holds — a changed enrollment above all — and is reported as
    /// [`BiometricError::EnrollmentChanged`] rather than retried quietly.
    pub fn load(&self) -> Result<Option<SecretString>, BiometricError> {
        match get_generic_password(SERVICE, self.account) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(secret) => Ok(Some(SecretString::new(secret))),
                Err(_) => Err(BiometricError::EnrollmentChanged {
                    reason: "the protected keychain item no longer decodes".to_owned(),
                }),
            },
            Err(error) => match error.code() {
                ERR_SEC_ITEM_NOT_FOUND => Ok(None),
                ERR_SEC_INTERACTION_NOT_ALLOWED => Err(BiometricError::Busy {
                    reason: format!("the keychain cannot ask the user right now: {error}"),
                }),
                _ => Err(BiometricError::EnrollmentChanged {
                    reason: format!("the protected keychain item refused the read: {error}"),
                }),
            },
        }
    }

    /// Forgets the entry. Forgetting one that was never saved succeeds.
    ///
    /// # Errors
    ///
    /// The keychain refused the deletion.
    pub fn delete(&self) -> Result<(), BiometricError> {
        match delete_generic_password(SERVICE, self.account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(error) => Err(BiometricError::Unavailable {
                reason: format!("the keychain refused to delete the protected entry: {error}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capability_query_answers_without_prompting() {
        // canEvaluatePolicy:error: is one of the calls LocalAuthentication
        // answers from state alone; on a machine without Touch ID it simply
        // says no. This test deliberately asks nothing else: a real
        // evaluation would put a system prompt in front of whoever runs the
        // tests.
        let capability = SystemAuthenticator.capability();
        assert!(
            !capability.is_biometric() || capability.can_verify_user(),
            "a machine that can verify with biometrics can verify at all"
        );
    }

    #[test]
    fn the_messages_this_module_sends_match_the_declared_methods() {
        use block2::Block;
        use objc2::sel;

        // Checks the two messages the code sends against the signatures the
        // system declares, without sending either: no prompt, no context.
        let class = LAContext::class();
        class
            .verify_sel::<(isize, *mut *mut NSError), objc2::runtime::Bool>(sel!(
                canEvaluatePolicy:error:
            ))
            .expect(
                "canEvaluatePolicy:error: takes a policy and an error out-parameter and answers \
                 with a boolean",
            );
        class
            .verify_sel::<(isize, &NSString, &Block<dyn Fn(ObjCBool, *mut NSError)>), ()>(
                sel!(evaluatePolicy:localizedReason:reply:),
            )
            .expect(
                "evaluatePolicy:localizedReason:reply: takes a policy, a reason and a reply block",
            );
    }
}
