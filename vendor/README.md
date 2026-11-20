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
| Truncated text measure cache | `src/elements/text.rs` | a truncated line draws its "…" instead of a cut letter |

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

### Truncated text measure cache

A text element measures itself once per width the layout asks about and paints the
lines of the last measure it kept. In the released crate a one-line text
(`white-space: nowrap`) reused its first measure whenever it was asked again, because
the cache compared only the wrap width, which is `None` for every one-line measure.
The first ask is usually for the text's natural width, with no definite width and so
no truncation; the later ask at the width the row gives it returned that first,
uncut layout, `truncate_line` never ran, and the line was clipped at the element's
edge without its "…" ("roo", "1 connec" on the home page, a host address cut in
the middle of a number).

- `src/elements/text.rs`: `TextLayoutInner` keeps the `truncate_width` its lines were
  shaped at, and the measure cache is used only when that width is the same as the
  one being asked about, as well as the wrap width.
- `src/elements/text.rs`: each measure truncates a copy of the element's runs. The
  released closure truncated the runs it captured in place, so once a line could be
  measured at a second width, the whole text was cut with the runs of the first cut:
  on Windows a run then ended inside a character and DirectWrite's layout panicked
  ("end byte index 3 is not a char boundary; it is inside '台'" on the Chinese home
  page at 860x640). The test platform's text system ignores runs, so this half is
  checked by the capture: `shellune --ui-capture --only home-narrow` must finish.

Wrapping text and text without `text_overflow` are measured as before (their
truncate width is always `None`). Check after re-applying:
`cargo test -p shellune --bin shellune cut_words_in_rows_banners_and_sidebars_end_in_a_drawn_ellipsis`,
which reads the shaped line back and fails without the patch.

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

## Protocol libraries

There is no `freerdp`, `winpr` or `libvncserver` here, and the build downloads
nothing: the RDP client (`crates/desktop/src/rdp`) and the VNC client
(`crates/desktop/src/vnc`) are part of that crate, and its `build.rs` compiles one
small C file of diagnostic names. A release therefore needs no native prefix built
before `cargo build --workspace --release`, links no protocol library, and carries
no protocol library license text beside Shellune's `LICENSE`.

`ring`, the crypto backend of the SSH and TLS stacks, is the only dependency that
compiles C of its own; it does so inside its own build script, from sources it ships.
