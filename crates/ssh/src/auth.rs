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
    // `~` is the home folder in every SSH configuration, and a host imported from one
    // keeps the path as it was written; the file system does not expand it, so a path
    // like `~/.ssh/id_ed25519` would otherwise be looked up literally and not found.
    let expanded = expand_home(chosen);
    let key_path = expanded.as_path();

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

/// Replaces a leading `~` (alone, or before `/` or `\\`) with the user's home folder.
///
/// Anything else is returned unchanged, including `~user/...`, which names another
/// account's home and is not this program's to resolve, and a path when no home folder
/// is known.
#[must_use]
pub fn expand_home(path: &str) -> std::path::PathBuf {
    expand_home_with(path, home_dir().as_deref())
}

fn home_dir() -> Option<std::path::PathBuf> {
    let windows_home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        None
    };
    windows_home
        .or_else(|| std::env::var_os("HOME"))
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
}

fn expand_home_with(path: &str, home: Option<&Path>) -> std::path::PathBuf {
    let Some(home) = home else {
        return std::path::PathBuf::from(path);
    };
    if path == "~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None => std::path::PathBuf::from(path),
    }
}

#[cfg(test)]
mod home_tests {
    use super::*;

    #[test]
    fn a_leading_tilde_is_the_home_folder() {
        let home = Path::new("/home/ada");
        assert_eq!(
            expand_home_with("~/.ssh/id_ed25519", Some(home)),
            home.join(".ssh/id_ed25519")
        );
        assert_eq!(
            expand_home_with("~\\.ssh\\id_rsa", Some(home)),
            home.join(".ssh\\id_rsa")
        );
        assert_eq!(expand_home_with("~", Some(home)), home.to_path_buf());
    }

    #[test]
    fn other_paths_are_left_alone() {
        let home = Path::new("/home/ada");
        for path in ["/etc/key", "keys/~/x", "~bob/.ssh/id", "C:\\keys\\id"] {
            assert_eq!(expand_home_with(path, Some(home)), Path::new(path));
        }
        assert_eq!(
            expand_home_with("~/.ssh/id", None),
            Path::new("~/.ssh/id"),
            "without a known home the path is not guessed"
        );
    }

    #[test]
    fn a_tilde_key_path_is_read_from_the_home_folder() {
        // The error names the path as the user wrote it, but the read went to the
        // expanded one: a missing file under a real home says so with the OS reason.
        let error = load_private_key("~/shellune-no-such-key-7c1e", None)
            .expect_err("the key does not exist");
        match error {
            ConnectError::KeyUnreadable { path, .. } => {
                assert_eq!(path, "~/shellune-no-such-key-7c1e");
            }
            other => panic!("expected KeyUnreadable, got {other:?}"),
        }
    }
}
