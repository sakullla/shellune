//! `shellune-sign` as the release pipeline runs it: its signatures verify with
//! the updater's own check, and it refuses to run without its secrets.
#![cfg(all(feature = "release-tools", feature = "test-support"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use semver::Version;
use shellune_update::release::{KEY_VARIABLE, PASSWORD_VARIABLE, generate_keys};
use shellune_update::testing::TempFolder;
use shellune_update::{UpdateError, verify_file};

fn tool(folder: &Path, arguments: &[&str], secrets: Option<(&str, &str)>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shellune-sign"));
    command
        .args(arguments)
        .current_dir(folder)
        .env_remove(KEY_VARIABLE)
        .env_remove(PASSWORD_VARIABLE);
    if let Some((key, password)) = secrets {
        command
            .env(KEY_VARIABLE, key)
            .env(PASSWORD_VARIABLE, password);
    }
    command.output().expect("shellune-sign runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_tool_signs_for_a_version_the_updater_accepts() {
    let folder = TempFolder::new("sign-cli");
    let keys = generate_keys("release password").unwrap();
    let package = folder
        .path()
        .join("Shellune-1.0.0-windows-x86_64-setup.exe");
    fs::write(&package, b"installer bytes".repeat(4096)).unwrap();
    let package_arg = package.to_str().unwrap();

    let output = tool(
        folder.path(),
        &["sign", package_arg, "--version", "1.0.0"],
        Some((&keys.secret_key_base64, "release password")),
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let signature_path = PathBuf::from(format!("{package_arg}.sig"));
    assert_eq!(
        text(&output.stdout).trim(),
        signature_path.display().to_string()
    );
    let signature = fs::read_to_string(&signature_path).unwrap();

    let version = Version::new(1, 0, 0);
    verify_file(&keys.public_key, &signature, &package, &version).unwrap();
    assert!(matches!(
        verify_file(
            &keys.public_key,
            &signature,
            &package,
            &Version::new(1, 0, 1)
        ),
        Err(UpdateError::VersionMismatch { .. })
    ));
    fs::write(&package, b"changed").unwrap();
    assert!(matches!(
        verify_file(&keys.public_key, &signature, &package, &version),
        Err(UpdateError::SignatureInvalid { .. })
    ));

    // An explicit output path, and the public key of the secret in use.
    let output = tool(
        folder.path(),
        &[
            "sign",
            package_arg,
            "--version",
            "v1.0.1",
            "--output",
            "custom.sig",
        ],
        Some((&keys.secret_key_base64, "release password")),
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let signature = fs::read_to_string(folder.path().join("custom.sig")).unwrap();
    verify_file(
        &keys.public_key,
        &signature,
        &package,
        &Version::new(1, 0, 1),
    )
    .unwrap();
    let output = tool(
        folder.path(),
        &["public-key"],
        Some((&keys.secret_key_base64, "release password")),
    );
    assert_eq!(text(&output.stdout).trim(), keys.public_key);
}

#[test]
fn the_tool_refuses_without_its_secrets_or_with_a_wrong_password() {
    let folder = TempFolder::new("sign-cli-refused");
    let keys = generate_keys("release password").unwrap();
    let package = folder.path().join("package.zip");
    fs::write(&package, b"zip").unwrap();
    let package_arg = package.to_str().unwrap();
    let signature = folder.path().join("package.zip.sig");

    for secrets in [
        None,
        Some((keys.secret_key_base64.as_str(), "")),
        Some(("", "release password")),
    ] {
        let output = tool(
            folder.path(),
            &["sign", package_arg, "--version", "1.0.0"],
            secrets,
        );
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
        assert!(text(&output.stderr).contains("is not set"));
        assert!(!signature.exists());
    }
    let output = tool(
        folder.path(),
        &["sign", package_arg, "--version", "1.0.0"],
        Some((&keys.secret_key_base64, "wrong")),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(!signature.exists());
    let output = tool(
        folder.path(),
        &["sign", package_arg, "--version", "not-a-version"],
        Some((&keys.secret_key_base64, "release password")),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(!signature.exists());
    // Usage mistakes.
    for arguments in [
        &["sign", package_arg][..],
        &["sign", "--version", "1.0.0"],
        &[],
    ] {
        let output = tool(folder.path(), arguments, None);
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn keygen_writes_a_working_pair_and_never_overwrites() {
    let folder = TempFolder::new("keygen");
    let out = folder.path().join("keys");
    let out_arg = out.to_str().unwrap();
    let output = tool(folder.path(), &["keygen", "--out", out_arg], None);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    let public_key = fs::read_to_string(out.join("shellune-updater.pub")).unwrap();
    let key_line = public_key.lines().nth(1).unwrap();
    assert!(stdout.contains(&format!("public key: {key_line}")));
    let password = fs::read_to_string(out.join("shellune-updater.password")).unwrap();
    assert!(!stdout.contains(&password), "the password is never printed");
    let secret = fs::read_to_string(out.join("shellune-updater.key.base64")).unwrap();
    assert!(!stdout.contains(&secret));
    assert!(
        fs::read_to_string(out.join("shellune-updater.key"))
            .unwrap()
            .starts_with("untrusted comment:")
    );

    let package = folder.path().join("file.bin");
    fs::write(&package, b"data").unwrap();
    let output = tool(
        folder.path(),
        &["sign", package.to_str().unwrap(), "--version", "2.0.0"],
        Some((&secret, &password)),
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let signature = fs::read_to_string(folder.path().join("file.bin.sig")).unwrap();
    verify_file(key_line, &signature, &package, &Version::new(2, 0, 0)).unwrap();

    let again = tool(folder.path(), &["keygen", "--out", out_arg], None);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(out.join("shellune-updater.pub")).unwrap(),
        public_key
    );
}
