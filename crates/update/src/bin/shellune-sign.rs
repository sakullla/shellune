//! Signs Shellune release files for the updater, and makes the signing key pair.
//!
//! ```text
//! shellune-sign sign <file> --version <v> [--output <file.sig>]
//! shellune-sign keygen --out <folder>
//! shellune-sign public-key
//! ```
//!
//! `sign` reads the secret key from `SHELLUNE_UPDATER_KEY` (the base64 of the
//! minisign secret key file) and its password from
//! `SHELLUNE_UPDATER_KEY_PASSWORD`, and writes `<file>.sig`: the base64 of a
//! minisign signature whose trusted comment carries `version:<v>`. Without either
//! variable it exits with status 2 and signs nothing.
//!
//! `keygen` makes a new key pair with a random password and writes, into a
//! folder that must not already hold them: `shellune-updater.key` (the encrypted
//! secret key file), `shellune-updater.key.base64` (the value for
//! `SHELLUNE_UPDATER_KEY`), `shellune-updater.password` (the value for
//! `SHELLUNE_UPDATER_KEY_PASSWORD`) and `shellune-updater.pub`. It prints the
//! public key line for `shellune_update::PUBLIC_KEY`, never the secret.
//!
//! `public-key` prints the public key of the secret in the environment.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use shellune_update::release::{KEY_VARIABLE, PASSWORD_VARIABLE, SigningKey, generate_keys};

const USAGE: &str = "usage:
  shellune-sign sign <file> --version <version> [--output <file.sig>]
  shellune-sign keygen --out <folder>
  shellune-sign public-key";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.first().map(String::as_str) {
        Some("sign") => sign(&arguments[1..]),
        Some("keygen") => keygen(&arguments[1..]),
        Some("public-key") if arguments.len() == 1 => public_key(),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => Err(Failure::Usage(String::new())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            if !message.is_empty() {
                eprintln!("shellune-sign: {message}");
            }
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::MissingSecret(name)) => {
            eprintln!("shellune-sign: {name} is not set; nothing was signed");
            ExitCode::from(2)
        }
        Err(Failure::Failed(message)) => {
            eprintln!("shellune-sign: {message}");
            ExitCode::FAILURE
        }
    }
}

enum Failure {
    Usage(String),
    MissingSecret(&'static str),
    Failed(String),
}

fn sign(arguments: &[String]) -> Result<(), Failure> {
    let mut file: Option<PathBuf> = None;
    let mut version: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let mut value = |name: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| Failure::Usage(format!("{name} needs a value")))
        };
        match argument.as_str() {
            "--version" => version = Some(value("--version")?),
            "--output" => output = Some(PathBuf::from(value("--output")?)),
            other if other.starts_with("--") => {
                return Err(Failure::Usage(format!("unknown option {other}")));
            }
            other if file.is_none() => file = Some(PathBuf::from(other)),
            other => return Err(Failure::Usage(format!("unexpected argument {other}"))),
        }
    }
    let file = file.ok_or_else(|| Failure::Usage("no file to sign".to_owned()))?;
    let version = version.ok_or_else(|| Failure::Usage("--version is required".to_owned()))?;
    let key = signing_key()?;

    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let data = File::open(&file)
        .map_err(|error| Failure::Failed(format!("{} cannot be read: {error}", file.display())))?;
    let signature = key
        .sign(data, &name, &version)
        .map_err(|error| Failure::Failed(error.to_string()))?;
    let output = output.unwrap_or_else(|| {
        let mut path = file.clone().into_os_string();
        path.push(".sig");
        PathBuf::from(path)
    });
    fs::write(&output, &signature).map_err(|error| {
        Failure::Failed(format!("{} cannot be written: {error}", output.display()))
    })?;
    println!("{}", output.display());
    Ok(())
}

fn signing_key() -> Result<SigningKey, Failure> {
    let read = |name: &'static str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(Failure::MissingSecret(name))
    };
    let encoded = read(KEY_VARIABLE)?;
    // A secret pasted with a line break after it still opens the key.
    let password = read(PASSWORD_VARIABLE)?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    SigningKey::open(&encoded, &password).map_err(|error| Failure::Failed(error.to_string()))
}

fn public_key() -> Result<(), Failure> {
    println!("{}", signing_key()?.public_key());
    Ok(())
}

fn keygen(arguments: &[String]) -> Result<(), Failure> {
    let folder = match arguments {
        [flag, folder] if flag == "--out" => PathBuf::from(folder),
        _ => return Err(Failure::Usage("keygen needs --out <folder>".to_owned())),
    };
    let names = [
        "shellune-updater.key",
        "shellune-updater.key.base64",
        "shellune-updater.password",
        "shellune-updater.pub",
    ];
    if let Some(existing) = names
        .iter()
        .map(|name| folder.join(name))
        .find(|path| path.exists())
    {
        return Err(Failure::Failed(format!(
            "{} already exists; a key pair is never overwritten",
            existing.display()
        )));
    }
    fs::create_dir_all(&folder).map_err(|error| {
        Failure::Failed(format!("{} cannot be made: {error}", folder.display()))
    })?;
    // Two random UUIDs: 244 random bits, typeable, no shell metacharacters.
    let password = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let keys = generate_keys(&password).map_err(|error| Failure::Failed(error.to_string()))?;
    // Prove the pair before anything is written.
    let check = SigningKey::open(&keys.secret_key_base64, &password)
        .map_err(|error| Failure::Failed(error.to_string()))?;
    if check.public_key() != keys.public_key {
        return Err(Failure::Failed(
            "the new key pair does not match".to_owned(),
        ));
    }
    for (name, content) in names.iter().zip([
        keys.secret_key_file.as_str(),
        keys.secret_key_base64.as_str(),
        password.as_str(),
        keys.public_key_file.as_str(),
    ]) {
        write_new(&folder.join(name), content)?;
    }
    println!("public key: {}", keys.public_key);
    for name in names {
        println!("wrote {}", folder.join(name).display());
    }
    Ok(())
}

/// Writes a file that must not exist yet.
fn write_new(path: &Path, content: &str) -> Result<(), Failure> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            Failure::Failed(format!("{} cannot be written: {error}", path.display()))
        })?;
    file.write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| Failure::Failed(format!("{} cannot be written: {error}", path.display())))
}
