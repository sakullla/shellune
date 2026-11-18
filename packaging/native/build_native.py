#!/usr/bin/env python3
"""Build the pinned native desktop dependencies of Shellune.

The RDP and VNC sessions in crates/desktop need two protocol libraries. This
script builds both from source into one prefix and writes the manifest
crates/desktop/build.rs reads:

    target/native/dist/shellune-native.txt

Three platforms are built: Windows x86_64 with MSVC, macOS arm64 and Linux x86_64
with the system C compiler (clang on macOS, gcc or clang on Linux). The pins and
patches are the same on every platform; only the toolchain, the archive names and
the system libraries the manifest lists differ.

What is pinned, and why:

* freerdp      - FreeRDP; the C shim in crates/desktop is written against its 3.x
                 public API.
* libvncserver - LibVNCClient.
* zlib, OpenSSL - build dependencies of those two.

None of the four is stored in this repository. FreeRDP and LibVNCServer are
downloaded as a source tarball pinned by commit and verified by sha256; zlib and
OpenSSL are cloned by tag and the checked-out commit is verified. Every check runs
before anything is compiled, so a moved tag or a re-cut archive fails the build
instead of quietly changing what ships. The sources land in
<work>/src/<name>, never in vendor/: 3.3k upstream files in the repository make
routine git commands exceed the Windows command-line limit.

Fetching needs network access the first time. After a successful build the
verified tarball and the unpacked tree are reused, and an offline re-run works as
long as <work>/src still holds the trees the manifest points at.

Re-running skips a step only when its archives are installed *and* carry a pin
stamp naming the version, commit, tarball hash and options they were built from
(<dist>/.shellune-step-<name>). A missing or different stamp rebuilds, so a
changed pin cannot leave the prefix on the old version while the manifest claims
the new one. `--self-test` checks that comparison without a toolchain or network;
the first run against a prefix built by an older version of this script rebuilds
once, because those archives have no stamp yet.

Nothing is downloaded at run time and no installed shared library is used: the
shim links the static archives built here. Build prerequisites live in
vendor/README.md - on Windows Visual Studio Build Tools (MSVC, CMake, Ninja) and, for
OpenSSL, a Perl with its standard modules (portable Strawberry Perl works); on macOS
and Linux a C compiler, CMake, Ninja, make, Perl and git.

Usage:
    python packaging/native/build_native.py
    python packaging/native/build_native.py --perl C:/Strawberry/perl/bin/perl.exe
    python packaging/native/build_native.py --work target/native --dist target/native/dist
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]


def host_platform() -> str:
    """The platform this run builds for: windows, macos or linux."""
    if sys.platform == "win32":
        return "windows"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform.startswith("linux"):
        return "linux"
    return sys.platform


PLATFORM = host_platform()

# The POSIX platforms this script builds, with OpenSSL's Configure target for each.
# Anything else is refused instead of guessed at.
OPENSSL_TARGETS = {
    ("macos", "arm64"): "darwin64-arm64-cc",
    ("linux", "x86_64"): "linux-x86_64",
}
# The oldest macOS the archives run on. Rust's aarch64-apple-darwin default is 11.0;
# archives built for the runner's own (newer) version would make every link warn.
MACOS_DEPLOYMENT_TARGET = "11.0"

DEFAULT_BUILD_TOOLS = pathlib.Path(
    "C:/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools"
)

# Tarballs pinned by commit, not by tag: the URL names the commit directly, so a
# re-pointed tag cannot change what is downloaded, and the sha256 catches a re-cut
# archive. "required" is the few paths that must exist after unpacking; a tree that
# unpacks but lacks them is not the release we pinned.
FREERDP = {
    "name": "freerdp",
    "version": "3.33.1",
    "tag": "3.33.1",
    "commit": "65bcca1913a28cbdd7eb913599f692bd2b707991",
    "license": "Apache-2.0",
    "url": "https://codeload.github.com/FreeRDP/FreeRDP/tar.gz/"
           "65bcca1913a28cbdd7eb913599f692bd2b707991",
    "sha256": "97771fc4b967b4262bbe690f9b666402ad4c717717527c006b4acea8e21fffb8",
    "required": [
        "CMakeLists.txt",
        "include/freerdp/freerdp.h",
        "winpr/include/winpr/winpr.h",
    ],
}
FREERDP_MAC_PATCH = REPO_ROOT / "packaging/native/patches/freerdp-salted-mac-negotiation.patch"
FREERDP_CONNECTION_ORIGINAL = "9d197d202d558b782cf600981c3fb940296b43bf437d6ebba1970b063e5f2713"
FREERDP_CONNECTION_PATCHED = "ca4d99bb0482b6a2cf6f690086b3e3f0124949ab3607d9acc822ad24f37ed219"
FREERDP_DRIVE_PATCH = REPO_ROOT / "packaging/native/patches/freerdp-drive-confinement.patch"
FREERDP_RDPDR_PATCH = REPO_ROOT / "packaging/native/patches/freerdp-rdpdr-device-reply.patch"
# Every patch names each file it touches with the whole-file sha256 before and after
# (line endings normalized), so only the pinned source is patched and a repeat build
# of the exact patched tree is accepted. The order is the order of application.
FREERDP_PATCHES = [
    {
        "label": "MAC",
        "pin": "salted-mac-patch",
        "patch": FREERDP_MAC_PATCH,
        "files": {
            "libfreerdp/core/connection.c": (
                FREERDP_CONNECTION_ORIGINAL, FREERDP_CONNECTION_PATCHED,
            ),
        },
    },
    {
        "label": "drive confinement",
        "pin": "drive-confinement-patch",
        "patch": FREERDP_DRIVE_PATCH,
        "files": {
            "channels/drive/client/drive_file.c": (
                "c6495ccba9a513b6e5fee40ca7a93273f351632fa9f26c35ad3a875f07e8e4cf",
                "84b273803870b8341a92ac4d64839d24be56914016b7b731e215d535435d1ec6",
            ),
        },
    },
    {
        "label": "rdpdr device reply",
        "pin": "rdpdr-device-reply-patch",
        "patch": FREERDP_RDPDR_PATCH,
        "files": {
            "include/freerdp/client/rdpdr.h": (
                "f51500716fa5afa2d49605addb87c1cf8b4b60c512c73ed311b9d3607d06fd34",
                "4e42afc82abe8e3546567afc71595b49aec3298462d406a8be3d1303ba7f8cf3",
            ),
            "channels/rdpdr/client/rdpdr_main.c": (
                "6f726d13b8bcd8e789ff6046d27dda88a2e6944f04e322c68346f0652341e259",
                "27ca18954129ef0fadc645be1fd5f8101f3912f3b841f117ba39cfcb44aa7eb2",
            ),
        },
    },
]
LIBVNC = {
    "name": "libvncserver",
    "version": "0.9.15",
    "tag": "LibVNCServer-0.9.15",
    "commit": "9b54b1ec32731bd23158ca014dc18014db4194c3",
    "license": "GPL-2.0-or-later",
    "url": "https://codeload.github.com/LibVNC/libvncserver/tar.gz/"
           "9b54b1ec32731bd23158ca014dc18014db4194c3",
    "sha256": "662c311e0d4a8fc71bae6cf4759308948e4681c2e23987583ff2121cb1f1aedf",
    "required": [
        "CMakeLists.txt",
        "include/rfb/rfbclient.h",
        "src/libvncclient",
    ],
}
# Written into an unpacked tree so a later run can tell a verified checkout from a
# half-finished download without hashing the whole tree again.
SOURCE_STAMP = ".shellune-source-commit"
# Written into the prefix next to the archives, so a later run can tell which pin
# those archives were compiled from. Without it a changed pin plus an existing
# archive would write a manifest that claims the new version while the prefix still
# holds the old one.
STEP_STAMP = ".shellune-step-"

# Fetched by tag. The commit is verified after checkout.
EXTERNALS = {
    "zlib": {
        "url": "https://github.com/madler/zlib.git",
        "tag": "v1.3.1",
        "commit": "51b7f2abdade71cd9bb0e7a373ef2610ec6f9daf",
        "license": "Zlib",
    },
    "openssl": {
        "url": "https://github.com/openssl/openssl.git",
        "tag": "openssl-3.5.4",
        "commit": "c1eeb9406b6142148f267594197d853403d10208",
        "license": "Apache-2.0",
    },
}

# The exact options used for each library; they are also copied into the manifest
# so an installed prefix states how it was built.
FREERDP_OPTIONS = [
    "-DCMAKE_BUILD_TYPE=Release",
    "-DBUILD_SHARED_LIBS=OFF",
    # FreeRDP defaults a static build to the static CRT; Rust links the DLL CRT, and
    # two CRTs in one process means two heaps and cross-heap frees.
    "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL",
    "-DWITH_MONOLITHIC_BUILD=ON",
    # FreeRDP turns on whole-program optimisation by default; a /GL archive forces
    # /LTCG on every consumer, including a Rust link that does not ask for it.
    "-DCMAKE_INTERPROCEDURAL_OPTIMIZATION=OFF",
    "-DWITH_CLIENT=OFF",
    "-DWITH_SERVER=OFF",
    "-DWITH_X11=OFF",
    "-DWITH_WAYLAND=OFF",
    "-DWITH_FFMPEG=OFF",
    "-DWITH_SWSCALE=OFF",
    "-DWITH_CUPS=OFF",
    "-DWITH_PULSE=OFF",
    "-DWITH_ALSA=OFF",
    "-DWITH_OSS=OFF",
    "-DWITH_PCSC=OFF",
    "-DWITH_KRB5=OFF",
    "-DWITH_FUSE=OFF",
    "-DWITH_MANPAGES=OFF",
    "-DWITH_SAMPLE=OFF",
    "-DWITH_CLIENT_SDL=OFF",
    "-DWITH_URIPARSER=OFF",
    # USB redirection needs libusb and is not a feature this project offers.
    "-DCHANNEL_URBDRC=OFF",
    "-DCHANNEL_URBDRC_CLIENT=OFF",
    # MD4 and RC4 live in OpenSSL's legacy provider, which is a loadable module and
    # would mean shipping a DLL version of it. WinPR's own implementations keep the
    # CredSSP path working in a static build.
    "-DWITH_INTERNAL_MD4=ON",
    "-DWITH_INTERNAL_RC4=ON",
    "-DBUILD_TESTING=OFF",
]

LIBVNC_OPTIONS = [
    "-DCMAKE_BUILD_TYPE=Release",
    "-DBUILD_SHARED_LIBS=OFF",
    "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL",
    "-DWITH_ZLIB=ON",
    "-DWITH_JPEG=OFF",
    "-DWITH_PNG=OFF",
    "-DWITH_GCRYPT=OFF",
    "-DWITH_GNUTLS=OFF",
    # VNC TLS uses the same OpenSSL the RDP backend needs.
    "-DWITH_OPENSSL=ON",
    "-DWITH_SASL=OFF",
    "-DWITH_LZO=OFF",
    "-DWITH_THREADS=ON",
    "-DWITH_IPv6=ON",
    "-DWITH_WEBSOCKETS=OFF",
    "-DWITH_FFMPEG=OFF",
    "-DWITH_SDL=OFF",
    "-DWITH_GTK=OFF",
    "-DWITH_QT=OFF",
    "-DWITH_LIBSSHTUNNEL=OFF",
    "-DWITH_XCB=OFF",
    "-DWITH_SYSTEMD=OFF",
    "-DWITH_EXAMPLES=OFF",
    "-DWITH_TESTS=OFF",
    # libvncserver copies compile_commands.json into the source tree otherwise, and
    # the fetched tree should stay exactly what the pinned archive contains.
    "-DCMAKE_EXPORT_COMPILE_COMMANDS=OFF",
]

# The archive names the FreeRDP and LibVNCClient builds produce, and the ones the
# manifest tells build.rs to link. They carry the upstream version suffix.
# remdesk-common and rdpsnd-common are separate archives even in a monolithic build;
# the client channels reference their shared halves.
FREERDP_LIBRARIES = [
    "freerdp3",
    "winpr3",
    "freerdp-client3",
    "remdesk-common",
    "rdpsnd-common",
]
LIBVNC_LIBRARIES = ["vncclient"]
CRYPTO_LIBRARIES = ["libssl", "libcrypto"]
# zlibstatic, not zlib: the latter is the import library for a DLL we do not ship.
ZLIB_LIBRARY = "zlibstatic"

# The same archives on macOS and Linux: OpenSSL keeps its plain names there, and
# zlib's static archive is libz.a.
POSIX_CRYPTO_LIBRARIES = ["ssl", "crypto"]
POSIX_ZLIB_LIBRARY = "z"

# Options added on macOS and Linux. Each one keeps the build from picking up a
# library that happens to be installed on the build machine, so the archives link
# the same set of system libraries everywhere; the Windows build finds none of
# these either. Position independent code because Rust links PIE executables.
POSIX_COMMON_OPTIONS = ["-DCMAKE_POSITION_INDEPENDENT_CODE=ON"]
MACOS_COMMON_OPTIONS = [
    "-DCMAKE_OSX_ARCHITECTURES=arm64",
    "-DCMAKE_OSX_DEPLOYMENT_TARGET=" + MACOS_DEPLOYMENT_TARGET,
]
POSIX_FREERDP_OPTIONS = [
    "-DOPENSSL_USE_STATIC_LIBS=ON",
    # No JSON, ICU, systemd, image or codec library: each would be a shared library
    # the application then needs at run time.
    "-DWITH_JSON_DISABLED=ON",
    "-DWITH_AAD=OFF",
    "-DWITH_UNICODE_BUILTIN=ON",
    "-DWITH_TIMEZONE_ICU=OFF",
    "-DWITH_SYSTEMD=OFF",
    "-DWITH_CAIRO=OFF",
    "-DWITH_JPEG=OFF",
    "-DWITH_WEBP=OFF",
    "-DWITH_OPUS=OFF",
    "-DWITH_FDK_AAC=OFF",
    "-DWITH_OPENH264=OFF",
    "-DWITH_SNDIO=OFF",
    "-DWITH_PKCS11=OFF",
    "-DWITH_SMARTCARD_PCSC=OFF",
]
MACOS_FREERDP_OPTIONS = [
    "-DWITH_APPLE_VIDEOTOOLBOX=OFF",
    # The CoreAudio/AVFoundation sound and microphone backends, and the camera one:
    # the shim never loads an audio or camera channel, and these would tie the link
    # to AVFoundation and AudioToolbox.
    "-DWITH_MACAUDIO=OFF",
    "-DCHANNEL_RDPECAM_CLIENT=OFF",
]
POSIX_LIBVNC_OPTIONS = [
    "-DOPENSSL_USE_STATIC_LIBS=ON",
]
# OpenSSL's Configure arguments on macOS and Linux, after the target name.
OPENSSL_POSIX_FLAGS = [
    "no-asm", "no-shared", "no-module", "no-tests", "no-docs", "no-apps", "-fPIC",
]

# What the static archives need from the system on macOS and Linux.
LINUX_SYSTEM_LIBRARIES = ["pthread", "dl", "m", "rt"]
MACOS_SYSTEM_LIBRARIES: list[str] = []
MACOS_FRAMEWORKS = ["CoreFoundation", "Carbon", "IOKit", "Security"]

# System libraries the static archives need on Windows.
WINDOWS_SYSTEM_LIBRARIES = [
    "ws2_32",
    "crypt32",
    "secur32",
    "bcrypt",
    "ncrypt",
    "userenv",
    "advapi32",
    "user32",
    "gdi32",
    "ole32",
    "oleaut32",
    "comdlg32",
    "shell32",
    "shlwapi",
    "pathcch",
    "iphlpapi",
    "dnsapi",
    "rpcrt4",
    "ntdsapi",
    "dbghelp",
    "uuid",
    "winspool",
    "winmm",
    "wtsapi32",
    "credui",
]


def log(message: str) -> None:
    print("[native] " + message, flush=True)


def archive_file(name: str, target: str | None = None) -> str:
    """The file name of static archive `name`: foo.lib on Windows, libfoo.a elsewhere."""
    if (target or PLATFORM) == "windows":
        return name + ".lib"
    return "lib" + name + ".a"


def common_options(target: str) -> list[str]:
    if target == "windows":
        return []
    if target == "macos":
        return [*POSIX_COMMON_OPTIONS, *MACOS_COMMON_OPTIONS]
    return list(POSIX_COMMON_OPTIONS)


def freerdp_options(target: str) -> list[str]:
    """FreeRDP's options on `target`. Windows uses FREERDP_OPTIONS unchanged."""
    if target == "windows":
        return list(FREERDP_OPTIONS)
    options = [option for option in FREERDP_OPTIONS if "MSVC" not in option]
    options.extend(common_options(target))
    options.extend(POSIX_FREERDP_OPTIONS)
    if target == "macos":
        options.extend(MACOS_FREERDP_OPTIONS)
    return options


