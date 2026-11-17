//! How this copy is updated: which route it is on, and putting a verified update
//! file in place for that route.
//!
//! * Windows, installed (`HKCU\Software\Shellune\InstallDir` is the folder of the
//!   running executable): start the downloaded installer with `/S /R` (silent, then
//!   start the new version); the caller then exits so the installer can replace
//!   the files.
//! * Windows, portable (anything else): take `shellune.exe` out of the portable
//!   archive into the executable's folder under a temporary name, rename the
//!   running executable to `<name>.old`, move the new one into place. Any failure
//!   puts the old name back. The next start removes the `.old` file
//!   ([`cleanup_portable_leftovers`]).
//! * macOS, inside `X.app/Contents/MacOS`: unpack the `.app.tar.gz` into a folder
//!   beside the bundle (same volume, so the moves are renames), move the old
//!   bundle aside, move the new one into place, then delete the old one. Any
//!   failure moves the old bundle back. When the folder cannot be written the
//!   update is left to the user (no elevation is asked for).
//! * Linux and anything else, and debug builds: the release page only.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::manifest::PlatformKey;
use crate::{DEBUG_INSTALL_PATH_VARIABLE, UpdateError};

/// The arguments the Windows installer is started with: silent, and start the
/// new version when done.
const INSTALLER_ARGUMENTS: [&str; 2] = ["/S", "/R"];

/// The executable inside the portable archive.
const PORTABLE_EXECUTABLE: &str = "shellune.exe";

/// The largest executable taken out of a portable archive.
const PORTABLE_EXECUTABLE_LIMIT: u64 = 512 * 1024 * 1024;

/// Why an update is left to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualReason {
    /// This platform or packaging (a Linux package, an unknown system) is updated
    /// from the release page.
    Platform,
    /// A debug build, without [`DEBUG_INSTALL_PATH_VARIABLE`].
    DebugBuild,
    /// The application's folder cannot be written without elevation.
    NoPermission,
}

/// How this copy is updated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallRoute {
    /// Installed on Windows: the installer replaces it.
    WindowsInstaller {
        /// The running executable.
        executable: PathBuf,
    },
    /// Portable on Windows: the executable is swapped in place.
    WindowsPortable {
        /// The running executable.
        executable: PathBuf,
    },
    /// A macOS application bundle, replaced in place.
    MacApp {
        /// The `.app` folder.
        bundle: PathBuf,
    },
    /// Only the release page is offered.
    Manual {
        /// Why.
        reason: ManualReason,
    },
}

impl InstallRoute {
    /// The manifest entry this route downloads, if it downloads anything.
    #[must_use]
    pub const fn platform_key(&self) -> Option<PlatformKey> {
        match self {
            Self::WindowsInstaller { .. } => Some(PlatformKey::WindowsX86_64),
            Self::WindowsPortable { .. } => Some(PlatformKey::WindowsX86_64Portable),
            Self::MacApp { .. } => Some(PlatformKey::DarwinAarch64),
            Self::Manual { .. } => None,
        }
    }
}

/// What to start again after an in-place update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestartTarget {
    /// An executable (the portable copy).
    Executable(PathBuf),
    /// A macOS application bundle (`open -n`).
    AppBundle(PathBuf),
}

/// What an install did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The Windows installer is running; the application should save its state and
    /// exit now so the installer can replace it (it starts the new version).
    InstallerStarted,
    /// The new version is in place and runs from the next start ([`relaunch`]).
    RestartRequired(RestartTarget),
    /// Nothing was changed; the user updates from the release page.
    Manual {
        /// The page.
        release_page: String,
        /// Why.
        reason: ManualReason,
    },
}

