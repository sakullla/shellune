# Patched dependencies

## `gpui` 0.2.2

The capture tool must read the frame the hidden window actually rendered. The
released crate has no public CPU readback, so this copy adds one small, rendering-
neutral API:

- `src/platform.rs`: public `FrameImage` and a default
  `PlatformWindow::capture_frame` that returns `None`.
- `src/window.rs`: public `Window::capture_frame`.
- `src/platform/windows/window.rs`: forwards to the DirectX renderer.
- `src/platform/windows/directx_renderer.rs`: copies the swap-chain back buffer to a
  staging texture and returns tightly packed RGB.

No drawing, layout, input or platform lifecycle behaviour is changed; other
platforms keep the default `None`. The root `Cargo.toml` routes `gpui` here with
`[patch.crates-io]`. On a GPUI upgrade, re-apply this API and re-run the hidden-window
capture probe before accepting the upgrade.

## `xattr` 0.2.3

`gpui` 0.2.2 → `gpui_http_client` → `zed-async-tar` (default feature `xattr`) →
`xattr` 0.2.x. `xattr` 0.2.3 names `libc::ENOATTR` on Linux, but current `libc` 0.2
releases no longer export that constant for Linux, so the Linux build fails with
`E0425: cannot find value ENOATTR in crate libc`.

This copy is the released 0.2.3 crate with one change in `src/sys/mod.rs`: on Linux
and Android `ENOATTR` is `libc::ENODATA` (glibc and bionic define `ENOATTR` as
`ENODATA`); every other platform keeps `libc::ENOATTR`. The root `Cargo.toml` routes
the dependency here with `[patch.crates-io]`.

Remove this directory and the patch entry once `gpui` no longer pulls in `xattr` 0.2.

## `freerdp` 3.33.1 and `libvncserver` 0.9.15

The RDP and VNC sessions in `crates/desktop` are the two protocol libraries, not a
reimplementation. Neither is checked into this repository. 3.3k upstream files in
a git tree make routine git commands exceed the Windows command-line limit, so both
are fetched at build time instead: `packaging/native/build_native.py` downloads one
source tarball per dependency, verifies its sha256, unpacks it under
`target/native/src/<name>` and only then compiles it. A build still never depends on
a library that happens to be installed on the machine.

| dependency | upstream | commit | tarball sha256 | license |
| --- | --- | --- | --- | --- |
| FreeRDP 3.33.1 | https://github.com/FreeRDP/FreeRDP | `65bcca1913a28cbdd7eb913599f692bd2b707991` | `97771fc4b967b4262bbe690f9b666402ad4c717717527c006b4acea8e21fffb8` | Apache-2.0 |
| LibVNCServer 0.9.15 | https://github.com/LibVNC/libvncserver | `9b54b1ec32731bd23158ca014dc18014db4194c3` | `662c311e0d4a8fc71bae6cf4759308948e4681c2e23987583ff2121cb1f1aedf` | GPL-2.0-or-later |

The tarball URLs name the commit, not the tag, so a moved tag cannot change what is
downloaded; the sha256 catches a re-cut archive as well. Both were checked against
the tags at pin time (`3.33.1`, `LibVNCServer-0.9.15`) and the unpacked tarballs
match those trees file for file, with only `.git`, `.github`, `.gitignore` and
`.gitattributes` absent because GitHub's archive exports omit them.

Two further build dependencies are fetched by tag, and the checked-out commit is
verified before anything is compiled:

| fetched by tag | tag | commit | license |
| --- | --- | --- | --- |
| zlib | `v1.3.1` | `51b7f2abdade71cd9bb0e7a373ef2610ec6f9daf` | Zlib |
| OpenSSL | `openssl-3.5.4` | `c1eeb9406b6142148f267594197d853403d10208` | Apache-2.0 |

