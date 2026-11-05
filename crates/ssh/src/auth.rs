//! Turning the credentials of one attempt into something the transport can use (R4).
//!
//! This is the only place that opens a key file, and the passphrase is borrowed for
//! the single decrypt call. Nothing here writes a secret anywhere: the errors name the
//! path and the problem, never the value.

use std::path::Path;

use russh::keys::PrivateKey;
use shellune_core::SecretString;
use zeroize::Zeroizing;

use crate::error::ConnectError;

/// Reads a private key file, decrypting it when a passphrase was given.
///
/// # Errors
///
/// Returns [ConnectError::KeyUnreadable] for a blank path or a file this account
/// cannot read, [ConnectError::KeyEncrypted] when the file needs a passphrase that
/// was not given, [ConnectError::KeyDecryptFailed] when the passphrase is wrong and
/// [ConnectError::KeyInvalid] for content that is not an OpenSSH private key.
pub fn load_private_key(
    path: &str,
    passphrase: Option<&SecretString>,
) -> Result<PrivateKey, ConnectError> {
    let chosen = path.trim();
    if chosen.is_empty() {
        return Err(ConnectError::KeyUnreadable {
            path: String::new(),
            reason: "no key file was chosen".to_owned(),
        });
    }
    let key_path = Path::new(chosen);

    // The file holds the private key itself, so the text is zeroed when it is
    // dropped instead of being left in an ordinary String.
    let pem = Zeroizing::new(std::fs::read_to_string(key_path).map_err(|error| {
        ConnectError::KeyUnreadable {
            path: chosen.to_owned(),
            reason: error.to_string(),
        }
    })?);
    let key =
        PrivateKey::from_openssh(pem.as_bytes()).map_err(|error| ConnectError::KeyInvalid {
            path: chosen.to_owned(),
            reason: error.to_string(),
        })?;

    if !key.is_encrypted() {
        // An unencrypted key is usable whatever was typed for the passphrase: asking
        // for one it does not need is not a failure.
        return Ok(key);
    }
    let Some(secret) = passphrase else {
        return Err(ConnectError::KeyEncrypted {
            path: chosen.to_owned(),
        });
    };
    key.decrypt(secret.expose())
        .map_err(|_| ConnectError::KeyDecryptFailed {
            path: chosen.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch_file(name: &str, contents: &str) -> std::path::PathBuf {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let unique = format!(
            "shellune-key-{}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            name
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, contents).expect("the fixture is written");
        path
    }

    #[test]
    fn no_path_is_reported_before_anything_is_read() {
        let error = load_private_key("   ", None).expect_err("a blank path is not a key");
        assert!(matches!(error, ConnectError::KeyUnreadable { .. }));
        assert!(error.next_step().contains("key file"), "{error}");
    }

    #[test]
    fn a_missing_file_is_not_reported_as_a_broken_key() {
        let error = load_private_key("/no/such/shellune/key", None)
            .expect_err("a missing file cannot be read");
        assert!(matches!(error, ConnectError::KeyUnreadable { .. }));
        assert!(
            !error.to_string().contains("usable"),
            "the file was never read, so its content is not the problem: {error}"
        );
    }

    #[test]
    fn content_that_is_not_a_key_names_the_parser_reason() {
        let path = scratch_file("garbage", "this is not a private key\n");
        let error = load_private_key(path.to_str().expect("a path"), None)
            .expect_err("garbage is not a key");
        assert!(matches!(error, ConnectError::KeyInvalid { .. }));
        assert!(error.to_string().contains("garbage"), "{error}");
        std::fs::remove_file(path).ok();
    }
}