/// The route for `executable` on the system `os` / `arch` (the values of
/// `std::env::consts::OS` / `ARCH`), given the folder the Windows installer
/// recorded. No file is read except to compare folders.
#[must_use]
pub fn route_for(
    os: &str,
    arch: &str,
    executable: &Path,
    install_dir: Option<&Path>,
) -> InstallRoute {
    let manual = InstallRoute::Manual {
        reason: ManualReason::Platform,
    };
    match (os, arch) {
        ("windows", "x86_64") => {
            let installed = windows_parent(executable)
                .zip(install_dir)
                .is_some_and(|(folder, installed)| same_folder(&folder, installed, true));
            let executable = executable.to_path_buf();
            if installed {
                InstallRoute::WindowsInstaller { executable }
            } else {
                InstallRoute::WindowsPortable { executable }
            }
        }
        ("macos", "aarch64") => {
            app_bundle_of(executable).map_or(manual, |bundle| InstallRoute::MacApp { bundle })
        }
        _ => manual,
    }
}

/// The route for `executable` on this system (reading the installer's registry
/// value on Windows).
#[must_use]
pub fn route_for_executable(executable: &Path) -> InstallRoute {
    let install_dir = installed_folder();
    route_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        executable,
        install_dir.as_deref(),
    )
}

/// The route of the running copy. A debug build is on the manual route unless
/// [`DEBUG_INSTALL_PATH_VARIABLE`] names an executable to act as.
#[must_use]
pub fn current_route() -> InstallRoute {
    if cfg!(debug_assertions) {
        return crate::debug_override(DEBUG_INSTALL_PATH_VARIABLE).map_or(
            InstallRoute::Manual {
                reason: ManualReason::DebugBuild,
            },
            |path| route_for_executable(Path::new(&path)),
        );
    }
    std::env::current_exe().map_or(
        InstallRoute::Manual {
            reason: ManualReason::Platform,
        },
        |executable| route_for_executable(&executable),
    )
}

/// The arguments the Windows installer is started with (`/S /R`).
#[must_use]
pub fn installer_arguments() -> &'static [&'static str] {
    &INSTALLER_ARGUMENTS
}

/// Removes what a portable update left beside `executable`: the previous
/// executable (`<name>.old`, still in use until the restart) and an unfinished
/// `<name>.new`. Missing files are fine.
///
/// # Errors
///
/// [`UpdateError::Io`] when a leftover exists but cannot be removed.
pub fn cleanup_portable_leftovers(executable: &Path) -> Result<(), UpdateError> {
    for path in [sibling(executable, ".old"), sibling(executable, ".new")] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(UpdateError::io(
                    format!("{} cannot be removed", path.display()),
                    &error,
                ));
            }
        }
    }
    Ok(())
}

/// Starts the new version.
///
/// # Errors
///
/// The system's error when it cannot be started.
pub fn relaunch(target: &RestartTarget) -> io::Result<()> {
    match target {
        RestartTarget::Executable(executable) => {
            let mut command = Command::new(executable);
            if let Some(folder) = executable.parent() {
                command.current_dir(folder);
            }
            command.spawn().map(drop)
        }
        RestartTarget::AppBundle(bundle) => {
            Command::new("open").arg("-n").arg(bundle).spawn().map(drop)
        }
    }
}

/// Starts the Windows installer silently; the caller exits next.
pub(crate) fn launch_installer(installer: &Path) -> Result<InstallOutcome, UpdateError> {
    let mut command = Command::new(installer);
    command.args(INSTALLER_ARGUMENTS);
    if let Some(folder) = installer.parent() {
        command.current_dir(folder);
    }
    command.spawn().map_err(|error| {
        UpdateError::install(format!("the installer could not be started: {error}"))
    })?;
    Ok(InstallOutcome::InstallerStarted)
}

