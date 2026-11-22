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
//! changes, and a read that can no longer decode under that protection is
//! reported as [`BiometricError::EnrollmentChanged`] so the caller disables
//! biometric unlocking on this machine and asks for the current passphrase.
//! A read the user cancelled, or an answer that did not match, keeps its own
//! error — cancelling the Touch ID prompt must never read as a reset.

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

// Keychain status codes (Security/SecBase.h), each with the wording the
// header documents for it. The mapping below hangs on those wordings, not on
// guesswork about what a status "probably" means.
const ERR_SEC_USER_CANCELED: i32 = -128; // "User canceled the operation."
const ERR_SEC_NOT_AVAILABLE: i32 = -25291; // "No keychain is available."
const ERR_SEC_AUTH_FAILED: i32 = -25293; // "The user name or passphrase you entered is not correct."
const ERR_SEC_NO_SUCH_KEYCHAIN: i32 = -25294; // "The specified keychain could not be found."
const ERR_SEC_INVALID_KEYCHAIN: i32 = -25295; // "The specified keychain is not a valid keychain file."
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300; // "The specified item could not be found in the keychain."
const ERR_SEC_NO_DEFAULT_KEYCHAIN: i32 = -25307; // "A default keychain could not be found."
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308; // "User interaction is not allowed."
const ERR_SEC_INTERACTION_REQUIRED: i32 = -25315; // "User interaction is required, but is currently not allowed."
const ERR_SEC_IN_DARK_WAKE: i32 = -25320; // "In dark wake, no UI possible"
const ERR_SEC_DECODE: i32 = -26275; // "Unable to decode the provided data."
const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018; // "A required entitlement is not present."

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

/// What a failed read of the biometry-protected entry means, decided from the
/// keychain status alone so it can be tested without a keychain.
///
/// The mapping is the contract: a user who cancels the Touch ID prompt the
/// read raised was not verified, but nothing about the machine's protection
/// changed, so that answer must stay a [`ProtectedReadFailure::Cancelled`]
/// ("ask again later") and never an enrollment change ("disable the
/// protection and re-enable with the passphrase"). See [`map_read_status`]
/// for where each status comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProtectedReadFailure {
    /// No protected entry exists.
    Missing,
    /// The user backed out of the prompt the read raised. Not a failure, and
    /// never a pass.
    Cancelled,
    /// The system cannot ask the user right now: no interaction allowed,
    /// interaction required but not allowed, or dark wake.
    Busy { reason: String },
    /// The system asked and the answer did not match.
    Failed,
    /// The entry's content can no longer be decoded under the protection it
    /// was saved with — the signature of a `biometryCurrentSet` item whose
    /// enrolled set changed. This machine's biometric protection was reset.
    EnrollmentChanged { reason: String },
    /// A status the table cannot pin down, and a keychain that is missing or
    /// misconfigured. The caller asks again or falls back to the current
    /// passphrase. Never a pass, and never on its own a reason to disable
    /// the machine's biometric protection.
    Unavailable { reason: String },
}

impl From<ProtectedReadFailure> for BiometricError {
    fn from(failure: ProtectedReadFailure) -> Self {
        match failure {
            ProtectedReadFailure::Missing => Self::Unavailable {
                reason: "no protected keychain entry exists".to_owned(),
            },
            ProtectedReadFailure::Cancelled => Self::Cancelled,
            ProtectedReadFailure::Busy { reason } => Self::Busy { reason },
            ProtectedReadFailure::Failed => Self::Failed,
            ProtectedReadFailure::EnrollmentChanged { reason } => {
                Self::EnrollmentChanged { reason }
            }
            ProtectedReadFailure::Unavailable { reason } => Self::Unavailable { reason },
        }
    }
}

