//! OpenSSH-style SHA-256 host key fingerprint formatting (R6).
//!
//! The SSH layer supplies the raw public key bytes once a host answers; this module
//! turns them into the `SHA256:<base64-without-padding>` form users can compare
//! with `ssh-keygen -lf`. It only formats public material, so nothing here can leak
//! a credential.

/// Renders a SHA-256 host key fingerprint as `SHA256:<base64>`.
///
/// Returns `None` when the input is not a SHA-256 digest, so a caller can never
/// display a malformed or truncated fingerprint as if it were verifiable.
#[must_use]
pub fn sha256_fingerprint(digest: &[u8]) -> Option<String> {
    const SHA256_LEN: usize = 32;
    if digest.len() != SHA256_LEN {
        return None;
    }
    Some(format!("SHA256:{}", base64_unpadded(digest)))
}

fn base64_unpadded(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut encoded = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map(u32::from);
        let third = chunk.get(2).copied().map(u32::from);

        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded
            .push(ALPHABET[(((first & 0b11) << 4) | (second.unwrap_or(0) >> 4)) as usize] as char);

        if let Some(second) = second {
            encoded.push(
                ALPHABET[(((second & 0b1111) << 2) | (third.unwrap_or(0) >> 6)) as usize] as char,
            );
        }
        if let Some(third) = third {
            encoded.push(ALPHABET[(third & 0b11_1111) as usize] as char);
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_use_the_openssh_sha256_prefix_without_padding() {
        let digest = [0u8; 32];
        let fingerprint = sha256_fingerprint(&digest).expect("32 byte digest is a sha256");

        assert_eq!(fingerprint, format!("SHA256:{}", "A".repeat(43)));
        assert!(!fingerprint.contains('='));
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_unpadded(b""), "");
        assert_eq!(base64_unpadded(b"f"), "Zg");
        assert_eq!(base64_unpadded(b"fo"), "Zm8");
        assert_eq!(base64_unpadded(b"foo"), "Zm9v");
        assert_eq!(base64_unpadded(b"foob"), "Zm9vYg");
        assert_eq!(base64_unpadded(b"fooba"), "Zm9vYmE");
        assert_eq!(base64_unpadded(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn digests_of_the_wrong_length_are_rejected() {
        assert!(sha256_fingerprint(&[]).is_none());
        assert!(sha256_fingerprint(&[1u8; 31]).is_none());
        assert!(sha256_fingerprint(&[1u8; 33]).is_none());
    }
}