License compatibility: LibVNCServer is GPL-2.0-or-later and this project is
GPL-3.0-only, which covers it. FreeRDP and OpenSSL are Apache-2.0 and zlib is
permissive; nothing here adds a distribution obligation beyond the licenses already
listed. Because no source tree is kept in this repository, the license texts come
from the fetched trees (`LICENSE` in FreeRDP, `COPYING` in LibVNCServer, both
under `target/native/src/<name>/`); packaging that ships a binary must include them.

### Building

`packaging/native/build_native.py` does the whole build and writes
`target/native/dist/shellune-native.txt`, the manifest
`crates/desktop/build.rs` reads. Nothing in that prefix is a shared library: the
shim links static archives, so a user's installed FreeRDP or LibVNCClient can never
be picked up by accident.

Prerequisites on Windows: Visual Studio 2022 Build Tools with MSVC, CMake and Ninja
(the script looks for `VC/Auxiliary/Build/vcvars64.bat`), a Perl with its standard
modules for OpenSSL's Configure (portable Strawberry Perl works, and `--perl` points
at it), and **network access** for the first run: nothing is pre-fetched into the
repository.

    python packaging/native/build_native.py --perl C:/Strawberry/perl/bin/perl.exe

That is the whole reproduction: on an empty `target/native` it downloads, verifies,
unpacks and builds zlib, OpenSSL, FreeRDP and LibVNCServer. Failures are reported per
step and are not skipped: a network error, a sha256 mismatch (with the expected and
the actual digest) and an unusable archive each print their own message, and a
mismatch stops before anything is compiled. Verified tarballs stay in
`target/native/downloads/` and unpacked trees in `target/native/src/`, so a repeat run
re-uses them; `--force` rebuilds a step whose archives are already installed.

The options are fixed in the script and copied into the manifest, so a built prefix
records how it was made:

* FreeRDP: static, monolithic, client and server applications off, X11/Wayland/SDL
  off, FFmpeg/SWScale/CUPS/Pulse/ALSA/OSS/PCSC/Kerberos/FUSE/manpages/sample off.
  The library API, TLS through OpenSSL and the client channels this project calls
  are compiled in; audio, drive, printer and smartcard redirection are not.
* LibVNCServer: static, zlib on, OpenSSL on for TLS/VeNCrypt, JPEG/PNG/GnuTLS/gcrypt/
  SASL off, examples off.
* OpenSSL: static, `no-asm`, `no-tests`, `no-apps`, built with `/MD` so it shares the C runtime
  Rust links against - a `/MT` libcrypto would put two heaps in one process.

### What is and is not verified

| platform | build | run |
| --- | --- | --- |
| Windows x86_64 | built here, repeatedly, from the pinned sources | shim and wrapper tests run against scripted peers; no real RDP or VNC server exists on this machine |
| macOS, Linux | not wired up | not claimed: `crates/desktop` reports both protocols as unavailable there |

The shim's own header (`crates/desktop/shim/shellune_desktop_shim.h`) states the ABI
promises; `tests/desktop_native.rs` checks the struct layout from both sides and exercises
connect, cancel, frame delivery, input, clipboard, unsupported security types and the
close ordering. Real endpoint behaviour - Windows RDP, xrdp, TigerVNC, TightVNC, TLS
and certificate decisions against a live server - is not covered by that suite and
must not be claimed from it.

### Updating a pin

Change the version, commit and license row above *and* the matching spec in
`packaging/native/build_native.py` (`version`, `tag`, `commit`, `url`, `sha256`), then
delete `target/native/src/<name>` and `target/native/downloads/<name>-<old>.tar.gz`,
re-run the build and `cargo test --test desktop_native`. The new `sha256` is the digest
of the archive the script downloads; take it from a run that reports the mismatch, or
hash the file yourself (`certutil -hashfile <file> SHA256`).

The tarball URL must name the commit, not the tag, so a re-pointed tag cannot change
what is downloaded. The shim compiles against the public headers only, so an
incompatible change there shows up as a compile error rather than silently.