/// Decides what a failed read of the protected entry means, from the status
/// the keychain returned and the documented meaning of each code
/// (Security framework, `SecBase.h`, quoted beside the constants above).
///
/// Only `errSecDecode` — the item's content cannot be decoded under the
/// protection it was saved with — is treated as the protection being reset:
/// that is the documented signature of what a `biometryCurrentSet`
/// invalidation leaves behind. Every status the table cannot pin down is
/// deliberately the conservative [`ProtectedReadFailure::Unavailable`]:
/// the asymmetry is on purpose, because asking again or falling back to the
/// passphrase costs the user a moment, while mistaking a cancellation for an
/// invalidation would silently switch the machine to a weaker gate — the one
/// outcome R3 and R4 forbid. A user cancellation (-128) is reported as such;
/// an authentication mismatch (-25293) is a failed verification, not a reset.
#[must_use]
fn map_read_status(status: i32) -> ProtectedReadFailure {
    match status {
        ERR_SEC_ITEM_NOT_FOUND => ProtectedReadFailure::Missing,
        ERR_SEC_USER_CANCELED => ProtectedReadFailure::Cancelled,
        ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_INTERACTION_REQUIRED | ERR_SEC_IN_DARK_WAKE => {
            ProtectedReadFailure::Busy {
                reason: format!("the keychain cannot ask the user right now (status {status})"),
            }
        }
        ERR_SEC_AUTH_FAILED => ProtectedReadFailure::Failed,
        ERR_SEC_DECODE => ProtectedReadFailure::EnrollmentChanged {
            reason: "the protected keychain item no longer decodes".to_owned(),
        },
        ERR_SEC_NOT_AVAILABLE
        | ERR_SEC_NO_SUCH_KEYCHAIN
        | ERR_SEC_INVALID_KEYCHAIN
        | ERR_SEC_NO_DEFAULT_KEYCHAIN
        | ERR_SEC_MISSING_ENTITLEMENT => ProtectedReadFailure::Unavailable {
            reason: format!("the keychain is unavailable or misconfigured (status {status})"),
        },
        other => ProtectedReadFailure::Unavailable {
            reason: format!("the protected keychain entry refused the read (status {other})"),
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
/// read fails as [`BiometricError::EnrollmentChanged`] — while a cancelled
/// prompt stays [`BiometricError::Cancelled`], per [`map_read_status`].
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
    /// biometric prompt for an item under `biometryCurrentSet`. Nothing is
    /// ever released without that prompt succeeding.
    ///
    /// # Errors
    ///
    /// [`map_read_status`] decides, from the status the keychain returned: a
    /// cancelled prompt is [`BiometricError::Cancelled`] and a mismatch is
    /// [`BiometricError::Failed`] — neither disables the machine's biometric
    /// protection. Only a read whose content no longer decodes under the
    /// protection it was saved with is [`BiometricError::EnrollmentChanged`].
    /// A system that cannot ask right now answers [`BiometricError::Busy`];
    /// anything the table cannot pin down is the conservative
    /// [`BiometricError::Unavailable`], never a pass.
    pub fn load(&self) -> Result<Option<SecretString>, BiometricError> {
        match get_generic_password(SERVICE, self.account) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(secret) => Ok(Some(SecretString::new(secret))),
                // The entry answered with bytes this build never writes: the
                // protection served something other than what was saved.
                Err(_) => Err(BiometricError::EnrollmentChanged {
                    reason: "the protected keychain item no longer decodes".to_owned(),
                }),
            },
            Err(error) => match map_read_status(error.code()) {
                ProtectedReadFailure::Missing => Ok(None),
                failure => Err(failure.into()),
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

    #[test]
    fn a_cancelled_keychain_read_is_not_an_enrollment_change() {
        // -128 is errSecUserCanceled, "User canceled the operation": the
        // regular result of a user backing out of the Touch ID prompt a read
        // of the biometryCurrentSet entry raises. Treating it as an
        // enrollment change would permanently disable this machine's
        // biometric protection on one ordinary cancellation — the opposite
        // of "cancelled is not a pass, and not a reset either".
        assert_eq!(map_read_status(-128), ProtectedReadFailure::Cancelled);
        assert!(BiometricError::from(map_read_status(-128)).is_cancelled());
        assert!(
            !BiometricError::from(map_read_status(-128)).is_enrollment_changed(),
            "a cancellation must never disable the machine's biometric protection"
        );
    }

    #[test]
    fn an_authentication_mismatch_is_a_failed_read_not_a_reset() {
        // -25293 is errSecAuthFailed, "The user name or passphrase you
        // entered is not correct": the system asked and the answer did not
        // match. The entry still works; the user can be asked again.
        assert_eq!(map_read_status(-25293), ProtectedReadFailure::Failed);
        assert_eq!(
            BiometricError::from(map_read_status(-25293)),
            BiometricError::Failed
        );
    }

    #[test]
    fn a_missing_entry_is_absent_and_a_stalled_system_is_busy() {
        // -25300 is errSecItemNotFound: the one status that means "nothing
        // was saved", which load answers as Ok(None).
        assert_eq!(map_read_status(-25300), ProtectedReadFailure::Missing);

        // The three documented "cannot ask the user right now" statuses:
        // -25308 errSecInteractionNotAllowed, -25315
        // errSecInteractionRequired, -25320 errSecInDarkWake.
        for status in [-25308, -25315, -25320] {
            assert!(
                matches!(map_read_status(status), ProtectedReadFailure::Busy { .. }),
                "status {status} means the system cannot ask right now"
            );
        }
    }

    #[test]
    fn an_undecodable_entry_is_the_only_reset_signature() {
        // -26275 is errSecDecode, "Unable to decode the provided data": the
        // content can no longer be decoded under the protection it was saved
        // with, which is what a biometryCurrentSet invalidation leaves.
        assert!(
            map_read_status(-26275)
                == ProtectedReadFailure::EnrollmentChanged {
                    reason: "the protected keychain item no longer decodes".to_owned()
                }
        );
        assert!(
            BiometricError::from(map_read_status(-26275)).is_enrollment_changed(),
            "an entry that no longer decodes is the machine's protection being reset"
        );
    }

    #[test]
    fn a_status_the_table_cannot_pin_down_is_conservatively_not_a_reset() {
        // Statuses outside the table (-25316 errSecDataNotAvailable, and a
        // code nothing maps to) are "ask again or use the passphrase": never
        // a pass, and never a reason to disable the protection on their own.
        // The same holds for the documented broken-keychain statuses
        // (-25291, -25294, -25295, -25307, -34018).
        for status in [-25316, -1, -25291, -25294, -25295, -25307, -34018] {
            let error = BiometricError::from(map_read_status(status));
            assert!(
                !error.is_enrollment_changed(),
                "status {status} must not read as a reset"
            );
            assert!(
                !error.is_cancelled(),
                "status {status} must not read as a pass"
            );
            assert!(
                matches!(error, BiometricError::Unavailable { .. }),
                "status {status} is the conservative ask-again answer: {error:?}"
            );
        }
    }

    #[test]
    fn the_local_authentication_error_mapping_keeps_its_outcomes_apart() {
        // The LAError side of the model is unchanged: cancellation, mismatch,
        // nothing enrolled, locked out, and no hardware stay distinct.
        assert_eq!(map_error(-2, LA_ERROR_DOMAIN), BiometricError::Cancelled);
        assert_eq!(map_error(-9, LA_ERROR_DOMAIN), BiometricError::Cancelled);
        assert_eq!(map_error(-3, LA_ERROR_DOMAIN), BiometricError::Cancelled);
        assert_eq!(map_error(-1, LA_ERROR_DOMAIN), BiometricError::Failed);
        assert!(matches!(
            map_error(-8, LA_ERROR_DOMAIN),
            BiometricError::Busy { .. }
        ));
        assert!(matches!(
            map_error(-7, LA_ERROR_DOMAIN),
            BiometricError::NotEnrolled { .. }
        ));
        assert!(matches!(
            map_error(-6, LA_ERROR_DOMAIN),
            BiometricError::Unavailable { .. }
        ));
        // A foreign domain is never interpreted as a LocalAuthentication code.
        assert!(matches!(
            map_error(-2, "com.example.Other"),
            BiometricError::Unavailable { .. }
        ));
    }
}
