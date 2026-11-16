//! The updater against a loopback server, with throwaway keys, installing into
//! temporary folders.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

use crate::install::{BundleFailure, replace_app_bundle, replace_portable};
use crate::release::{SignError, SigningKey, generate_keys};
use crate::testing::{
    HttpServer, TempFolder, TestKey, app_tar_gz, config, manifest_json, portable_zip,
};
use crate::*;

const OLD_EXE: &[u8] = b"old shellune 0.4.7";
const NEW_EXE: &[u8] = b"new shellune 1.0.0";

fn portable_route(folder: &Path) -> (InstallRoute, PathBuf) {
    let executable = folder.join("shellune.exe");
    fs::write(&executable, OLD_EXE).unwrap();
    (
        InstallRoute::WindowsPortable {
            executable: executable.clone(),
        },
        executable,
    )
}

/// A server with a 1.0.0 manifest whose portable entry is `zip` signed by `signature`.
fn publish_portable(server: &HttpServer, zip: &[u8], signature: String) {
    server.serve(
        "files/Shellune-1.0.0-windows-x86_64-portable.zip",
        zip.to_vec(),
    );
    let manifest = manifest_json(
        "1.0.0",
        Some("Fixes"),
        &[(
            PlatformKey::WindowsX86_64Portable,
            server.url("files/Shellune-1.0.0-windows-x86_64-portable.zip"),
            signature,
        )],
    );
    server.serve("latest.json", manifest);
}

fn updater(
    server: &HttpServer,
    key: &TestKey,
    current: &str,
    route: InstallRoute,
    downloads: &Path,
) -> Updater {
    Updater::new(config(
        server.url("latest.json"),
        key.public_key(),
        current,
        route,
        downloads,
    ))
    .unwrap()
}

fn available(updater: &Updater) -> Release {
    match updater.check().unwrap() {
        CheckOutcome::Available(release) => release,
        other => panic!("expected a release, got {other:?}"),
    }
}

fn files_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn the_built_in_public_key_is_usable() {
    let mut settings = UpdaterConfig::from_environment();
    settings.public_key = PUBLIC_KEY.to_owned();
    assert!(Updater::new(settings).is_ok());
    assert_eq!(CURRENT_VERSION, env!("CARGO_PKG_VERSION"));
    assert!(ENDPOINT.starts_with("https://github.com/sakullla/shellune/"));
    assert!(ENDPOINT.ends_with("/releases/latest/download/latest.json"));
    assert_eq!(
        release_page(&Version::new(1, 2, 3)),
        "https://github.com/sakullla/shellune/releases/tag/v1.2.3"
    );
}