/// `path` with `suffix` added to its file name (`shellune.exe` → `shellune.exe.old`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Swaps the portable executable for the one in `archive`. `before_swap` runs
/// after the running executable has been renamed and before the new one is moved
/// into place (tests make it fail).
pub(crate) fn replace_portable(
    archive: &Path,
    executable: &Path,
    before_swap: &dyn Fn() -> io::Result<()>,
) -> Result<(), UpdateError> {
    let staged = sibling(executable, ".new");
    let old = sibling(executable, ".old");
    let failed = |what: &str, error: &dyn std::fmt::Display| {
        UpdateError::install(format!("{what}: {error}"))
    };

    extract_portable_executable(archive, &staged).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })?;
    #[cfg(unix)]
    if let Ok(metadata) = fs::metadata(executable) {
        let _ = fs::set_permissions(&staged, metadata.permissions());
    }

    // A previous update's executable is normally gone after the restart; if it is
    // still there it must go before the current one can take its name.
    match fs::remove_file(&old) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let _ = fs::remove_file(&staged);
            return Err(failed(
                "the previous update's executable cannot be removed",
                &error,
            ));
        }
    }
    if let Err(error) = fs::rename(executable, &old) {
        let _ = fs::remove_file(&staged);
        return Err(failed("the running executable cannot be renamed", &error));
    }
    if let Err(error) = before_swap().and_then(|()| fs::rename(&staged, executable)) {
        let restored = fs::rename(&old, executable);
        let _ = fs::remove_file(&staged);
        return Err(match restored {
            Ok(()) => failed("the new executable cannot be moved into place", &error),
            Err(restore) => UpdateError::install(format!(
                "the new executable cannot be moved into place ({error}), and the old one \
                 could not be renamed back from {} ({restore})",
                old.display()
            )),
        });
    }
    Ok(())
}

/// Writes the archive's `shellune.exe` (the one nearest the top) to `target`.
fn extract_portable_executable(archive: &Path, target: &Path) -> Result<(), UpdateError> {
    let invalid = |reason: String| UpdateError::install(format!("the portable archive {reason}"));
    let file = File::open(archive)
        .map_err(|error| UpdateError::io("the update file cannot be read", &error))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|error| invalid(format!("cannot be read: {error}")))?;
    let mut best: Option<(usize, usize)> = None;
    for index in 0..zip.len() {
        let entry = zip
            .by_index(index)
            .map_err(|error| invalid(format!("cannot be read: {error}")))?;
        if !entry.is_file() {
            continue;
        }
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let matches = name
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(PORTABLE_EXECUTABLE));
        let depth = name.components().count();
        if matches && best.is_none_or(|(_, best_depth)| depth < best_depth) {
            best = Some((index, depth));
        }
    }
    let Some((index, _)) = best else {
        return Err(invalid(format!("has no {PORTABLE_EXECUTABLE}")));
    };
    let mut entry = zip
        .by_index(index)
        .map_err(|error| invalid(format!("cannot be read: {error}")))?;
    if entry.size() > PORTABLE_EXECUTABLE_LIMIT {
        return Err(invalid(format!(
            "holds a {PORTABLE_EXECUTABLE} that is too large"
        )));
    }
    let mut output = File::create(target).map_err(|error| {
        UpdateError::install(format!("the new executable cannot be written: {error}"))
    })?;
    io::copy(
        &mut io::Read::take(&mut entry, PORTABLE_EXECUTABLE_LIMIT),
        &mut output,
    )
    .and_then(|_| output.sync_all())
    .map_err(|error| invalid(format!("cannot be unpacked: {error}")))?;
    Ok(())
}

/// How replacing a macOS bundle ended when it did not succeed.
#[derive(Debug)]
pub(crate) enum BundleFailure {
    /// The folder cannot be written; nothing was changed.
    NoPermission,
    /// Something else failed; anything changed has been put back.
    Failed(UpdateError),
}

