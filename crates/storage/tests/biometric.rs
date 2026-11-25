//! Device-local verification and lock policy (R3, R4), run against the
//! scripted double and the lock policy file — never against the real system
//! verifier, so no test puts a Touch ID or Windows Hello prompt in front of
//! whoever runs it. The real adapters are exercised on hardware in the
//! delivery matrix, not here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use shellune_storage::{
    BiometricAuthenticator, BiometricCapability, BiometricError, DEFAULT_IDLE_LOCK_MINUTES,
    LocalOnlyFallback, LockPolicy, Platform, SECURITY_FILE_NAME, ScriptedAuthenticator,
    SecuritySettings, SecurityStore, VerificationPrompt,
};

fn scratch(name: &str) -> PathBuf {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shellune-biometric-{name}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn prompt(reason: &str) -> VerificationPrompt {
    VerificationPrompt::new(reason)
}

#[test]
fn each_verification_takes_the_next_scripted_step_and_none_is_replayed() {
    let system = ScriptedAuthenticator::new(BiometricCapability::Biometrics).script([
        Ok(()),
        Err(BiometricError::Cancelled),
        Err(BiometricError::Failed),
        Err(BiometricError::Busy {
            reason: "locked out for a moment".to_owned(),
        }),
    ]);

    // Every call is a fresh verification: the first pass is used up by the
    // first operation and the second operation has to survive its own answer.
    assert_eq!(system.verify(&prompt("read the vault")), Ok(()));
    assert_eq!(
        system.verify(&prompt("read the vault")),
        Err(BiometricError::Cancelled)
    );
    assert_eq!(
        system.verify(&prompt("read the vault")),
        Err(BiometricError::Failed)
    );
    assert_eq!(
        system.verify(&prompt("read the vault")),
        Err(BiometricError::Busy {
            reason: "locked out for a moment".to_owned()
        })
    );
    assert_eq!(system.verify_calls(), 4);
    assert_eq!(system.remaining_steps(), 0);
}

#[test]
fn a_first_success_is_not_the_next_operation_pass() {
    // The R3 acceptance shape: two managed-key authentications in a row, the
    // user verifying once and then cancelling. The second authentication must
    // not start on the strength of the first verification.
    let system = ScriptedAuthenticator::new(BiometricCapability::Biometrics)
        .script([Ok(()), Err(BiometricError::Cancelled)]);

    assert_eq!(system.verify(&prompt("sign with the managed key")), Ok(()));
    assert_eq!(
        system.verify(&prompt("sign with the managed key")),
        Err(BiometricError::Cancelled)
    );
}

#[test]
fn an_exhausted_script_refuses_instead_of_inventing_a_pass() {
    let system = ScriptedAuthenticator::new(BiometricCapability::SystemUserVerification);
    assert_eq!(system.remaining_steps(), 0);
    let result = system.verify(&prompt("export a private key"));
    assert!(
        matches!(result, Err(BiometricError::Unavailable { .. })),
        "a missing answer is a refusal, never a quiet success: {result:?}"
    );
}

#[test]
fn the_capability_query_is_silent_and_never_consumes_a_verification() {
    let system = ScriptedAuthenticator::new(BiometricCapability::Biometrics)
        .script([Err(BiometricError::Cancelled)]);

    // What a lock screen clearing itself, or a background sync deciding to
    // defer, may ask: the capability, as often as it needs to, without
    // spending the user's next prompt or raising anything.
    for _ in 0..3 {
        assert!(system.capability().is_biometric());
    }
    assert_eq!(system.capability_queries(), 3);
    assert_eq!(system.verify_calls(), 0);

    // The scripted verification is still the user's first prompt.
    assert_eq!(
        system.verify(&prompt("copy a password")),
        Err(BiometricError::Cancelled)
    );
}

#[test]
fn a_machine_without_any_capability_still_answers_the_silent_query() {
    let system = ScriptedAuthenticator::new(BiometricCapability::Unavailable);
    assert!(!system.capability().can_verify_user());
    assert!(matches!(
        system.verify(&prompt("copy a password")),
        Err(BiometricError::Unavailable { .. })
    ));
}

#[test]
fn a_local_only_machine_without_biometrics_falls_back_by_capability() {
    // With no biometrics, the high-risk gate is whatever the system can ask:
    // its own user verification when there is one, otherwise the already
    // unlocked session behind an explicit risk confirmation — never a
    // stronger claim than the machine can honour.
    assert_eq!(
        LocalOnlyFallback::for_capability(BiometricCapability::Unavailable),
        LocalOnlyFallback::UnlockedSessionWithConfirmation
    );
    assert_eq!(
        LocalOnlyFallback::for_capability(BiometricCapability::SystemUserVerification),
        LocalOnlyFallback::SystemUserVerification
    );
    assert_eq!(
        LocalOnlyFallback::for_capability(BiometricCapability::Biometrics),
        LocalOnlyFallback::SystemUserVerification
    );
}

#[test]
fn the_error_model_keeps_the_outcomes_a_caller_must_treat_differently() {
    // Each judgment matches exactly one outcome: none of them is a pass, and
    // they do not collapse into each other or into a single boolean.
    assert!(BiometricError::Cancelled.is_cancelled());
    assert!(!BiometricError::Failed.is_cancelled());
    assert!(
        BiometricError::EnrollmentChanged {
            reason: "a new finger".to_owned()
        }
        .is_enrollment_changed()
    );
    assert!(
        !BiometricError::Busy {
            reason: "busy".to_owned()
        }
        .is_enrollment_changed()
    );
    for outcome in [
        BiometricError::Unavailable {
            reason: "no hardware".to_owned(),
        },
        BiometricError::NotEnrolled {
            reason: "nothing enrolled".to_owned(),
        },
        BiometricError::Cancelled,
        BiometricError::Failed,
        BiometricError::Busy {
            reason: "busy".to_owned(),
        },
        BiometricError::EnrollmentChanged {
            reason: "a new finger".to_owned(),
        },
    ] {
        assert_eq!(outcome.is_cancelled(), outcome == BiometricError::Cancelled);
        assert!(
            outcome.is_enrollment_changed()
                == matches!(outcome, BiometricError::EnrollmentChanged { .. })
        );
    }
}

#[test]
fn the_three_lock_policies_and_the_idle_setting_stay_on_this_machine() {
    let dir = scratch("device-local");
    let store = SecurityStore::at(&dir);
    assert_eq!(store.load().policy, LockPolicy::UnlockVault);

    for policy in [
        LockPolicy::Disabled,
        LockPolicy::UnlockVault,
        LockPolicy::EverySensitiveOperation,
    ] {
        let settings = SecuritySettings {
            policy,
            idle_lock_minutes: Some(7),
            biometric_unlock: Some(true),
        };
        store.save(&settings).expect("saves");
        assert_eq!(store.load(), settings);
    }

    // The file lives beside the other host data, under its own name.
    assert!(store.path().ends_with(SECURITY_FILE_NAME));

    // None of it is part of what a backup, a sync or a portable export
    // carries: another machine's policy stays its own.
    let exported = shellune_storage::ShelluneData::export_from_vault(
        &shellune_storage::LocalState::default(),
        &shellune_storage::ExportOptions::new(Platform::Linux),
        None,
        None,
    )
    .expect("exports");
    let portable = serde_json::to_string(&exported.document).expect("encodes");
    assert!(!portable.contains("policy"), "{portable}");
    assert!(!portable.contains("idle_lock"), "{portable}");
    assert!(!portable.contains("biometric_unlock"), "{portable}");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_idle_setting_the_build_cannot_honour_is_reported_not_corrected() {
    let dir = scratch("unusable");
    std::fs::create_dir_all(&dir).expect("dir");
    let store = SecurityStore::at(&dir);

    // A usable value round-trips and is honoured as saved.
    store
        .save(&SecuritySettings {
            idle_lock_minutes: Some(2),
            ..SecuritySettings::default()
        })
        .expect("saves");
    let (settings, problem) = store.load_reported();
    assert_eq!(problem, None);
    assert_eq!(settings.idle_lock_minutes, Some(2));

    // A value this build cannot honour is not clamped into a quiet default.
    for text in [
        br#"{"idle_lock_minutes":0}"#.as_slice(),
        br#"{"idle_lock_minutes":99999}"#,
    ] {
        std::fs::write(store.path(), text).expect("write");
        let (settings, problem) = store.load_reported();
        assert_eq!(
            problem,
            Some(shellune_storage::SecurityProblem::Unreadable),
            "{}",
            String::from_utf8_lossy(text)
        );
        assert_eq!(settings.idle_lock_minutes, None);
        assert_eq!(
            settings.idle_lock(),
            std::time::Duration::from_secs(u64::from(DEFAULT_IDLE_LOCK_MINUTES) * 60)
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