#[test]
fn manifests_parse_and_versions_compare() {
    let text = r#"{"version":"v1.2.0","notes":"  ","platforms":{
        "windows-x86_64":{"url":"https://x/setup.exe","signature":"c2ln"},
        "darwin-aarch64":{"url":"","signature":"c2ln"},
        "freebsd-x86_64":{"url":"https://x/f","signature":"c2ln"}}}"#;
    let manifest = Manifest::parse(text.as_bytes()).unwrap();
    assert_eq!(manifest.version, Version::new(1, 2, 0));
    assert_eq!(manifest.notes, None);
    assert_eq!(manifest.pub_date, None);
    assert_eq!(
        manifest
            .asset(PlatformKey::WindowsX86_64)
            .map(|a| a.url.as_str()),
        Some("https://x/setup.exe")
    );
    // An empty address is no file; an unknown key is kept but unused.
    assert_eq!(manifest.asset(PlatformKey::DarwinAarch64), None);
    assert!(manifest.platforms.contains_key("freebsd-x86_64"));
    assert_eq!(
        PlatformKey::from_name("windows-x86_64-portable"),
        Some(PlatformKey::WindowsX86_64Portable)
    );
    assert_eq!(
        PlatformKey::from_name("linux-x86_64"),
        Some(PlatformKey::LinuxX86_64)
    );

    for broken in [
        r#"{"version":"1.2","platforms":{}}"#,
        r#"{"platforms":{}}"#,
        "not json",
        r#"{"version":"1.0.0","platforms":{"windows-x86_64":{"signature":"x"}}}"#,
    ] {
        assert!(
            matches!(
                Manifest::parse(broken.as_bytes()),
                Err(UpdateError::InvalidManifest { .. })
            ),
            "{broken}"
        );
    }
    // No platforms at all is still a manifest (the release page is offered).
    assert!(Manifest::parse(br#"{"version":"1.0.0"}"#).is_ok());

    let v = |text: &str| parse_version(text).unwrap();
    assert!(is_newer(&v("0.4.8"), &v("0.4.7")));
    assert!(is_newer(&v("0.10.0"), &v("0.9.9")));
    assert!(!is_newer(&v("0.4.7"), &v("0.4.7")));
    assert!(!is_newer(&v("0.4.6"), &v("0.4.7")));
    assert!(!is_newer(&v("0.5.0-beta.1"), &v("0.5.0")));
    assert!(is_newer(&v("0.5.0"), &v("0.5.0-beta.1")));
    assert!(!is_newer(&v("0.4.7+build.2"), &v("0.4.7")));
}

#[test]
fn a_check_offers_newer_releases_only() {
    let server = HttpServer::start();
    let key = TestKey::generate();
    let downloads = TempFolder::new("check");
    let zip = portable_zip("Shellune-1.0.0-windows-x86_64-portable", NEW_EXE);
    publish_portable(&server, &zip, key.sign(&zip, "1.0.0"));
    let route = InstallRoute::WindowsPortable {
        executable: downloads.path().join("shellune.exe"),
    };

    let release = available(&updater(
        &server,
        &key,
        "0.4.7",
        route.clone(),
        downloads.path(),
    ));
    assert_eq!(release.version, Version::new(1, 0, 0));
    assert_eq!(release.notes.as_deref(), Some("Fixes"));
    assert_eq!(release.pub_date.as_deref(), Some("2026-11-16T00:00:00Z"));
    assert_eq!(
        release.release_page,
        "https://github.com/sakullla/shellune/releases/tag/v1.0.0"
    );
    assert_eq!(
        release.asset.as_ref().map(|(key, _)| *key),
        Some(PlatformKey::WindowsX86_64Portable)
    );
    assert!(release.installable());

    for current in ["1.0.0", "1.0.1"] {
        assert_eq!(
            updater(&server, &key, current, route.clone(), downloads.path())
                .check()
                .unwrap(),
            CheckOutcome::UpToDate {
                latest: Version::new(1, 0, 0)
            }
        );
    }

    // The installed route wants the setup file, which this release lacks; Linux is
    // manual: both are told about the release without a file to install.
    for route in [
        InstallRoute::WindowsInstaller {
            executable: downloads.path().join("shellune.exe"),
        },
        InstallRoute::Manual {
            reason: ManualReason::Platform,
        },
    ] {
        let release = available(&updater(&server, &key, "0.4.7", route, downloads.path()));
        assert_eq!(release.asset, None);
        assert!(!release.installable());
    }

    // The manifest is fetched through a redirect, as GitHub serves it.
    server.redirect("redirected.json", "latest.json");
    let mut settings = updater(&server, &key, "0.4.7", route, downloads.path())
        .config()
        .clone();
    settings.endpoint = server.url("redirected.json");
    assert!(matches!(
        Updater::new(settings).unwrap().check(),
        Ok(CheckOutcome::Available(_))
    ));
}

#[test]
fn check_failures_are_told_apart() {
    let server = HttpServer::start();
    let key = TestKey::generate();
    let downloads = TempFolder::new("check-failures");
    let route = InstallRoute::Manual {
        reason: ManualReason::Platform,
    };
    let checker = updater(&server, &key, "0.4.7", route.clone(), downloads.path());

    // No manifest published.
    assert_eq!(
        checker.check(),
        Err(UpdateError::HttpStatus { status: 404 })
    );
    server.serve_status("latest.json", 503);
    assert_eq!(
        checker.check(),
        Err(UpdateError::HttpStatus { status: 503 })
    );
    server.serve("latest.json", "<html>");
    assert!(matches!(
        checker.check(),
        Err(UpdateError::InvalidManifest { .. })
    ));

    // A server that takes longer than the limit.
    server.serve_delayed(
        "latest.json",
        manifest_json("1.0.0", None, &[]),
        Duration::from_secs(3),
    );
    let mut settings = checker.config().clone();
    settings.check_timeout = Duration::from_millis(500);
    assert_eq!(
        Updater::new(settings).unwrap().check(),
        Err(UpdateError::Timeout)
    );

    // Connections dropped without an answer.
    server.set_offline(true);
    assert!(matches!(
        checker.check(),
        Err(UpdateError::Unreachable { .. })
    ));

    // Nothing listening at all.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let mut settings = checker.config().clone();
    settings.endpoint = format!("http://{closed}/latest.json");
    assert!(matches!(
        Updater::new(settings).unwrap().check(),
        Err(UpdateError::Unreachable { .. })
    ));

    // A key that is not a key is refused up front.
    let mut settings = checker.config().clone();
    settings.public_key = "not a key".to_owned();
    assert!(matches!(
        Updater::new(settings),
        Err(UpdateError::InvalidConfig { .. })
    ));
}

#[test]
fn a_signed_portable_update_downloads_installs_and_cleans_up() {
    let server = HttpServer::start();
    let key = TestKey::generate();
    let app = TempFolder::new("portable-app");
    let downloads = TempFolder::new("portable-downloads");
    let (route, executable) = portable_route(app.path());
    let zip = portable_zip("Shellune-1.0.0-windows-x86_64-portable", NEW_EXE);
    publish_portable(&server, &zip, key.sign(&zip, "1.0.0"));
    let updater = updater(&server, &key, "0.4.7", route, downloads.path());
    let release = available(&updater);

    let mut seen = Vec::new();
    let update = updater
        .download(&release, &mut |progress| seen.push(progress))
        .unwrap();
    assert_eq!(update.version(), &Version::new(1, 0, 0));
    assert_eq!(update.platform(), PlatformKey::WindowsX86_64Portable);
    assert_eq!(fs::read(update.path()).unwrap(), zip);
    assert_eq!(
        files_in(downloads.path()),
        ["Shellune-1.0.0-windows-x86_64-portable.zip"]
    );
    let total = zip.len() as u64;
    assert_eq!(seen.first().map(|p| p.downloaded), Some(0));
    assert_eq!(
        seen.last(),
        Some(&DownloadProgress {
            downloaded: total,
            total: Some(total)
        })
    );
    assert!(
        seen.windows(2)
            .all(|pair| pair[0].downloaded <= pair[1].downloaded)
    );
    // Downloading touched nothing in the application's folder.
    assert_eq!(fs::read(&executable).unwrap(), OLD_EXE);

    assert_eq!(
        updater.install(&update).unwrap(),
        InstallOutcome::RestartRequired(RestartTarget::Executable(executable.clone()))
    );
    assert_eq!(fs::read(&executable).unwrap(), NEW_EXE);
    assert_eq!(
        fs::read(app.path().join("shellune.exe.old")).unwrap(),
        OLD_EXE
    );
    assert_eq!(files_in(app.path()), ["shellune.exe", "shellune.exe.old"]);

    // The next start removes the old executable; doing it twice is harmless.
    cleanup_portable_leftovers(&executable).unwrap();
    cleanup_portable_leftovers(&executable).unwrap();
    assert_eq!(files_in(app.path()), ["shellune.exe"]);

    // A second update over a leftover .old still works.
    fs::write(app.path().join("shellune.exe.old"), b"stale").unwrap();
    assert!(updater.install(&update).is_ok());
    assert_eq!(
        fs::read(app.path().join("shellune.exe.old")).unwrap(),
        NEW_EXE
    );
}

#[test]
fn tampered_unsigned_and_misversioned_files_are_refused() {
    let server = HttpServer::start();
    let key = TestKey::generate();
    let other_key = TestKey::generate();
    let app = TempFolder::new("refused-app");
    let downloads = TempFolder::new("refused-downloads");
    let (route, executable) = portable_route(app.path());
    let zip = portable_zip("Shellune-1.0.0-windows-x86_64-portable", NEW_EXE);
    let mut tampered = zip.clone();
    let middle = tampered.len() / 2;
    tampered[middle] ^= 0x55;

    type Expected = fn(&UpdateError) -> bool;
    let cases: Vec<(Vec<u8>, String, Expected)> = vec![
        (tampered, key.sign(&zip, "1.0.0"), |error| {
            matches!(error, UpdateError::SignatureInvalid { .. })
        }),
        (zip.clone(), other_key.sign(&zip, "1.0.0"), |error| {
            matches!(error, UpdateError::SignatureInvalid { .. })
        }),
        // A genuine signature for an older version, served as 1.0.0.
        (zip.clone(), key.sign(&zip, "0.9.0"), |error| {
            *error
                == UpdateError::VersionMismatch {
                    expected: "1.0.0".to_owned(),
                    signed: "0.9.0".to_owned(),
                }
        }),
        // A prefix of the version is not the version.
        (
            zip.clone(),
            key.sign_with_comment(&zip, "timestamp:1\tversion:1.0.0.1"),
            |error| matches!(error, UpdateError::VersionMismatch { .. }),
        ),
        (
            zip.clone(),
            key.sign_with_comment(&zip, "timestamp:1\tfile:x"),
            |error| {
                *error
                    == UpdateError::VersionMismatch {
                        expected: "1.0.0".to_owned(),
                        signed: "no version".to_owned(),
                    }
            },
        ),
        (zip.clone(), "bm90IGEgc2lnbmF0dXJl".to_owned(), |error| {
            matches!(error, UpdateError::SignatureInvalid { .. })
        }),
        (zip.clone(), "%%%".to_owned(), |error| {
            matches!(error, UpdateError::SignatureInvalid { .. })
        }),
    ];
    for (served, signature, expected) in cases {
        publish_portable(&server, &served, signature);
        let updater = updater(&server, &key, "0.4.7", route.clone(), downloads.path());
        let release = available(&updater);
        let error = updater.download(&release, &mut |_| {}).unwrap_err();
        assert!(expected(&error), "unexpected {error:?}");
        assert!(
            files_in(downloads.path()).is_empty(),
            "a refused file was kept"
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_EXE);
        assert_eq!(files_in(app.path()), ["shellune.exe"]);
    }

    // A manifest entry without a signature is not offered for install, and a
    // release that claims one anyway is refused before any download.
    publish_portable(&server, &zip, String::new());
    let updater = updater(&server, &key, "0.4.7", route, downloads.path());
    let mut release = available(&updater);
    assert_eq!(release.asset, None);
    assert_eq!(
        updater.download(&release, &mut |_| {}),
        Err(UpdateError::NoInstallableAsset)
    );
    release.asset = Some((
        PlatformKey::WindowsX86_64Portable,
        Asset {
            url: server.url("files/Shellune-1.0.0-windows-x86_64-portable.zip"),
            signature: " ".to_owned(),
        },
    ));
    let before = server.requests().len();
    assert_eq!(
        updater.download(&release, &mut |_| {}),
        Err(UpdateError::SignatureMissing)
    );
    assert_eq!(server.requests().len(), before);
}

#[test]
fn interrupted_and_failed_downloads_leave_nothing() {
    let server = HttpServer::start();
    let key = TestKey::generate();
    let app = TempFolder::new("interrupted-app");
    let downloads = TempFolder::new("interrupted-downloads");
    let (route, executable) = portable_route(app.path());
    let zip = portable_zip("Shellune-1.0.0-windows-x86_64-portable", NEW_EXE);
    publish_portable(&server, &zip, key.sign(&zip, "1.0.0"));
    let updater = updater(&server, &key, "0.4.7", route, downloads.path());
    let release = available(&updater);

    server.serve_truncated(
        "files/Shellune-1.0.0-windows-x86_64-portable.zip",
        zip.clone(),
        zip.len() / 3,
    );
    assert!(matches!(
        updater.download(&release, &mut |_| {}),
        Err(UpdateError::DownloadInterrupted { .. })
    ));
    assert!(files_in(downloads.path()).is_empty());

    server.serve_status("files/Shellune-1.0.0-windows-x86_64-portable.zip", 404);
    assert_eq!(
        updater.download(&release, &mut |_| {}),
        Err(UpdateError::HttpStatus { status: 404 })
    );
    server.set_offline(true);
    assert!(matches!(
        updater.download(&release, &mut |_| {}),
        Err(UpdateError::Unreachable { .. })
    ));
    assert!(files_in(downloads.path()).is_empty());
    assert_eq!(fs::read(&executable).unwrap(), OLD_EXE);

    // Back online, through a redirect like GitHub's download links.
    server.set_offline(false);
    server.serve("files/real.zip", zip.clone());
    server.redirect(
        "files/Shellune-1.0.0-windows-x86_64-portable.zip",
        "files/real.zip",
    );
    let update = updater.download(&release, &mut |_| {}).unwrap();
    assert_eq!(fs::read(update.path()).unwrap(), zip);
}

#[test]
fn install_checks_the_file_again_and_follows_the_route() {
    let server = HttpServer::start();
    let key = TestKey::generate();
    let app = TempFolder::new("install-app");
    let downloads = TempFolder::new("install-downloads");
    let (route, executable) = portable_route(app.path());
    let zip = portable_zip("Shellune-1.0.0-windows-x86_64-portable", NEW_EXE);
    publish_portable(&server, &zip, key.sign(&zip, "1.0.0"));
    let portable = updater(&server, &key, "0.4.7", route, downloads.path());
    let update = portable
        .download(&available(&portable), &mut |_| {})
        .unwrap();

    // Changed on disk after the download.
    let mut changed = zip.clone();
    changed[10] ^= 1;
    fs::write(update.path(), &changed).unwrap();
    assert!(matches!(
        portable.install(&update),
        Err(UpdateError::SignatureInvalid { .. })
    ));
    assert_eq!(fs::read(&executable).unwrap(), OLD_EXE);
    fs::write(update.path(), &zip).unwrap();

    // The manual route changes nothing and points at the release page.
    let manual = updater(
        &server,
        &key,
        "0.4.7",
        InstallRoute::Manual {
            reason: ManualReason::DebugBuild,
        },
        downloads.path(),
    );
    assert_eq!(
        manual.install(&update).unwrap(),
        InstallOutcome::Manual {
            release_page: "https://github.com/sakullla/shellune/releases/tag/v1.0.0".to_owned(),
            reason: ManualReason::DebugBuild,
        }
    );
    // A file for another route is not installed.
    let mac = updater(
        &server,
        &key,
        "0.4.7",
        InstallRoute::MacApp {
            bundle: app.path().join("Shellune.app"),
        },
        downloads.path(),
    );
    assert!(matches!(
        mac.install(&update),
        Err(UpdateError::Install { .. })
    ));
    assert_eq!(fs::read(&executable).unwrap(), OLD_EXE);
}

#[test]
fn a_failed_portable_swap_puts_the_old_executable_back() {
    let app = TempFolder::new("portable-rollback");
    let (_, executable) = portable_route(app.path());
    let archive = app.path().join("update.zip");
    fs::write(
        &archive,
        portable_zip("Shellune-1.0.0-windows-x86_64-portable", NEW_EXE),
    )
    .unwrap();

    let error = replace_portable(&archive, &executable, &|| {
        Err(std::io::Error::other("simulated failure"))
    })
    .unwrap_err();
    assert!(matches!(error, UpdateError::Install { .. }), "{error:?}");
    assert_eq!(fs::read(&executable).unwrap(), OLD_EXE);
    assert_eq!(files_in(app.path()), ["shellune.exe", "update.zip"]);

    // An archive without the executable, and one that is not an archive.
    let empty = app.path().join("empty.zip");
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(
            "Shellune/README.txt",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    fs::write(&empty, writer.finish().unwrap().into_inner()).unwrap();
    let broken = app.path().join("broken.zip");
    fs::write(&broken, b"not a zip").unwrap();
    for archive in [&empty, &broken] {
        assert!(matches!(
            replace_portable(archive, &executable, &|| Ok(())),
            Err(UpdateError::Install { .. })
        ));
        assert_eq!(fs::read(&executable).unwrap(), OLD_EXE);
    }
    assert_eq!(
        files_in(app.path()),
        ["broken.zip", "empty.zip", "shellune.exe", "update.zip"]
    );

    // The executable nearest the top wins over a nested one.
    let nested = app.path().join("nested.zip");
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in [
        ("a/b/shellune.exe", b"deep".as_slice()),
        ("a/shellune.exe", NEW_EXE),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, data).unwrap();
    }
    fs::write(&nested, writer.finish().unwrap().into_inner()).unwrap();
    replace_portable(&nested, &executable, &|| Ok(())).unwrap();
    assert_eq!(fs::read(&executable).unwrap(), NEW_EXE);
}

#[test]
fn a_mac_bundle_is_replaced_in_place_or_restored() {
    let folder = TempFolder::new("mac");
    let bundle = folder.path().join("Shellune.app");
    let old_binary = bundle.join("Contents/MacOS/shellune");
    fs::create_dir_all(old_binary.parent().unwrap()).unwrap();
    fs::write(&old_binary, OLD_EXE).unwrap();
    let archive = folder.path().join("update.app.tar.gz");
    fs::write(&archive, app_tar_gz("Shellune", NEW_EXE)).unwrap();

    // A failure after the old bundle was moved aside puts it back.
    let failure = replace_app_bundle(&archive, &bundle, &|| {
        Err(std::io::Error::other("simulated failure"))
    });
    assert!(matches!(
        failure,
        Err(BundleFailure::Failed(UpdateError::Install { .. }))
    ));
    assert_eq!(fs::read(&old_binary).unwrap(), OLD_EXE);
    assert_eq!(
        files_in(folder.path()),
        ["Shellune.app", "update.app.tar.gz"]
    );

    // An archive without an application changes nothing.
    let wrong = folder.path().join("wrong.tar.gz");
    {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "readme.txt", b"x".as_slice())
            .unwrap();
        fs::write(&wrong, builder.into_inner().unwrap().finish().unwrap()).unwrap();
    }
    assert!(matches!(
        replace_app_bundle(&wrong, &bundle, &|| Ok(())),
        Err(BundleFailure::Failed(_))
    ));
    assert_eq!(fs::read(&old_binary).unwrap(), OLD_EXE);
    fs::remove_file(&wrong).unwrap();

    // The real swap: the new bundle takes the old one's place.
    replace_app_bundle(&archive, &bundle, &|| Ok(())).unwrap();
    assert_eq!(fs::read(&old_binary).unwrap(), NEW_EXE);
    assert_eq!(
        fs::read(bundle.join("Contents/Info.plist")).unwrap(),
        b"<plist/>"
    );
    // No staging folder or backup is left behind.
    assert_eq!(
        files_in(folder.path()),
        ["Shellune.app", "update.app.tar.gz"]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&old_binary).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the executable keeps its execute bits");
    }
}