/// Replaces `bundle` with the `.app` in `archive` (a `.tar.gz`). `before_swap`
/// runs after the old bundle has been moved aside and before the new one is moved
/// into place (tests make it fail).
pub(crate) fn replace_app_bundle(
    archive: &Path,
    bundle: &Path,
    before_swap: &dyn Fn() -> io::Result<()>,
) -> Result<(), BundleFailure> {
    let no_permission = |error: &io::Error| {
        matches!(
            error.kind(),
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
        )
    };
    let failed = |what: &str, error: &dyn std::fmt::Display| {
        BundleFailure::Failed(UpdateError::install(format!("{what}: {error}")))
    };
    let Some(parent) = bundle.parent() else {
        return Err(BundleFailure::Failed(UpdateError::install(
            "the application has no enclosing folder",
        )));
    };
    let bundle_name = bundle
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let staging = parent.join(format!(".shellune-update-{id}"));
    let backup = parent.join(format!(".{bundle_name}.old-{id}"));

    if let Err(error) = fs::create_dir(&staging) {
        return Err(if no_permission(&error) {
            BundleFailure::NoPermission
        } else {
            failed("a folder beside the application cannot be made", &error)
        });
    }
    let cleanup = || {
        let _ = fs::remove_dir_all(&staging);
    };

    let unpacked = File::open(archive).and_then(|file| {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(BufReader::new(file)));
        tar.set_preserve_permissions(true);
        tar.set_preserve_mtime(true);
        tar.set_overwrite(true);
        tar.unpack(&staging)
    });
    if let Err(error) = unpacked {
        cleanup();
        return Err(if no_permission(&error) {
            BundleFailure::NoPermission
        } else {
            failed("the application archive cannot be unpacked", &error)
        });
    }
    let Some(new_bundle) = find_app(&staging) else {
        cleanup();
        return Err(BundleFailure::Failed(UpdateError::install(
            "the application archive has no .app folder",
        )));
    };

    if let Err(error) = fs::rename(bundle, &backup) {
        cleanup();
        return Err(if no_permission(&error) {
            BundleFailure::NoPermission
        } else {
            failed("the current application cannot be moved aside", &error)
        });
    }
    if let Err(error) = before_swap().and_then(|()| fs::rename(&new_bundle, bundle)) {
        let restored = fs::rename(&backup, bundle);
        cleanup();
        return Err(match restored {
            Ok(()) => failed("the new application cannot be moved into place", &error),
            Err(restore) => BundleFailure::Failed(UpdateError::install(format!(
                "the new application cannot be moved into place ({error}), and the old one \
                 could not be moved back from {} ({restore})",
                backup.display()
            ))),
        });
    }
    let _ = fs::remove_dir_all(&backup);
    cleanup();
    Ok(())
}

/// The first `.app` folder directly inside `folder`, by name.
fn find_app(folder: &Path) -> Option<PathBuf> {
    let mut apps: Vec<PathBuf> = fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })
        .collect();
    apps.sort();
    apps.into_iter().next()
}

/// The `X.app` an executable at `X.app/Contents/MacOS/<name>` belongs to.
fn app_bundle_of(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = |path: &Path, name: &str| path.file_name().is_some_and(|file| file == name);
    let is_app = bundle
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"));
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then(|| bundle.to_path_buf())
}

/// The folder of a Windows executable path, split on either separator. `Path::parent`
/// only knows `\` on Windows, and the route is also decided (and tested) elsewhere.
fn windows_parent(executable: &Path) -> Option<PathBuf> {
    let text = executable.to_string_lossy();
    let cut = text.rfind(['\\', '/'])?;
    Some(PathBuf::from(&text[..cut]))
}

/// Whether two folders are the same: by their resolved paths when both exist,
/// else by their text (case-insensitive on Windows), ignoring a trailing
/// separator.
fn same_folder(left: &Path, right: &Path, ignore_case: bool) -> bool {
    if let (Ok(left), Ok(right)) = (fs::canonicalize(left), fs::canonicalize(right)) {
        return text_equal(&left, &right, ignore_case);
    }
    text_equal(left, right, ignore_case)
}

