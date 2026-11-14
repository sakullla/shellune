#!/usr/bin/env python3
"""Build the pinned native desktop dependencies of Shellune.

The RDP and VNC sessions in crates/desktop need two protocol libraries. This
script builds both from source with MSVC into one prefix and writes the manifest
crates/desktop/build.rs reads:

    target/native/dist/shellune-native.txt

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

Nothing is downloaded at run time and no installed DLL is used: the shim links
the static archives built here. Build prerequisites live in vendor/README.md -
Visual Studio Build Tools (MSVC, CMake, Ninja) and, for OpenSSL, a Perl with its
standard modules (portable Strawberry Perl works).

Usage:
    python packaging/native/build_native.py
    python packaging/native/build_native.py --perl C:/Strawberry/perl/bin/perl.exe
    python packaging/native/build_native.py --work target/native --dist target/native/dist
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import pathlib
import shutil
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]

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


def already_built(dist: pathlib.Path, names: list[str], force: bool) -> bool:
    """Skips a step whose archives are already in the prefix.

    The script is meant to be re-runnable: after a failure in a later step, or after
    adding an option, nobody should have to rebuild OpenSSL again from scratch.
    """
    if force:
        return False
    return all((dist / "lib" / (name + ".lib")).is_file() for name in names)


def run(command: list[str], cwd: pathlib.Path | None = None) -> None:
    log(" ".join(str(part) for part in command))
    subprocess.run(command, cwd=cwd, check=True)


def run_msvc(build_tools: pathlib.Path, work: pathlib.Path, name: str, lines: list[str]) -> None:
    """Runs shell lines inside the MSVC environment from vcvars64.bat."""
    vcvars = build_tools / "VC/Auxiliary/Build/vcvars64.bat"
    wrapper = work / ("step-" + name + ".cmd")
    body = ["@echo off", 'call "' + vcvars.as_posix() + '" >nul', "if errorlevel 1 exit /b 2"]
    body.extend(lines)
    body.append("exit /b %errorlevel%")
    wrapper.write_text("\r\n".join(body) + "\r\n", encoding="utf-8")
    log("step " + name)
    subprocess.run(["cmd", "/c", str(wrapper)], check=True)


def quoted(path: pathlib.Path) -> str:
    return '"' + path.as_posix() + '"'


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


def build_zlib(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
               force: bool) -> None:
    if already_built(dist, ["zlibstatic"], force):
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


def build_openssl(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
                  perl: str, force: bool) -> None:
    if already_built(dist, ["libcrypto", "libssl"], force):
        log("openssl already installed")
        return
    source = fetch_external(work, "openssl", EXTERNALS["openssl"])
    log_path = work / "openssl-build.log"
    # /MD matches the MSVC runtime Rust links against; a /MT libcrypto would give
    # the process two heaps and mix allocations across the boundary.
    run_msvc(build_tools, work, "openssl", [
        "cd /d " + quoted(source),
        " ".join([
            quoted(pathlib.Path(perl)), "Configure VC-WIN64A no-asm no-shared",
            "no-tests no-docs no-apps", 'CFLAGS="/MD -O2"',
            "--prefix=" + quoted(dist),
            "--openssldir=" + quoted(dist / "ssl"),
            ">", quoted(log_path),
        ]),
        "nmake /nologo build_libs >> " + quoted(log_path),
        "nmake /nologo install_dev >> " + quoted(log_path),
    ])


def build_freerdp(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
                  source: pathlib.Path, force: bool) -> pathlib.Path | None:
    build = work / "build" / "freerdp"
    if already_built(dist, FREERDP_LIBRARIES, force):
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
    return build


def build_libvnc(work: pathlib.Path, dist: pathlib.Path, build_tools: pathlib.Path,
                 source: pathlib.Path, force: bool) -> pathlib.Path | None:
    build = work / "build" / "libvncserver"
    if already_built(dist, LIBVNC_LIBRARIES, force):
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
    return build


def collect_archives(build: pathlib.Path, dist: pathlib.Path, names: list[str]) -> list[str]:
    """Copies the static archives a build produced into the prefix."""
    lib_dir = dist / "lib"
    lib_dir.mkdir(parents=True, exist_ok=True)
    wanted = {name.lower() for name in names}
    found: list[str] = []
    for archive in build.rglob("*.lib"):
        if archive.stem.lower() in wanted:
            shutil.copy2(archive, lib_dir / archive.name)
            found.append(archive.stem)
    return found


def write_manifest(dist: pathlib.Path, include_dirs: list[pathlib.Path],
                   libraries: list[str]) -> None:
    lines = [
        "# Generated by packaging/native/build_native.py. Do not edit by hand.",
        "# freerdp=" + FREERDP["version"] + " " + FREERDP["commit"]
        + " (" + FREERDP["license"] + ")",
        "# freerdp tarball sha256=" + FREERDP["sha256"],
        "# libvncserver=" + LIBVNC["version"] + " " + LIBVNC["commit"]
        + " (" + LIBVNC["license"] + ")",
        "# libvncserver tarball sha256=" + LIBVNC["sha256"],
        "# zlib=" + EXTERNALS["zlib"]["tag"] + " " + EXTERNALS["zlib"]["commit"]
        + " (" + EXTERNALS["zlib"]["license"] + ")",
        "# openssl=" + EXTERNALS["openssl"]["tag"] + " " + EXTERNALS["openssl"]["commit"]
        + " (" + EXTERNALS["openssl"]["license"] + ")",
    ]
    lines.extend("# freerdp option " + option for option in FREERDP_OPTIONS)
    lines.extend("# libvncserver option " + option for option in LIBVNC_OPTIONS)
    lines.extend("include=" + str(directory) for directory in include_dirs)
    lines.append("lib_dir=" + str(dist / "lib"))
    lines.extend("lib=" + library for library in libraries)
    lines.extend("syslib=" + library for library in WINDOWS_SYSTEM_LIBRARIES)
    lines.append("protocol=rdp")
    lines.append("protocol=vnc")
    target = dist / "shellune-native.txt"
    target.write_text("\n".join(lines) + "\n", encoding="utf-8")
    log("wrote " + str(target))


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
    arguments = parser.parse_args()

    if sys.platform != "win32":
        raise SystemExit(
            "Only the Windows build is implemented and verified. macOS and Linux "
            "builds of FreeRDP/LibVNCClient are not wired up; crates/desktop reports "
            "the protocol as unavailable there instead of claiming support."
        )

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
    log("done: " + ", ".join(libraries))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())