def libvnc_options(target: str) -> list[str]:
    """LibVNCServer's options on `target`. Windows uses LIBVNC_OPTIONS unchanged."""
    if target == "windows":
        return list(LIBVNC_OPTIONS)
    options = [option for option in LIBVNC_OPTIONS if "MSVC" not in option]
    options.extend(common_options(target))
    options.extend(POSIX_LIBVNC_OPTIONS)
    return options


def zlib_options(target: str) -> tuple[str, ...]:
    return (*ZLIB_OPTIONS, *common_options(target))


def openssl_options(target: str, machine: str | None = None) -> tuple[str, ...]:
    """OpenSSL's Configure line on `target`; it is also the step's pin."""
    if target == "windows":
        return OPENSSL_OPTIONS
    configure = OPENSSL_TARGETS.get((target, machine or platform.machine()), "unsupported")
    options = ("Configure " + " ".join([configure, *OPENSSL_POSIX_FLAGS]),)
    if target == "macos":
        options += ("MACOSX_DEPLOYMENT_TARGET=" + MACOS_DEPLOYMENT_TARGET,)
    return options


def crypto_libraries(target: str) -> list[str]:
    return list(CRYPTO_LIBRARIES) if target == "windows" else list(POSIX_CRYPTO_LIBRARIES)