#[test]
fn a_mac_update_installs_through_the_updater() {
    let server = HttpServer::start();
    let key = TestKey::generate();
    let folder = TempFolder::new("mac-updater");
    let downloads = TempFolder::new("mac-downloads");
    let bundle = folder.path().join("Shellune.app");
    let binary = bundle.join("Contents/MacOS/shellune");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, OLD_EXE).unwrap();
    let archive = app_tar_gz("Shellune", NEW_EXE);
    server.serve(
        "files/Shellune-1.0.0-macos-arm64.app.tar.gz",
        archive.clone(),
    );
    server.serve(
        "latest.json",
        manifest_json(
            "1.0.0",
            None,
            &[(
                PlatformKey::DarwinAarch64,
                server.url("files/Shellune-1.0.0-macos-arm64.app.tar.gz"),
                key.sign(&archive, "1.0.0"),
            )],
        ),
    );
    let updater = updater(
        &server,
        &key,
        "0.4.7",
        InstallRoute::MacApp {
            bundle: bundle.clone(),
        },
        downloads.path(),
    );
    let update = updater.download(&available(&updater), &mut |_| {}).unwrap();
    assert_eq!(
        updater.install(&update).unwrap(),
        InstallOutcome::RestartRequired(RestartTarget::AppBundle(bundle.clone()))
    );
    assert_eq!(fs::read(&binary).unwrap(), NEW_EXE);
}

