# Patched dependencies

## `gpui` 0.2.2

This copy is the released 0.2.2 crate with the patches listed below. The root
`Cargo.toml` routes `gpui` here with `[patch.crates-io]`. On a GPUI upgrade, go
through the list: re-apply each patch the new release does not already contain, and
re-run the check each one names before accepting the upgrade.

| patch | files | why |
| --- | --- | --- |
| Frame readback | `src/platform.rs`, `src/window.rs`, `src/platform/windows/{window,directx_renderer}.rs` | the capture tool reads the frame a hidden window rendered |
| Initial window background | `src/platform.rs`, `src/platform/windows/window.rs` | a light window does not open on a black flash |
| Clipboard images and mixed items | `src/platform.rs`, `src/platform/windows/{clipboard,dib}.rs`, `src/platform/windows.rs`, `src/platform/linux/x11/{client,clipboard}.rs`, `src/platform/linux/wayland/clipboard.rs`, `src/platform/mac/platform.rs` | remote-desktop clipboard: screenshots and text-plus-image copies |

### Frame readback

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
platforms keep the default `None`. Check after re-applying: the hidden-window capture
probe.

### Initial window background

A new window is filled with its class background brush before its first frame is
drawn, and the released crate always uses black, so a light theme opened on a black
flash.

- `src/platform.rs`: public `set_initial_window_background(Rgba)`, stored in an
  atomic, and the crate-internal `initial_window_background()` that reads it.
- `src/platform/windows/window.rs`: the window class's brush is that colour; a window
  opened after the application asked for another colour (a theme switch) replaces
  the class brush before it is shown and frees the old one.

Other platforms keep their own behaviour. Check after re-applying: open the
application with the light theme and watch for a black frame.

### Clipboard images and mixed items

The remote-desktop clipboard carries images and text together. The released crate
could neither read a bitmap-only Windows clipboard nor write an image on X11, wrote
only the first entry of an item on Windows, and on every platform read one entry: a
copy that offers both text and an image (spreadsheet cells, a slide, a picture with
its caption) lost one of them.

- `src/platform.rs`: public `ClipboardItem::new_entries(Vec<ClipboardEntry>)`, so an
  application can build one item holding a text and an image.
- Reading, on every platform: an item holds the first text and the first image the
  clipboard offers, instead of only the first of either. Callers that want text
  (`ClipboardItem::text`) get the text of a mixed copy; callers that look for an
  image find it beside the text. An item holding one entry reads as before.
- `src/platform/windows/clipboard.rs`, read: the formats are enumerated as before;
  the first text format (`CF_UNICODETEXT`, or `CF_HDROP` file names) and the first
  encoded image format each give an entry, in the order the program offered them.
  When no encoded image is offered, `CF_DIB` (else `CF_DIBV5`) is decoded to a PNG
  `ClipboardEntry::Image`, also beside text. PrintScreen, the snipping overlay and many
  older programs offer only a bitmap; Excel and PowerPoint offer one beside their
  text. An encoded image offered beside the bitmap still wins.
- `src/platform/windows/clipboard.rs`, bitmap cache: converting a bitmap to PNG takes
  about 100 ms for a 4K screen in a release build, and callers read the clipboard
  while drawing (the terminal and text-field context menus check for something to
  paste on every frame they are open). The converted entry is kept with the clipboard
  sequence number (`GetClipboardSequenceNumber`) it was read at, and served again
  until the number changes; a bitmap that would not convert is remembered the same
  way. Nothing is cached when the number is 0 (a session that may not see it).
- `src/platform/windows/clipboard.rs`, write: the first text entry and the first
  image entry are both written, instead of only the first entry; one failing does not
  keep the other off. An image is written as before (its own format plus PNG) and
  also as `CF_DIB`, because Paint, Office and similar programs paste only a bitmap.
  Images whose bitmap would exceed 256 MiB get no `CF_DIB` copy.