def zlib_library(target: str) -> str:
    return ZLIB_LIBRARY if target == "windows" else POSIX_ZLIB_LIBRARY


def system_libraries(target: str) -> list[str]:
    if target == "windows":
        return list(WINDOWS_SYSTEM_LIBRARIES)
    if target == "macos":
        return list(MACOS_SYSTEM_LIBRARIES)
    return list(LINUX_SYSTEM_LIBRARIES)


def frameworks(target: str) -> list[str]:
    return list(MACOS_FRAMEWORKS) if target == "macos" else []


def step_pin(name: str, spec: dict, options: list[str] | tuple[str, ...] = ()) -> str:
    """The identity of one build step: what it compiles and how."""
    parts = [name, str(spec.get("version", spec.get("tag", ""))), str(spec.get("commit", "")),
             str(spec.get("sha256", ""))]
    parts.extend(options)
    return "|".join(parts)


def step_stamp_path(dist: pathlib.Path, name: str) -> pathlib.Path:
    return dist / (STEP_STAMP + name)


def already_built(dist: pathlib.Path, names: list[str], force: bool,
                  step: str | None = None, pin: str | None = None) -> bool:
    """Skips a step whose archives are already in the prefix, built from this pin.

    The script is meant to be re-runnable: after a failure in a later step, or after
    adding an option, nobody should have to rebuild OpenSSL again from scratch.

    An archive on disk is not enough. The step stamp records the version, commit,
    tarball hash and options the archives were produced from, and a run with a
    different pin (or with no stamp at all) rebuilds. Otherwise a changed pin would
    leave the old archives in place while write_manifest() declared the new ones.
    """
    if force:
        return False
    if not all((dist / "lib" / archive_file(name)).is_file() for name in names):
        return False
    if step is None or pin is None:
        return True
    stamp = step_stamp_path(dist, step)
    if not stamp.is_file():
        log(step + ": installed archives carry no pin stamp; rebuilding them")
        return False
    recorded = stamp.read_text(encoding="utf-8").strip()
    if recorded != pin:
        log(step + ": installed archives were built from another pin; rebuilding them")
        return False
    return True


def record_built(dist: pathlib.Path, step: str, pin: str) -> None:
    """Records the pin the archives in `dist` were built from."""
    step_stamp_path(dist, step).write_text(pin + "\n", encoding="utf-8")