fn text_equal(left: &Path, right: &Path, ignore_case: bool) -> bool {
    let normalize = |path: &Path| {
        let text = path.to_string_lossy().replace('/', "\\");
        let text = text.trim_end_matches('\\').to_owned();
        if ignore_case {
            text.to_lowercase()
        } else {
            text
        }
    };
    normalize(left) == normalize(right)
}

/// `HKCU\Software\Shellune\InstallDir`, written by the installer.
#[cfg(windows)]
fn installed_folder() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide("Software\\Shellune");
    let value = wide("InstallDir");
    let mut size: u32 = 0;
    // SAFETY: both names are NUL-terminated UTF-16; with no buffer the call only
    // reports the size the value needs, in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    if status != ERROR_SUCCESS || size < 2 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let mut size = u32::try_from(buffer.len() * 2).ok()?;
    // SAFETY: the buffer holds `size` bytes, and the call writes at most that many.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &raw mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    let text = std::ffi::OsString::from_wide(&buffer[..length]);
    (!text.is_empty()).then(|| PathBuf::from(text))
}

#[cfg(not(windows))]
fn installed_folder() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installer_folder_decides_between_installed_and_portable() {
        let exe = Path::new(r"C:\Users\me\AppData\Local\Programs\Shellune\shellune.exe");
        let installed = Path::new(r"c:\users\me\appdata\local\programs\shellune\");
        assert_eq!(
            route_for("windows", "x86_64", exe, Some(installed)),
            InstallRoute::WindowsInstaller {
                executable: exe.to_path_buf()
            }
        );
        assert_eq!(
            route_for("windows", "x86_64", exe, Some(Path::new(r"D:\Elsewhere"))),
            InstallRoute::WindowsPortable {
                executable: exe.to_path_buf()
            }
        );
        assert_eq!(
            route_for("windows", "x86_64", exe, None),
            InstallRoute::WindowsPortable {
                executable: exe.to_path_buf()
            }
        );
        assert_eq!(
            route_for("windows", "aarch64", exe, Some(installed)),
            InstallRoute::Manual {
                reason: ManualReason::Platform
            }
        );
    }

    #[test]
    fn a_mac_executable_inside_an_app_bundle_replaces_the_bundle() {
        let exe = Path::new("/Applications/Shellune.app/Contents/MacOS/shellune");
        assert_eq!(
            route_for("macos", "aarch64", exe, None),
            InstallRoute::MacApp {
                bundle: PathBuf::from("/Applications/Shellune.app")
            }
        );
        let manual = InstallRoute::Manual {
            reason: ManualReason::Platform,
        };
        // Outside a bundle (a cargo build), or on an Intel Mac (no release file).
        assert_eq!(
            route_for(
                "macos",
                "aarch64",
                Path::new("/Users/me/target/shellune"),
                None
            ),
            manual
        );
        assert_eq!(
            route_for(
                "macos",
                "aarch64",
                Path::new("/x/Shellune/Contents/MacOS/shellune"),
                None
            ),
            manual
        );
        assert_eq!(route_for("macos", "x86_64", exe, None), manual);
        assert_eq!(
            route_for("linux", "x86_64", Path::new("/usr/bin/shellune"), None),
            manual
        );
        assert_eq!(
            InstallRoute::MacApp {
                bundle: PathBuf::from("/Applications/Shellune.app")
            }
            .platform_key(),
            Some(PlatformKey::DarwinAarch64)
        );
        assert_eq!(manual.platform_key(), None);
    }

    #[test]
    fn the_installer_runs_silently_and_restarts() {
        assert_eq!(installer_arguments(), ["/S", "/R"]);
    }

    #[test]
    fn a_debug_build_without_an_install_path_is_manual() {
        if cfg!(debug_assertions) && std::env::var_os(DEBUG_INSTALL_PATH_VARIABLE).is_none() {
            assert_eq!(
                current_route(),
                InstallRoute::Manual {
                    reason: ManualReason::DebugBuild
                }
            );
        }
    }
}
