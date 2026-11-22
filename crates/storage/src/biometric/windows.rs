//! Windows: Windows Hello user consent is the verification gate.
//!
//! The desktop interop (`IUserConsentVerifierInterop`) parents the prompt to
//! the window the user is in; when the calling thread has no window of its
//! own — a background task, a test — the plain WinRT call is the fallback.
//! This is the system verifying the window's user: Windows does not tell the
//! application whether Hello answered with a face, a fingerprint or a PIN, so
//! the capability is reported as user verification rather than biometrics,
//! and nothing here is advertised as cryptographically unextractable. Local
//! secrets stay where they already are, in the Credential Manager.

use std::sync::OnceLock;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::System::Com::CoIncrementMTAUsage;
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::core::{HSTRING, factory};
use windows_future::IAsyncOperation;

use super::{BiometricAuthenticator, BiometricCapability, BiometricError, VerificationPrompt};

/// The system's own user verification, by way of Windows Hello consent.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAuthenticator;

impl SystemAuthenticator {
    fn availability(&self) -> windows::core::Result<UserConsentVerifierAvailability> {
        // CheckAvailabilityAsync asks the device, not the user: no prompt.
        let operation = UserConsentVerifier::CheckAvailabilityAsync()?;
        wait(operation)
    }
}

impl BiometricAuthenticator for SystemAuthenticator {
    fn capability(&self) -> BiometricCapability {
        match self.availability() {
            Ok(UserConsentVerifierAvailability::Available) => {
                // Windows does not say whether Hello will answer with a
                // sensor or a PIN, so this is user verification, not a
                // biometric claim.
                BiometricCapability::SystemUserVerification
            }
            _ => BiometricCapability::Unavailable,
        }
    }

    fn verify(&self, prompt: &VerificationPrompt) -> Result<(), BiometricError> {
        ensure_com_apartment();
        let message = HSTRING::from(prompt.reason());
        let result = consent(&message).map_err(|error| BiometricError::Unavailable {
            reason: error.to_string(),
        })?;
        match result {
            UserConsentVerificationResult::Verified => Ok(()),
            UserConsentVerificationResult::Canceled => Err(BiometricError::Cancelled),
            UserConsentVerificationResult::RetriesExhausted => Err(BiometricError::Failed),
            UserConsentVerificationResult::DeviceBusy => Err(BiometricError::Busy {
                reason: "Windows Hello is busy".to_owned(),
            }),
            UserConsentVerificationResult::NotConfiguredForUser => {
                Err(BiometricError::NotEnrolled {
                    reason: "Windows Hello is not configured for this user".to_owned(),
                })
            }
            result => Err(BiometricError::Unavailable {
                reason: format!("Windows Hello refused the verification: {}", result.0),
            }),
        }
    }
}

/// Raises the consent prompt and waits for the user's answer.
///
/// Each call starts its own operation; nothing about one answer can satisfy
/// the next call.
fn consent(message: &HSTRING) -> windows::core::Result<UserConsentVerificationResult> {
    // Desktop apps parent the prompt to a window through the interop
    // interface; a thread without a window of its own (the null handle here)
    // takes the plain WinRT call instead.
    let window = unsafe { GetActiveWindow() };
    let operation: IAsyncOperation<UserConsentVerificationResult> = if window.is_invalid() {
        UserConsentVerifier::RequestVerificationAsync(message)?
    } else {
        let interop: IUserConsentVerifierInterop =
            factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;
        unsafe { interop.RequestVerificationForWindowAsync(window, message)? }
    };
    wait(operation)
}

/// Waits for a WinRT operation on the calling thread.
///
/// The no-op waker is deliberate: pacing the poll loop is what keeps this
/// from spinning while the user looks at the prompt, which is a
/// human-timescale wait rather than a machine one.
fn wait<T: windows::core::RuntimeType>(operation: IAsyncOperation<T>) -> windows::core::Result<T> {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(operation.into_future());
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
            return result;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Keeps an implicit MTA alive for the process.
///
/// WinRT activation needs an apartment. The window owns its threads, but
/// verification runs on whichever thread asks, including ones that never
/// initialized COM. The cookie is never released on purpose: the implicit MTA
/// those calls use has to outlive every later verification.
fn ensure_com_apartment() {
    static APARTMENT: OnceLock<()> = OnceLock::new();
    APARTMENT.get_or_init(|| {
        // A refusal is not fatal: the verification itself then reports
        // whatever the system says about a thread without an apartment.
        let _ = unsafe { CoIncrementMTAUsage() };
    });
}