def run(command: list[str], cwd: pathlib.Path | None = None) -> None:
    log(" ".join(str(part) for part in command))
    subprocess.run(command, cwd=cwd, check=True)


def run_msvc(build_tools: pathlib.Path, work: pathlib.Path, name: str, lines: list[str]) -> None:
    """Runs shell lines inside the MSVC environment from vcvars64.bat."""
    vcvars = build_tools / "VC/Auxiliary/Build/vcvars64.bat"
    wrapper = work / ("step-" + name + ".cmd")
    body = ["@echo off", 'call "' + vcvars.as_posix() + '" >nul', "if errorlevel 1 exit /b 2"]
    for line in lines:
        body.extend([line, "if errorlevel 1 exit /b %errorlevel%"])
    body.append("exit /b %errorlevel%")
    wrapper.write_text("\r\n".join(body) + "\r\n", encoding="utf-8")
    log("step " + name)
    subprocess.run(["cmd", "/c", str(wrapper)], check=True)


def quoted(path: pathlib.Path) -> str:
    return '"' + path.as_posix() + '"'


def run_logged(command: list[str], log_path: pathlib.Path, cwd: pathlib.Path | None = None) -> None:
    """Runs a noisy command with its output appended to `log_path`.

    A failure prints the end of the log before it stops the build, so a CI log says
    why without holding every line of a successful OpenSSL build.
    """
    log(" ".join(str(part) for part in command) + " >> " + str(log_path))
    with log_path.open("a", encoding="utf-8") as handle:
        result = subprocess.run(command, cwd=cwd, stdout=handle, stderr=subprocess.STDOUT)
    if result.returncode != 0:
        lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
        for line in lines[-60:]:
            print(line, flush=True)
        raise subprocess.CalledProcessError(result.returncode, command)


def jobs() -> str:
    return str(os.cpu_count() or 2)