- `src/platform/windows/dib.rs` (new, declared in `src/platform/windows.rs`): the
  DIB-to-PNG and image-to-DIB conversions. Decoding uses `image`'s `BmpDecoder` without
  a file header, refuses a header that claims more than 512 MiB of pixels, and makes
  a 32-bit bitmap opaque when its alpha is zero everywhere; writing produces a
  `BITMAPINFOHEADER` with 32-bit `BI_RGB` rows bottom-up.
- `src/platform/linux/x11/client.rs` and `clipboard.rs`: writing an item that holds an
  image calls `Clipboard::set_image` (unused before), which now offers the image under
  the atom of its own format - it used to label every image `image/png` - and the
  item's text as `UTF8_STRING` beside it. Items without an image are written as
  text, as before.
- `src/platform/linux/x11/clipboard.rs`, read: `Inner::read` became `Inner::read_each`,
  which asks the owner for its `TARGETS` once and then reads the first offered format
  of each group; `get_any` passes the text formats and the image formats as two groups.
  It used to pass one list with the images first, so a copy offering both gave only
  the image. A group that fails does not keep the other out; `get_any` fails only when
  neither gives anything.
- `src/platform/linux/wayland/clipboard.rs`, read: the offer's text and its image are
  both read (`DataOffer::read_text_and_image`); it used to read the image only when
  there was no text.
- `src/platform/mac/platform.rs`: reading takes the string and then the first
  supported image type; it used to stop at the string. Writing an item with several
  entries also puts its first image on the pasteboard, in its own type, beside the
  text; it used to drop it.

Not changed: Wayland still writes text only, so an image cannot be put on the
clipboard there. X11 serves the selection in one property change, without INCR, so
an image larger than the X server's maximum request length (about 16 MiB with
BIG-REQUESTS on Xorg) cannot be handed to a requesting program. The macOS changes
have not been compiled on macOS (see below).

Check after re-applying: the conversion tests in `dib.rs`. `gpui` is not a member of
this workspace, so `cargo test -p gpui` is refused; the file depends only on `image`
and `anyhow` and is tested through a throwaway crate outside the workspace:

    # target/gpui-dib-tests/Cargo.toml
    [package]
    name = "gpui-dib-tests"
    version = "0.0.0"
    edition = "2024"
    publish = false

    [workspace]

    [dependencies]
    anyhow = "1"
    image = "0.25"

    # target/gpui-dib-tests/src/lib.rs
    #[path = "../../../vendor/gpui/src/platform/windows/dib.rs"]
    #[allow(dead_code)]
    mod dib;

    cargo test --manifest-path target/gpui-dib-tests/Cargo.toml

The Windows read and write paths themselves need a real clipboard: copy a
PrintScreen image and paste it in a desktop pane; copy an image in the application
and paste it in Paint; copy cells in Excel and check that a desktop pane offers the
remote side both the text and the picture.

The platform reads were checked with throwaway programs that include the vendored
file through `#[path]` and stand in the few `crate::` types it names
(`ClipboardItem`, `ClipboardEntry`, `ClipboardString`, `Image`, `ImageFormat`, `hash`):

- Windows (`clipboard.rs` and `dib.rs`): put `CF_UNICODETEXT` + `CF_DIB`, `CF_DIB`
  alone, text alone, `PNG` + text, and text + `PNG` + `CF_DIB` on the real clipboard,
  write a text-plus-PNG item with `write_to_clipboard`, and read each back with
  `read_from_clipboard`; time a 4K bitmap beside text on the first and later reads.
  Another program (clipboard history) may hold the clipboard for a moment after each
  change, so the probe retries opening it.
- X11 (`clipboard.rs`, plus a stand-in `util` crate with `defer`), under `xvfb-run`:
  a child process owns `CLIPBOARD` with an image and a text, an image alone, or a text
  alone; the parent reads it with `get_any`.

The Linux build (X11 and Wayland) is checked with `cargo check -p gpui` on Linux. The
macOS build cannot be checked from Windows or Linux: `gpui_media` generates its
bindings from the macOS SDK.

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