#[test]
fn signatures_from_an_encrypted_release_key_verify() {
    let keys = generate_keys("correct horse").unwrap();
    assert!(keys.secret_key_file.starts_with("untrusted comment:"));
    assert_eq!(
        SigningKey::open(&keys.secret_key_base64, "wrong").map(|_| ()),
        Err(SignError::Key("Wrong password for that key".to_owned()))
    );
    let signer = SigningKey::open(&keys.secret_key_base64, "correct horse").unwrap();
    assert_eq!(signer.public_key(), keys.public_key);
    // The secret key file's own text opens too.
    assert!(SigningKey::open(&keys.secret_key_file, "correct horse").is_ok());

    let data = b"Shellune-1.0.0-windows-x86_64-setup.exe".repeat(1000);
    let signature = signer
        .sign(
            data.as_slice(),
            "Shellune-1.0.0-windows-x86_64-setup.exe",
            "v1.0.0",
        )
        .unwrap();
    let version = Version::new(1, 0, 0);
    verify_bytes(&keys.public_key, &signature, &data, &version).unwrap();
    verify_bytes(&keys.public_key_file, &signature, &data, &version).unwrap();
    assert!(matches!(
        verify_bytes(&keys.public_key, &signature, &data, &Version::new(1, 0, 1)),
        Err(UpdateError::VersionMismatch { .. })
    ));
    assert!(matches!(
        verify_bytes(&keys.public_key, &signature, b"other", &version),
        Err(UpdateError::SignatureInvalid { .. })
    ));
    assert_eq!(
        signer.sign(data.as_slice(), "x", "1.0").map(|_| ()),
        Err(SignError::Version("1.0".to_owned()))
    );
}