def fetch_external(work: pathlib.Path, name: str, spec: dict) -> pathlib.Path:
    """Clones the pinned tag and refuses to continue on a different commit."""
    target = work / "src" / name
    if not (target / ".git").exists():
        target.parent.mkdir(parents=True, exist_ok=True)
        log("fetching " + name + " " + spec["tag"])
        run([
            "git", "clone", "--depth", "1", "--branch", spec["tag"],
            "--single-branch", spec["url"], str(target),
        ])
    head = subprocess.run(
        ["git", "-C", str(target), "rev-parse", "HEAD"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    if head != spec["commit"]:
        raise SystemExit(
            name + ": checked out " + head + ", expected " + spec["commit"]
            + " for tag " + spec["tag"] + ". Update the pin deliberately."
        )
    return target


def sha256_of(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download(url: str, destination: pathlib.Path, name: str) -> None:
    """Fetches a source tarball; a network failure is reported, never skipped."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    partial = destination.with_name(destination.name + ".part")
    log("downloading " + name + " from " + url)
    try:
        with urllib.request.urlopen(url, timeout=120) as response:
            with partial.open("wb") as handle:
                shutil.copyfileobj(response, handle, 1024 * 256)
    except (urllib.error.URLError, http.client.HTTPException, OSError) as error:
        partial.unlink(missing_ok=True)
        raise SystemExit(
            name + ": network error while downloading " + url + ": " + str(error)
            + ". Fetching this dependency needs network access; the source tree is "
            "not stored in this repository."
        ) from error
    partial.replace(destination)


def verify_sha256(archive: pathlib.Path, spec: dict) -> None:
    actual = sha256_of(archive)
    if actual != spec["sha256"]:
        raise SystemExit(
            spec["name"] + ": sha256 mismatch for " + archive.name + "\n"
            + "  expected " + spec["sha256"] + "\n"
            + "  actual   " + actual + "\n"
            + "Upstream re-cut the archive or the download was corrupted. Nothing was "
            "built. Delete the file to fetch it again, and change the pin in this "
            "script only deliberately."
        )


def unpack(archive: pathlib.Path, spec: dict, work: pathlib.Path) -> pathlib.Path:
    """Unpacks into <work>/src/<name> through a staging directory.

    The commit is written into the tree as SOURCE_STAMP only after the archive and
    the expected files check out, so an interrupted run cannot leave something that
    a later run mistakes for a verified checkout.
    """
    target = work / "src" / spec["name"]
    staging = work / "src" / (spec["name"] + ".unpacking")
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir(parents=True)
    log("unpacking " + archive.name)
    try:
        with tarfile.open(archive, "r:gz") as bundle:
            try:
                bundle.extractall(staging, filter="data")
            except TypeError:  # filter= arrived in Python 3.12.
                bundle.extractall(staging)
    except (tarfile.TarError, OSError) as error:
        shutil.rmtree(staging, ignore_errors=True)
        raise SystemExit(
            spec["name"] + ": cannot unpack " + str(archive) + ": " + str(error)
            + ". The downloaded archive is not usable; delete it and fetch it again."
        ) from error
    roots = [entry for entry in staging.iterdir() if entry.name != SOURCE_STAMP]
    if len(roots) != 1 or not roots[0].is_dir():
        shutil.rmtree(staging, ignore_errors=True)
        raise SystemExit(
            spec["name"] + ": unexpected layout in " + archive.name + ", expected one "
            "top-level directory with the sources."
        )
    missing = [name for name in spec["required"] if not (roots[0] / name).exists()]
    if missing:
        shutil.rmtree(staging, ignore_errors=True)
        raise SystemExit(
            spec["name"] + ": " + archive.name + " unpacked but is missing "
            + ", ".join(missing) + ", so it is not " + spec["version"] + "."
        )
    if target.exists():
        shutil.rmtree(target)
    roots[0].rename(target)
    shutil.rmtree(staging, ignore_errors=True)
    (target / SOURCE_STAMP).write_text(spec["commit"] + "\n", encoding="utf-8")
    log(spec["name"] + " " + spec["version"] + " unpacked into " + str(target))
    return target


def ensure_source(work: pathlib.Path, spec: dict) -> pathlib.Path:
    """Returns a verified source tree, fetching it once.

    Reuses an unpacked tree only when its stamp names this commit; reuses a cached
    tarball only when its sha256 still matches. Anything else is re-fetched, so a
    stale or tampered tree cannot be compiled by accident.
    """
    target = work / "src" / spec["name"]
    stamp = target / SOURCE_STAMP
    if stamp.is_file() and stamp.read_text(encoding="utf-8").strip() == spec["commit"]:
        log(spec["name"] + " " + spec["version"] + " sources already unpacked")
        return target
    if target.exists():
        log(spec["name"] + ": replacing the source tree, it is not " + spec["commit"])
        shutil.rmtree(target)
    archive = work / "downloads" / (spec["name"] + "-" + spec["version"] + ".tar.gz")
    if archive.is_file() and sha256_of(archive) == spec["sha256"]:
        log(spec["name"] + ": using cached " + archive.name)
    else:
        if archive.exists():
            log(spec["name"] + ": cached tarball does not match the pinned sha256")
            archive.unlink()
        download(spec["url"], archive, spec["name"])
        verify_sha256(archive, spec)
    return unpack(archive, spec, work)


ZLIB_OPTIONS = ("-DCMAKE_BUILD_TYPE=Release", "-DZLIB_BUILD_EXAMPLES=OFF",
                "-DBUILD_SHARED_LIBS=OFF")
# VNC password authentication needs DES from the legacy provider. no-module puts
# that provider in libcrypto, so a portable build needs no external legacy.dll.
OPENSSL_OPTIONS = ("Configure VC-WIN64A no-asm no-shared no-module no-tests no-docs no-apps",
                   'CFLAGS="/MD -O2 /Z7"')


def build_zlib(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
               force: bool) -> None:
    pin = step_pin("zlib", EXTERNALS["zlib"], ZLIB_OPTIONS)
    if already_built(dist, ["zlibstatic"], force, "zlib", pin):
        log("zlib already installed")
        return
    source = fetch_external(work, "zlib", EXTERNALS["zlib"])
    build = work / "build" / "zlib"
    run_msvc(build_tools, work, "zlib", [
        " ".join([
            "cmake -S", quoted(source), "-B", quoted(build), "-G Ninja",
            "-DCMAKE_BUILD_TYPE=Release",
            "-DCMAKE_INSTALL_PREFIX=" + quoted(dist),
            "-DZLIB_BUILD_EXAMPLES=OFF",
            "-DBUILD_SHARED_LIBS=OFF",
        ]),
        "cmake --build " + quoted(build) + " --target install",
    ])
    record_built(dist, "zlib", pin)


def build_zlib_posix(work: pathlib.Path, dist: pathlib.Path, force: bool) -> None:
    options = zlib_options(PLATFORM)
    pin = step_pin("zlib", EXTERNALS["zlib"], options)
    if already_built(dist, [zlib_library(PLATFORM)], force, "zlib", pin):
        log("zlib already installed")
        return
    source = fetch_external(work, "zlib", EXTERNALS["zlib"])
    build = work / "build" / "zlib"
    run(["cmake", "-S", str(source), "-B", str(build), "-G", "Ninja", *options,
         "-DCMAKE_INSTALL_PREFIX=" + str(dist), "-DINSTALL_LIB_DIR=" + str(dist / "lib")])
    run(["cmake", "--build", str(build), "--target", "install"])
    # zlib 1.3.1 always installs its shared library too. Nothing may link it: the
    # prefix is a link search path, and a -lz from any crate would otherwise find it
    # and tie the binary to this build directory.
    for shared in [*(dist / "lib").glob("libz.so*"), *(dist / "lib").glob("libz*.dylib")]:
        shared.unlink()
    record_built(dist, "zlib", pin)


def build_openssl_posix(work: pathlib.Path, dist: pathlib.Path, perl: str, force: bool) -> None:
    options = openssl_options(PLATFORM)
    pin = step_pin("openssl", EXTERNALS["openssl"], options)
    if already_built(dist, crypto_libraries(PLATFORM), force, "openssl", pin):
        log("openssl already installed")
        return
    configure = OPENSSL_TARGETS[(PLATFORM, platform.machine())]
    source = fetch_external(work, "openssl", EXTERNALS["openssl"])
    log_path = work / "openssl-build.log"
    log_path.write_text("", encoding="utf-8")
    run_logged([
        perl, "Configure", configure, *OPENSSL_POSIX_FLAGS,
        "--prefix=" + str(dist), "--openssldir=" + str(dist / "ssl"), "--libdir=lib",
    ], log_path, cwd=source)
    run_logged(["make", "-j" + jobs(), "build_libs"], log_path, cwd=source)
    run_logged(["make", "install_dev"], log_path, cwd=source)
    record_built(dist, "openssl", pin)


def build_cmake_posix(work: pathlib.Path, dist: pathlib.Path, name: str,
                      source: pathlib.Path, options: list[str]) -> pathlib.Path:
    """Configures and builds one CMake project against the prefix, without installing."""
    build = work / "build" / name
    run([
        "cmake", "-S", str(source), "-B", str(build), "-G", "Ninja",
        "-DCMAKE_PREFIX_PATH=" + str(dist),
        "-DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF",
        "-DZLIB_LIBRARY=" + str(dist / "lib" / archive_file(zlib_library(PLATFORM))),
        "-DZLIB_INCLUDE_DIR=" + str(dist / "include"),
        "-DOPENSSL_ROOT_DIR=" + str(dist),
        *options,
    ])
    run(["cmake", "--build", str(build), "--parallel", jobs()])
    return build


def build_freerdp_posix(work: pathlib.Path, dist: pathlib.Path, source: pathlib.Path,
                        force: bool) -> pathlib.Path:
    apply_freerdp_patches(source)
    options = freerdp_options(PLATFORM)
    pin = step_pin("freerdp", FREERDP, [*options, freerdp_patch_pin()])
    if already_built(dist, FREERDP_LIBRARIES, force, "freerdp", pin):
        log("freerdp already installed")
        return work / "build" / "freerdp"
    build = build_cmake_posix(work, dist, "freerdp", source, options)
    collect_archives(build, dist, FREERDP_LIBRARIES)
    record_built(dist, "freerdp", pin)
    return build


def build_libvnc_posix(work: pathlib.Path, dist: pathlib.Path, source: pathlib.Path,
                       force: bool) -> pathlib.Path:
    options = libvnc_options(PLATFORM)
    pin = step_pin("libvncserver", LIBVNC, options)
    if already_built(dist, LIBVNC_LIBRARIES, force, "libvncserver", pin):
        log("libvncserver already installed")
        return work / "build" / "libvncserver"
    build = build_cmake_posix(work, dist, "libvncserver", source, options)
    collect_archives(build, dist, LIBVNC_LIBRARIES)
    record_built(dist, "libvncserver", pin)
    return build


def build_openssl(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
                  perl: str, force: bool) -> None:
    pin = step_pin("openssl", EXTERNALS["openssl"], OPENSSL_OPTIONS)
    if already_built(dist, ["libcrypto", "libssl"], force, "openssl", pin):
        log("openssl already installed")
        return
    source = fetch_external(work, "openssl", EXTERNALS["openssl"])
    log_path = work / "openssl-build.log"
    # /MD matches the MSVC runtime Rust links against; a /MT libcrypto would give
    # the process two heaps and mix allocations across the boundary.
    run_msvc(build_tools, work, "openssl", [
        "cd /d " + quoted(source),
        " ".join([
            quoted(pathlib.Path(perl)), "Configure VC-WIN64A no-asm no-shared no-module",
            "no-tests no-docs no-apps", 'CFLAGS="/MD -O2 /Z7"',
            "--prefix=" + quoted(dist),
            "--openssldir=" + quoted(dist / "ssl"),
            ">", quoted(log_path),
        ]),
        "nmake /nologo build_libs >> " + quoted(log_path),
        # install_dev copies ossl_static.pdb unconditionally, but /Z7 (above) keeps the
        # debug information in the objects, and newer MSVC releases (19.5x) then write
        # no .pdb at all. An empty stand-in lets the install finish; the archives are
        # the same either way, so the step's pin does not change.
        "if not exist ossl_static.pdb copy /y nul ossl_static.pdb >nul",
        "nmake /nologo install_dev >> " + quoted(log_path),
    ])
    record_built(dist, "openssl", pin)


def build_freerdp(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
                  source: pathlib.Path, force: bool) -> pathlib.Path | None:
    build = work / "build" / "freerdp"
    apply_freerdp_patches(source)
    pin = step_pin("freerdp", FREERDP, [*FREERDP_OPTIONS, freerdp_patch_pin()])
    if already_built(dist, FREERDP_LIBRARIES, force, "freerdp", pin):
        log("freerdp already installed")
        return build
    options = [
        "-DCMAKE_PREFIX_PATH=" + quoted(dist),
        "-DZLIB_LIBRARY=" + quoted(dist / "lib/zlibstatic.lib"),
        "-DZLIB_INCLUDE_DIR=" + quoted(dist / "include"),
        *FREERDP_OPTIONS,
    ]
    run_msvc(build_tools, work, "freerdp", [
        " ".join(["cmake -S", quoted(source), "-B", quoted(build), "-G Ninja", *options]),
        "cmake --build " + quoted(build),
    ])
    record_built(dist, "freerdp", pin)
    return build


def freerdp_patch_pin() -> str:
    """Names every patch by its sha256, so a changed patch rebuilds FreeRDP."""
    return ",".join(spec["pin"] + "=" + sha256_of(spec["patch"]) for spec in FREERDP_PATCHES)


def source_digest(path: pathlib.Path) -> str:
    # Normalize checkout line endings, without accepting other source edits.
    return hashlib.sha256(path.read_text(encoding="utf-8").encode()).hexdigest()


def apply_freerdp_patch(source: pathlib.Path, spec: dict) -> None:
    """Only patch the pinned source, and accept repeat builds of that exact result."""
    label = spec["label"]
    files = spec["files"]
    current = {name: source_digest(source / name) for name in files}
    if all(current[name] == patched for name, (_, patched) in files.items()):
        return
    for name, (original, _) in files.items():
        if current[name] != original:
            raise SystemExit(
                f"FreeRDP {label} patch: unexpected {pathlib.PurePosixPath(name).name}; "
                "refusing to patch"
            )
    run(["git", "apply", "--unsafe-paths", "--directory=" + source.resolve().as_posix(),
         str(spec["patch"])], cwd=REPO_ROOT)
    for name, (_, patched) in files.items():
        if source_digest(source / name) != patched:
            raise SystemExit(f"FreeRDP {label} patch: patched source checksum mismatch")


def apply_freerdp_mac_patch(source: pathlib.Path) -> None:
    apply_freerdp_patch(source, FREERDP_PATCHES[0])


def apply_freerdp_patches(source: pathlib.Path) -> None:
    for spec in FREERDP_PATCHES:
        apply_freerdp_patch(source, spec)


def build_libvnc(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
                 source: pathlib.Path, force: bool) -> pathlib.Path | None:
    build = work / "build" / "libvncserver"
    pin = step_pin("libvncserver", LIBVNC, LIBVNC_OPTIONS)
    if already_built(dist, LIBVNC_LIBRARIES, force, "libvncserver", pin):
        log("libvncserver already installed")
        return build
    options = [
        "-DCMAKE_PREFIX_PATH=" + quoted(dist),
        "-DZLIB_LIBRARY=" + quoted(dist / "lib/zlibstatic.lib"),
        "-DZLIB_INCLUDE_DIR=" + quoted(dist / "include"),
        *LIBVNC_OPTIONS,
    ]
    run_msvc(build_tools, work, "libvncserver", [
        " ".join(["cmake -S", quoted(source), "-B", quoted(build), "-G Ninja", *options]),
        "cmake --build " + quoted(build),
    ])
    record_built(dist, "libvncserver", pin)
    return build


def collect_archives(build: pathlib.Path, dist: pathlib.Path, names: list[str]) -> list[str]:
    """Copies the static archives a build produced into the prefix."""
    lib_dir = dist / "lib"
    lib_dir.mkdir(parents=True, exist_ok=True)
    found: list[str] = []
    if PLATFORM != "windows":
        wanted = {archive_file(name): name for name in names}
        for archive in build.rglob("lib*.a"):
            if archive.name in wanted:
                shutil.copy2(archive, lib_dir / archive.name)
                found.append(wanted[archive.name])
        return found
    wanted = {name.lower() for name in names}
    for archive in build.rglob("*.lib"):
        if archive.stem.lower() in wanted:
            shutil.copy2(archive, lib_dir / archive.name)
            found.append(archive.stem)
    return found


# The license text each linked library ships, copied to <dist>/licenses so a package
# can carry it next to Shellune's own LICENSE: (file in the prefix, source tree,
# file in that tree).
THIRD_PARTY_LICENSES = [
    ("FreeRDP-LICENSE.txt", "freerdp", "LICENSE"),
    ("LibVNCServer-COPYING.txt", "libvncserver", "COPYING"),
    ("OpenSSL-LICENSE.txt", "openssl", "LICENSE.txt"),
    ("zlib-LICENSE.txt", "zlib", "LICENSE"),
]
LICENSES_DIR = "licenses"


def write_licenses(work: pathlib.Path, dist: pathlib.Path) -> None:
    """Copies the license of every library the manifest links into <dist>/licenses.

    A missing text stops the run: a prefix whose package could not state the
    licenses of what it links must not look finished.
    """
    target = dist / LICENSES_DIR
    target.mkdir(parents=True, exist_ok=True)
    for name, tree, relative in THIRD_PARTY_LICENSES:
        source = work / "src" / tree / relative
        if not source.is_file():
            raise SystemExit("license text missing: " + str(source))
        shutil.copyfile(source, target / name)
    notice = [
        "Shellune links these libraries statically for its RDP and VNC sessions.",
        "",
        "FreeRDP " + FREERDP["version"] + " (" + FREERDP["license"] + "): FreeRDP-LICENSE.txt",
        "LibVNCServer " + LIBVNC["version"] + " (" + LIBVNC["license"]
        + "): LibVNCServer-COPYING.txt",
        "OpenSSL " + EXTERNALS["openssl"]["tag"] + " (" + EXTERNALS["openssl"]["license"]
        + "): OpenSSL-LICENSE.txt",
        "zlib " + EXTERNALS["zlib"]["tag"] + " (" + EXTERNALS["zlib"]["license"]
        + "): zlib-LICENSE.txt",
        "",
        "Sources: the pinned upstream commits listed in vendor/README.md of the Shellune",
        "source tree; FreeRDP carries the patches in packaging/native/patches.",
    ]
    (target / "README.txt").write_text("\n".join(notice) + "\n", encoding="utf-8")
    log("wrote the third-party licenses to " + str(target))


def prune(work: pathlib.Path) -> None:
    """Deletes what a finished prefix no longer needs, before a CI cache saves it.

    Object files and the verified tarballs go. The source trees (patched FreeRDP is
    checked again on a re-run), the generated headers, the archives in the build trees
    and the prefix stay, so a re-run with the same pins skips every step.
    """
    removed = 0
    for path in work.rglob("*"):
        if path.suffix in (".o", ".obj") and path.is_file():
            path.unlink()
            removed += 1
    shutil.rmtree(work / "downloads", ignore_errors=True)
    log("pruned " + str(removed) + " object files and the downloaded tarballs")


def write_manifest(dist: pathlib.Path, include_dirs: list[pathlib.Path],
                   libraries: list[str]) -> None:
    lines = [
        "# Generated by packaging/native/build_native.py. Do not edit by hand.",
        "# freerdp=" + FREERDP["version"] + " " + FREERDP["commit"]
        + " (" + FREERDP["license"] + ")",
        "# freerdp tarball sha256=" + FREERDP["sha256"],
        "# freerdp " + freerdp_patch_pin(),
        "# libvncserver=" + LIBVNC["version"] + " " + LIBVNC["commit"]
        + " (" + LIBVNC["license"] + ")",
        "# libvncserver tarball sha256=" + LIBVNC["sha256"],
        "# zlib=" + EXTERNALS["zlib"]["tag"] + " " + EXTERNALS["zlib"]["commit"]
        + " (" + EXTERNALS["zlib"]["license"] + ")",
        "# openssl=" + EXTERNALS["openssl"]["tag"] + " " + EXTERNALS["openssl"]["commit"]
        + " (" + EXTERNALS["openssl"]["license"] + ")",
    ]
    lines.extend("# freerdp option " + option for option in freerdp_options(PLATFORM))
    lines.extend("# libvncserver option " + option for option in libvnc_options(PLATFORM))
    # build.rs refuses a prefix built for another platform than the one it compiles.
    lines.append("platform=" + PLATFORM)
    lines.extend("include=" + str(directory) for directory in include_dirs)
    lines.append("lib_dir=" + str(dist / "lib"))
    lines.extend("lib=" + library for library in libraries)
    lines.extend("syslib=" + library for library in system_libraries(PLATFORM))
    lines.extend("framework=" + name for name in frameworks(PLATFORM))
    lines.append("protocol=rdp")
    lines.append("protocol=vnc")
    target = dist / "shellune-native.txt"
    target.write_text("\n".join(lines) + "\n", encoding="utf-8")
    log("wrote " + str(target))


def self_test() -> int:
    """Checks the pin comparison without a toolchain, network or prefix.

    Run it with `python packaging/native/build_native.py --self-test`. It builds a
    throwaway prefix and asserts the three cases that matter: no stamp means
    rebuild, a matching stamp skips, a changed pin rebuilds even though the archive
    is still on disk.
    """
    work = pathlib.Path(tempfile.mkdtemp(prefix="shellune-native-selftest-"))
    try:
        dist = work / "dist"
        (dist / "lib").mkdir(parents=True)
        (dist / "lib" / archive_file("zlibstatic")).write_bytes(b"archive")
        pin = step_pin("zlib", EXTERNALS["zlib"], ZLIB_OPTIONS)
        if already_built(dist, ["zlibstatic"], False, "zlib", pin):
            raise SystemExit("self-test: an archive without a stamp must not count as built")
        record_built(dist, "zlib", pin)
        if not already_built(dist, ["zlibstatic"], False, "zlib", pin):
            raise SystemExit("self-test: a matching stamp must be reused")
        if already_built(dist, ["zlibstatic"], True, "zlib", pin):
            raise SystemExit("self-test: --force must rebuild")
        moved = step_pin("zlib", {
            **EXTERNALS["zlib"], "commit": "0" * 40
        }, ZLIB_OPTIONS)
        if already_built(dist, ["zlibstatic"], False, "zlib", moved):
            raise SystemExit(
                "self-test: a changed pin must rebuild even though the archive exists"
            )
        if sys.platform == "win32":
            vcvars = work / "VC/Auxiliary/Build/vcvars64.bat"
            vcvars.parent.mkdir(parents=True)
            vcvars.write_text("@exit /b 0\n")
            marker = work / "must-not-exist"
            try:
                run_msvc(work, work, "fail-fast", [
                    "cmd /c exit 7", "echo stale-build > " + quoted(marker),
                ])
            except subprocess.CalledProcessError as error:
                assert error.returncode == 7
            else:
                raise SystemExit("self-test: a failed native command must stop the build")
            assert not marker.exists(), "a later command ran after failure"
        self_test_platforms()
        log("self-test passed: pin stamping decides skip versus rebuild as documented")
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


def self_test_platforms() -> None:
    """The Windows build keeps its options, names and pins; the others differ only
    where they must."""
    if freerdp_options("windows") != FREERDP_OPTIONS:
        raise SystemExit("self-test: the Windows FreeRDP options changed")
    if libvnc_options("windows") != LIBVNC_OPTIONS:
        raise SystemExit("self-test: the Windows LibVNCServer options changed")
    if zlib_options("windows") != ZLIB_OPTIONS or openssl_options("windows") != OPENSSL_OPTIONS:
        raise SystemExit("self-test: the Windows zlib or OpenSSL options changed")
    if (crypto_libraries("windows") != CRYPTO_LIBRARIES
            or zlib_library("windows") != ZLIB_LIBRARY
            or system_libraries("windows") != WINDOWS_SYSTEM_LIBRARIES
            or frameworks("windows")):
        raise SystemExit("self-test: the Windows link line changed")
    if archive_file("freerdp3", "windows") != "freerdp3.lib":
        raise SystemExit("self-test: Windows archives are <name>.lib")
    for target, machine in OPENSSL_TARGETS:
        if archive_file("freerdp3", target) != "libfreerdp3.a":
            raise SystemExit("self-test: " + target + " archives are lib<name>.a")
        for options in (freerdp_options(target), libvnc_options(target), zlib_options(target)):
            if any("MSVC" in option for option in options):
                raise SystemExit("self-test: an MSVC option reached " + target)
            if "-DCMAKE_POSITION_INDEPENDENT_CODE=ON" not in options:
                raise SystemExit("self-test: " + target + " archives must be PIC for Rust")
        configure = openssl_options(target, machine)[0]
        if OPENSSL_TARGETS[(target, machine)] not in configure or "no-shared" not in configure:
            raise SystemExit("self-test: " + target + " OpenSSL Configure line is wrong")
    if "-DCMAKE_OSX_ARCHITECTURES=arm64" not in freerdp_options("macos"):
        raise SystemExit("self-test: macOS builds arm64")
    if not frameworks("macos") or frameworks("linux"):
        raise SystemExit("self-test: frameworks are a macOS link item only")


def main() -> int:
    parser = argparse.ArgumentParser(description="Build the pinned native desktop backends.")
    parser.add_argument("--work", default=str(REPO_ROOT / "target/native"),
                        help="build tree and fetched sources")
    parser.add_argument("--dist", default=None, help="install prefix (default: <work>/dist)")
    parser.add_argument("--build-tools", default=str(DEFAULT_BUILD_TOOLS),
                        help="Visual Studio Build Tools root")
    parser.add_argument("--perl", default=shutil.which("perl") or "perl",
                        help="Perl with its standard modules, needed by OpenSSL")
    parser.add_argument("--force", action="store_true",
                        help="rebuild steps whose archives are already installed")
    parser.add_argument("--prune", action="store_true",
                        help="after a successful build, delete object files and tarballs "
                             "(for a CI cache); a re-run still skips every step")
    parser.add_argument("--self-test", action="store_true",
                        help="check the pin comparison logic and exit, no build")
    arguments = parser.parse_args()

    if arguments.self_test:
        return self_test()

    # CMake 4 refuses projects that declare a minimum below 3.5, which LibVNCServer
    # 0.9.15 does. The environment form of the switch is used, not a -D option, so the
    # step pins stay as they are; CMake 3 ignores it, and a project that already asks
    # for 3.5 or later is not affected.
    os.environ.setdefault("CMAKE_POLICY_VERSION_MINIMUM", "3.5")

    if PLATFORM != "windows":
        return main_posix(arguments)

    work = pathlib.Path(arguments.work).resolve()
    dist = pathlib.Path(arguments.dist).resolve() if arguments.dist else work / "dist"
    build_tools = pathlib.Path(arguments.build_tools)
    if not (build_tools / "VC/Auxiliary/Build/vcvars64.bat").is_file():
        raise SystemExit("Visual Studio Build Tools not found at " + str(build_tools))
    work.mkdir(parents=True, exist_ok=True)
    dist.mkdir(parents=True, exist_ok=True)

    # Sources first: every check runs before anything is compiled, so a missing
    # network or a wrong archive is reported instead of a half-built prefix.
    freerdp_source = ensure_source(work, FREERDP)
    vnc_source = ensure_source(work, LIBVNC)

    build_zlib(work, dist, build_tools, arguments.force)
    build_openssl(work, dist, build_tools, arguments.perl, arguments.force)
    freerdp_build = build_freerdp(work, dist, build_tools, freerdp_source, arguments.force)
    vnc_build = build_libvnc(work, dist, build_tools, vnc_source, arguments.force)

    libraries = collect_archives(freerdp_build, dist, FREERDP_LIBRARIES)
    libraries.extend(collect_archives(vnc_build, dist, LIBVNC_LIBRARIES))
    missing = [
        name
        for name in [*FREERDP_LIBRARIES, *LIBVNC_LIBRARIES, *CRYPTO_LIBRARIES, ZLIB_LIBRARY]
        if not (dist / "lib" / (name + ".lib")).is_file()
    ]
    if missing:
        raise SystemExit("missing archives after the build: " + ", ".join(missing))
    libraries.extend(CRYPTO_LIBRARIES)
    libraries.append(ZLIB_LIBRARY)
    include_dirs = [
        dist / "include",
        freerdp_source / "include",
        freerdp_source / "winpr/include",
        freerdp_build / "include",
        freerdp_build / "winpr/include",
        vnc_source / "include",
        vnc_build / "include",
    ]
    write_manifest(dist, [directory for directory in include_dirs if directory.is_dir()],
                   libraries)
    write_licenses(work, dist)
    if arguments.prune:
        prune(work)
    log("done: " + ", ".join(libraries))
    return 0


# The link order on macOS and Linux. A Unix linker reads each archive once, in order,
# so an archive comes before the archives it calls into: the client library and its
# channel halves before FreeRDP, FreeRDP before WinPR, both protocol libraries before
# OpenSSL and zlib.
POSIX_LINK_ORDER = [
    "freerdp-client3",
    "remdesk-common",
    "rdpsnd-common",
    "freerdp3",
    "winpr3",
    "vncclient",
    *POSIX_CRYPTO_LIBRARIES,
    POSIX_ZLIB_LIBRARY,
]


def main_posix(arguments: argparse.Namespace) -> int:
    """The macOS arm64 and Linux x86_64 build: same pins, the system C compiler."""
    machine = platform.machine()
    if (PLATFORM, machine) not in OPENSSL_TARGETS:
        raise SystemExit(
            "The native desktop build supports Windows x86_64, macOS arm64 and Linux "
            "x86_64; this is " + PLATFORM + " " + machine + ". crates/desktop reports "
            "the protocols as unavailable here instead of claiming support."
        )
    for tool in ["cmake", "ninja", "make", "git"]:
        if shutil.which(tool) is None:
            raise SystemExit(tool + " is needed to build the native desktop backends")
    if PLATFORM == "macos":
        # Every compiler this run starts, OpenSSL's included, targets the same macOS.
        os.environ["MACOSX_DEPLOYMENT_TARGET"] = MACOS_DEPLOYMENT_TARGET

    work = pathlib.Path(arguments.work).resolve()
    dist = pathlib.Path(arguments.dist).resolve() if arguments.dist else work / "dist"
    work.mkdir(parents=True, exist_ok=True)
    dist.mkdir(parents=True, exist_ok=True)

    # Sources first, as on Windows: a network or checksum failure stops the run
    # before anything is compiled.
    freerdp_source = ensure_source(work, FREERDP)
    vnc_source = ensure_source(work, LIBVNC)

    build_zlib_posix(work, dist, arguments.force)
    build_openssl_posix(work, dist, arguments.perl, arguments.force)
    freerdp_build = build_freerdp_posix(work, dist, freerdp_source, arguments.force)
    vnc_build = build_libvnc_posix(work, dist, vnc_source, arguments.force)

    missing = [
        name for name in POSIX_LINK_ORDER if not (dist / "lib" / archive_file(name)).is_file()
    ]
    if missing:
        raise SystemExit("missing archives after the build: " + ", ".join(missing))
    include_dirs = [
        dist / "include",
        freerdp_source / "include",
        freerdp_source / "winpr/include",
        freerdp_build / "include",
        freerdp_build / "winpr/include",
        vnc_source / "include",
        vnc_build / "include",
    ]
    write_manifest(dist, [directory for directory in include_dirs if directory.is_dir()],
                   list(POSIX_LINK_ORDER))
    write_licenses(work, dist)
    if arguments.prune:
        prune(work)
    log("done: " + ", ".join(POSIX_LINK_ORDER))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
