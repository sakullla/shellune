#!/usr/bin/env python3
"""Build, run and publish the Shellune UI capture (R1, R2, R9, R11).

The Rust half - "shellune --ui-capture" - owns the scenarios and the pixels: it draws the
real shell in a window that is never shown, waits for the scenario's own state and for two
identical GPU frames, and writes one PNG per shot plus a manifest naming the theme, the
logical and physical size, the DPI, the language, the text size, the fixture and what the
shot cannot prove.

This script owns the rest of the command:

* it builds the binary with the "ui-capture" feature;
* it passes the filters through (--list, --quick, --theme, --size, --only) and captures
  into a staging directory;
* on success it publishes: it removes exactly the PNGs the previous manifest registered,
  moves this run's files into docs/verification/ui and writes the gallery;
* on failure it moves the staging directory aside as ".failed-<stamp>" and leaves the
  previous round's images, manifest and gallery untouched, so a failed run can never
  replace the baseline it was meant to compare against.

Usage:

    python scripts/capture-ui.py --list
    python scripts/capture-ui.py --quick
    python scripts/capture-ui.py --only "home-*" --theme dark --size 1120
    python scripts/capture-ui.py

"--self-test" checks the publishing rules without a window, a GPU or a build.
"""

from __future__ import annotations

import argparse
import datetime
import html
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

MANIFEST_NAME = "capture-ui-manifest.json"
GALLERY_NAME = "index.html"
STAGING_PREFIX = ".staging-"
FAILED_PREFIX = ".failed-"


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def stamp() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def target_directory(root: Path) -> Path:
    """The directory cargo writes to. A cargo config can move it away from target/."""
    try:
        completed = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                                   cwd=root, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, encoding="utf-8",
                                   errors="replace", check=True)
        value = json.loads(completed.stdout).get("target_directory")
        if isinstance(value, str) and value:
            return Path(value)
    except (subprocess.SubprocessError, ValueError, OSError):
        pass
    return root / "target"


def binary_path(root: Path) -> Path:
    name = "shellune.exe" if os.name == "nt" else "shellune"
    return target_directory(root) / "debug" / name


def build(root: Path) -> Path:
    """Builds the capture entry and returns where cargo put the executable.

    The binary path is read from cargo rather than guessed: the target directory can be
    moved by a cargo config, and a script that assumed target/debug would then run an
    old binary or none at all.
    """
    command = ["cargo", "build", "--features", "ui-capture", "--bin", "shellune",
               "--message-format=json-render-diagnostics"]
    print("building: " + " ".join(command[:-1]))
    completed = subprocess.run(command, cwd=root, check=True, stdout=subprocess.PIPE,
                               encoding="utf-8", errors="replace")
    executable = None
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target") or {}
        if target.get("name") == "shellune" and message.get("executable"):
            executable = message["executable"]
    if executable:
        return Path(executable)
    fallback = binary_path(root)
    print("cargo did not report an executable; falling back to " + str(fallback))
    return fallback


def load_manifest(directory: Path):
    path = directory / MANIFEST_NAME
    if not path.is_file():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def write_manifest(directory: Path, manifest) -> None:
    path = directory / MANIFEST_NAME
    path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")


def registered_files(manifest) -> list:
    """The image names a manifest claims, refusing anything that could leave the folder.

    A manifest is read back from disk, so it is treated as untrusted input: only a bare
    file name is ever deleted, never a path.
    """
    names = []
    if not isinstance(manifest, dict):
        return names
    for shot in manifest.get("shots") or []:
        name = shot.get("file") if isinstance(shot, dict) else None
        if not isinstance(name, str) or not name:
            continue
        if name != Path(name).name or name in {".", ".."}:
            continue
        names.append(name)
    return names


def remove_previous_images(dest: Path, previous) -> list:
    """Deletes the PNGs the previous manifest registered, and nothing else."""
    removed = []
    for name in registered_files(previous):
        path = dest / name
        if path.is_file():
            try:
                path.unlink()
                removed.append(name)
            except OSError as error:
                print("could not remove " + str(path) + ": " + str(error))
    return removed


def compare_with_previous(manifest, previous) -> dict:
    """Which shots changed since the previous round, by the checksum in the manifest."""
    if not isinstance(previous, dict):
        return {"previous": None, "changed": [], "added": []}
    old = {}
    for shot in previous.get("shots") or []:
        if isinstance(shot, dict) and shot.get("file"):
            old[shot["file"]] = shot.get("pixel_crc32")
    changed = []
    added = []
    for shot in manifest.get("shots") or []:
        if not isinstance(shot, dict):
            continue
        name = shot.get("file")
        if name not in old:
            added.append(name)
        elif old[name] != shot.get("pixel_crc32"):
            changed.append(name)
    return {
        "previous": previous.get("started"),
        "changed": sorted(name for name in changed if name),
        "added": sorted(name for name in added if name),
    }


def sort_text(value) -> str:
    return value if isinstance(value, str) else ""


def scenario_sort_key(shot):
    return (
        sort_text(shot.get("category")),
        sort_text(shot.get("scenario")),
        {"dark": 0, "light": 1}.get(sort_text(shot.get("theme")), 2),
        shot.get("logical_width") or 0,
        shot.get("logical_height") or 0,
        shot.get("zoom_percent") or 100,
        sort_text(shot.get("language")),
    )


def chips(shots, key, label) -> str:
    seen = []
    for shot in shots:
        value = shot.get(key)
        if value is not None and value not in seen:
            seen.append(value)
    parts = ['<button type="button" class="chip on" data-filter="' + key + '" data-value="*">'
             + html.escape(label) + "</button>"]
    for value in seen:
        text = str(value)
        parts.append('<button type="button" class="chip" data-filter="' + key + '" data-value="'
                     + html.escape(text) + '">' + html.escape(text) + "</button>")
    return '<div class="row">' + "".join(parts) + "</div>"


def card(shot, changed) -> str:
    name = sort_text(shot.get("file"))
    size = str(shot.get("logical_width")) + "x" + str(shot.get("logical_height"))
    note = ""
    if name in changed:
        note = '<span class="changed">changed since the last round</span>'
    lines = [
        '<figure class="card" data-scenario="' + html.escape(sort_text(shot.get("scenario")))
        + '" data-theme="' + html.escape(sort_text(shot.get("theme")))
        + '" data-size="' + html.escape(size)
        + '" data-language="' + html.escape(sort_text(shot.get("language"))) + '">',
        '<a href="' + html.escape(name) + '"><img loading="lazy" src="' + html.escape(name)
        + '" alt="' + html.escape(name) + '"></a>',
        "<figcaption>",
        "<strong>" + html.escape(sort_text(shot.get("title"))) + "</strong>" + note,
        '<div class="meta">' + " · ".join([
            html.escape(sort_text(shot.get("scenario"))),
            html.escape(sort_text(shot.get("theme"))),
            html.escape(size),
            str(shot.get("zoom_percent")) + "% text",
            html.escape(sort_text(shot.get("language"))),
        ]) + "</div>",
        '<div class="meta">' + " · ".join([
            str(shot.get("physical_width")) + "x" + str(shot.get("physical_height")) + " px",
            str(shot.get("dpi_percent")) + "% DPI",
            str(shot.get("bytes")) + " bytes",
            "crc " + html.escape(sort_text(shot.get("pixel_crc32"))),
        ]) + "</div>",
        '<div class="note">fixture: ' + html.escape(sort_text(shot.get("fixture"))) + "</div>",
        '<div class="note">does not prove: '
        + html.escape(sort_text(shot.get("evidence_boundary"))) + "</div>",
        "</figcaption>",
        "</figure>",
    ]
    return "".join(lines)


def bullet_list(lines) -> str:
    return "<ul>" + "".join("<li>" + html.escape(sort_text(line)) + "</li>" for line in lines) + "</ul>"


def render_gallery(manifest, previous_report) -> str:
    """The gallery: one card per shot, filterable by scenario, theme, size and language."""
    shots = sorted(manifest.get("shots") or [], key=scenario_sort_key)
    failures = [item for item in manifest.get("failures") or [] if isinstance(item, dict)]
    changed = set(previous_report.get("changed") or [])
    started = sort_text(manifest.get("started"))
    summary_bits = [
        "capture " + sort_text(manifest.get("status")),
        sort_text(manifest.get("mode")) + " run",
        started,
        str(len(shots)) + " shots",
        "scale " + str(manifest.get("scale_factor")),
        sort_text(manifest.get("method")),
    ]
    if previous_report.get("previous"):
        summary_bits.append("compared with " + sort_text(previous_report.get("previous")))
    summary = " · ".join(bit for bit in summary_bits if bit)

    isolation = [sort_text(key) + ": " + sort_text(value)
                 for key, value in sorted((manifest.get("isolation") or {}).items())]
    failure_html = ""
    if failures:
        failure_html = ("<h2>本次失败</h2>" + bullet_list(
            [sort_text(item.get("scenario")) + " " + sort_text(item.get("theme")) + " "
             + sort_text(item.get("size")) + ": " + sort_text(item.get("reason"))
             for item in failures]))
    changed_html = ""
    if changed:
        changed_html = "<h2>与本轮之前相比有变化的画面</h2>" + bullet_list(sorted(changed))

    parts = [
        "<!doctype html>",
        '<html lang="zh-CN"><head><meta charset="utf-8">',
        "<title>Shellune UI capture</title>",
        "<style>",
        ":root { color-scheme: light dark; }",
        'body { margin: 0 auto; max-width: 1400px; padding: 32px 24px 64px; font: 14px/1.5 '
        'system-ui, "Segoe UI", "Noto Sans SC", sans-serif; }',
        "h1 { font-size: 22px; margin: 0 0 4px; font-weight: 600; }",
        "h2 { font-size: 15px; margin: 32px 0 8px; font-weight: 600; }",
        ".summary, .note, .meta { color: #6b6f76; }",
        ".summary { margin: 0 0 24px; }",
        ".row { display: flex; flex-wrap: wrap; gap: 6px; margin: 0 0 8px; }",
        ".chip { border: 1px solid #c9ccd1; background: transparent; color: inherit; "
        "border-radius: 3px; padding: 3px 9px; font: inherit; cursor: pointer; }",
        ".chip.on { border-color: #0f7f8a; color: #0f7f8a; }",
        ".grid { display: grid; gap: 20px; grid-template-columns: repeat(auto-fill, "
        "minmax(420px, 1fr)); }",
        ".card { margin: 0; border: 1px solid #d8dade; border-radius: 4px; overflow: hidden; }",
        ".card img { display: block; width: 100%; height: auto; background: #000; }",
        "figcaption { padding: 10px 12px 12px; }",
        "figcaption strong { font-weight: 600; }",
        ".meta { font-size: 12px; }",
        ".note { font-size: 12px; margin-top: 4px; }",
        ".changed { font-size: 11px; border: 1px solid #b4741b; color: #b4741b; "
        "border-radius: 3px; padding: 0 5px; margin-left: 6px; }",
        "ul { margin: 6px 0 0; padding-left: 18px; font-size: 12px; }",
        ".hidden { display: none; }",
        "</style></head><body>",
        "<h1>Shellune UI capture</h1>",
        '<p class="summary">' + html.escape(summary) + "</p>",
        "<h2>过滤</h2>",
        chips(shots, "scenario", "场景"),
        chips(shots, "theme", "主题"),
        chips(shots, "size", "尺寸"),
        chips(shots, "language", "语言"),
        "<h2>画面</h2>",
        '<div class="grid" id="grid">',
        "".join(card(shot, changed) for shot in shots),
        "</div>",
        "<h2>证据边界</h2>",
        bullet_list(manifest.get("evidence_boundary") or []),
        "<h2>隔离</h2>",
        bullet_list(isolation),
        failure_html,
        changed_html,
        "<script>",
        'const state = { scenario: "*", theme: "*", size: "*", language: "*" };',
        "function apply() {",
        '  for (const card of document.querySelectorAll(".card")) {',
        '    const keys = ["scenario", "theme", "size", "language"];',
        '    const shown = keys.every((key) => state[key] === "*" || card.dataset[key] === state[key]);',
        '    card.classList.toggle("hidden", !shown);',
        "  }",
        '  for (const chip of document.querySelectorAll(".chip")) {',
        '    chip.classList.toggle("on", state[chip.dataset.filter] === chip.dataset.value);',
        "  }",
        "}",
        'for (const chip of document.querySelectorAll(".chip")) {',
        '  chip.addEventListener("click", () => {',
        "    state[chip.dataset.filter] = chip.dataset.value;",
        "    apply();",
        "  });",
        "}",
        "apply();",
        "</script>",
        "</body></html>",
    ]
    return "\n".join(parts) + "\n"


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    return (struct.pack(">I", len(payload)) + kind + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF))


def recompress_png(path: Path) -> int:
    """Rewrites a PNG with a real deflate stream, keeping every pixel.

    The capture tool writes its PNGs with stored deflate blocks, because its job is to
    record a frame rather than to compress one. That leaves a 1120x720 shot at about five
    megabytes, and a gallery of them would be a hundred megabytes of repository for no
    information gain. Repacking the same scan lines with zlib is lossless and is verified
    here by decompressing what was written.

    Returns the file's new size.
    """
    data = path.read_bytes()
    if data[:8] != PNG_SIGNATURE:
        raise ValueError(str(path) + " is not a PNG")
    at = 8
    header = None
    pixels = b""
    while at < len(data):
        (length,) = struct.unpack(">I", data[at:at + 4])
        kind = data[at + 4:at + 8]
        payload = data[at + 8:at + 8 + length]
        (crc,) = struct.unpack(">I", data[at + 8 + length:at + 12 + length])
        if zlib.crc32(kind + payload) & 0xFFFFFFFF != crc:
            raise ValueError(str(path) + " has a damaged " + kind.decode("ascii") + " chunk")
        if kind == b"IHDR":
            header = payload
        elif kind == b"IDAT":
            pixels += payload
        elif kind != b"IEND":
            raise ValueError(str(path) + " holds an unexpected " + kind.decode("ascii") + " chunk")
        at += 12 + length
    if header is None:
        raise ValueError(str(path) + " has no header")
    raw = zlib.decompress(pixels)
    packed = zlib.compress(raw, 9)
    if zlib.decompress(packed) != raw:
        raise ValueError("recompressing " + str(path) + " changed its pixels")
    out = PNG_SIGNATURE + png_chunk(b"IHDR", header) + png_chunk(b"IDAT", packed)
    out += png_chunk(b"IEND", b"")
    path.write_bytes(out)
    return len(out)


def label_for(dest: Path) -> str:
    """How the manifest names the published directory: relative to the repository when
    it is inside it, so the record does not carry one machine's absolute path."""
    root = repo_root()
    try:
        return dest.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return str(dest)


def publish(dest: Path, staging: Path, manifest, previous, label) -> dict:
    """Replaces the published round with this one, after it succeeded."""
    report = compare_with_previous(manifest, previous)
    removed = remove_previous_images(dest, previous)
    moved = []
    for entry in sorted(staging.iterdir()):
        if not entry.is_file():
            continue
        if entry.suffix.lower() == ".png":
            # A damaged frame must not be published as evidence: recompressing checks the
            # chunks and the pixels, so a corrupt file stops the run before it replaces
            # the previous round.
            try:
                recompress_png(entry)
            except (ValueError, zlib.error) as error:
                shutil.rmtree(staging, ignore_errors=True)
                raise SystemExit("refusing to publish a damaged image: " + str(error))
        target = dest / entry.name
        shutil.move(str(entry), str(target))
        moved.append(entry.name)
    # The run recorded the staging directory it was given; the published manifest names
    # where the images really are, so a reader is never pointed at a directory that the
    # next run has already deleted.
    manifest = dict(manifest)
    manifest["out"] = label
    for shot in manifest.get("shots") or []:
        if isinstance(shot, dict) and shot.get("file"):
            path = dest / shot["file"]
            if path.is_file():
                shot["bytes"] = path.stat().st_size
    write_manifest(dest, manifest)
    (dest / GALLERY_NAME).write_text(
        render_gallery(manifest, report), encoding="utf-8")
    shutil.rmtree(staging, ignore_errors=True)
    report["removed"] = removed
    report["moved"] = moved
    return report


def keep_failed(dest: Path, staging: Path) -> Path:
    """Keeps a failed run's diagnostics beside the published round, without publishing them."""
    target = dest / (FAILED_PREFIX + stamp())
    suffix = 1
    while target.exists():
        target = dest / (FAILED_PREFIX + stamp() + "-" + str(suffix))
        suffix += 1
    shutil.move(str(staging), str(target))
    return target


def clean_stale_staging(dest: Path) -> None:
    """Removes staging directories a crashed run left behind. Published files are never touched."""
    if not dest.is_dir():
        return
    for entry in dest.iterdir():
        if entry.is_dir() and entry.name.startswith(STAGING_PREFIX):
            shutil.rmtree(entry, ignore_errors=True)


def run_capture(dest: Path, binary: Path, passthrough) -> tuple:
    """Captures into a staging directory and reports what the tool said."""
    staging = dest / (STAGING_PREFIX + stamp())
    staging.mkdir(parents=True)
    command = [str(binary), "--ui-capture", "--out", str(staging)] + passthrough
    print("capturing: " + " ".join(command))
    completed = subprocess.run(command, cwd=str(repo_root()))
    return completed.returncode, staging


def capture(dest: Path, binary: Path, passthrough) -> int:
    dest.mkdir(parents=True, exist_ok=True)
    clean_stale_staging(dest)
    previous = load_manifest(dest)
    code, staging = run_capture(dest, binary, passthrough)
    manifest = load_manifest(staging)
    ok = code == 0 and isinstance(manifest, dict) and manifest.get("status") == "ok"
    if code == 2:
        # Bad arguments: the tool captured nothing, so there is nothing to diagnose and
        # nothing to keep beside the baseline.
        shutil.rmtree(staging, ignore_errors=True)
        print("the capture tool refused the arguments (exit 2); nothing was captured and "
              "the previous round is untouched.")
        return 2
    if not ok:
        kept = keep_failed(dest, staging)
        print("capture failed (exit " + str(code) + "); the previous round is untouched.")
        print("diagnostics: " + str(kept))
        if isinstance(manifest, dict):
            for failure in manifest.get("failures") or []:
                if isinstance(failure, dict):
                    print("  " + sort_text(failure.get("scenario")) + " "
                          + sort_text(failure.get("theme")) + " "
                          + sort_text(failure.get("size")) + ": "
                          + sort_text(failure.get("reason")))
        return 1 if code == 0 else code
    report = publish(dest, staging, manifest, previous, label_for(dest))
    print("published " + str(len(report["moved"])) + " file(s) to " + str(dest))
    print("  gallery: " + str(dest / GALLERY_NAME))
    print("  manifest: " + str(dest / MANIFEST_NAME))
    print("  removed from the previous round: " + str(len(report["removed"])))
    if report["changed"]:
        print("  changed since the previous round: " + ", ".join(report["changed"]))
    return 0


# ---------------------------------------------------------------------------
# Self test: the publishing rules, without a window, a GPU or a build
# ---------------------------------------------------------------------------


def write_png(path: Path, width: int, height: int, colour) -> None:
    """A PNG in the shape the capture tool writes: one stored deflate block."""
    raw = b"".join(b"\x00" + bytes(colour) * width for _ in range(height))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    path.write_bytes(PNG_SIGNATURE + png_chunk(b"IHDR", header)
                     + png_chunk(b"IDAT", zlib.compress(raw, 0)) + png_chunk(b"IEND", b""))


def fake_shot(name: str, scenario: str, crc: str) -> dict:
    return {
        "file": name,
        "scenario": scenario,
        "title": scenario,
        "category": "home",
        "theme": "dark",
        "language": "en",
        "zoom_percent": 100,
        "logical_width": 1120,
        "logical_height": 720,
        "physical_width": 1120,
        "physical_height": 720,
        "scale_factor": 1.0,
        "dpi_percent": 100,
        "bytes": 12,
        "distinct_colors": 100,
        "mean_luminance": 0.2,
        "pixel_crc32": crc,
        "window_visible_during_capture": False,
        "frames_drawn": 6,
        "fixture": "fixture",
        "evidence_boundary": "boundary",
    }


def fake_manifest(shots, status="ok") -> dict:
    return {
        "tool": "shellune-ui-capture",
        "schema": 1,
        "status": status,
        "mode": "quick",
        "started": "2026-11-12T09:00:00Z",
        "scale_factor": 1.0,
        "method": "GPU back-buffer readback",
        "isolation": {"credentials": "MemorySecrets"},
        "evidence_boundary": ["a capture is not a behaviour test"],
        "shots": shots,
        "failures": [],
    }


def self_test() -> int:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        dest = root / "ui"
        dest.mkdir()
        # A published round: two images, its manifest and a gallery.
        for name in ("home-empty-1120x720-dark.png", "settings-1120x720-dark.png"):
            write_png(dest / name, 32, 18, (17, 18, 24))
        write_manifest(dest, fake_manifest([
            fake_shot("home-empty-1120x720-dark.png", "home-empty", "aaaa"),
            fake_shot("settings-1120x720-dark.png", "settings", "bbbb"),
        ]))
        (dest / GALLERY_NAME).write_text("old gallery", encoding="utf-8")
        # A file the previous round never registered has to survive.
        (dest / "keep-me.txt").write_text("keep", encoding="utf-8")

        staging = dest / (STAGING_PREFIX + "test")
        staging.mkdir()
        write_png(staging / "home-empty-1120x720-dark.png", 32, 18, (11, 12, 16))
        write_png(staging / "terminal-local-1120x720-dark.png", 32, 18, (11, 12, 16))
        manifest = fake_manifest([
            fake_shot("home-empty-1120x720-dark.png", "home-empty", "cccc"),
            fake_shot("terminal-local-1120x720-dark.png", "terminal-local", "dddd"),
        ])
        write_manifest(staging, manifest)

        report = publish(dest, staging, manifest, None, "test/ui")
        assert (dest / "keep-me.txt").is_file(), "an unregistered file was removed"
        assert (dest / "terminal-local-1120x720-dark.png").is_file(), "the new shot is missing"
        assert not staging.exists(), "the staging directory was left behind"
        assert report["moved"], "nothing was published"

        # Publishing over a previous round removes exactly what that round registered.
        previous = fake_manifest([
            fake_shot("home-empty-1120x720-dark.png", "home-empty", "cccc"),
            fake_shot("terminal-local-1120x720-dark.png", "terminal-local", "dddd"),
        ])
        write_manifest(dest, previous)
        write_png(dest / "stale-1120x720-dark.png", 32, 18, (17, 18, 24))
        previous = fake_manifest([
            fake_shot("home-empty-1120x720-dark.png", "home-empty", "cccc"),
            fake_shot("stale-1120x720-dark.png", "stale", "eeee"),
        ])
        write_manifest(dest, previous)
        staging = dest / (STAGING_PREFIX + "second")
        staging.mkdir()
        for name in ("home-empty-1120x720-dark.png", "settings-1120x720-dark.png"):
            write_png(staging / name, 32, 18, (11, 12, 16))
        next_manifest = fake_manifest([
            fake_shot("home-empty-1120x720-dark.png", "home-empty", "ffff"),
            fake_shot("settings-1120x720-dark.png", "settings", "1111"),
        ])
        write_manifest(staging, next_manifest)
        report = publish(dest, staging, next_manifest, load_manifest(dest), "test/ui")
        assert not (dest / "stale-1120x720-dark.png").exists(), "a registered image was kept"
        assert (dest / "keep-me.txt").is_file(), "an unregistered file was removed"
        assert report["changed"] == ["home-empty-1120x720-dark.png"], report["changed"]
        published = load_manifest(dest)
        assert published is not None and published.get("out") == "test/ui", (
            "the published manifest does not name where the images are")
        gallery = (dest / GALLERY_NAME).read_text(encoding="utf-8")
        for needle in ("settings-1120x720-dark.png", "home-empty", "data-filter", "证据边界"):
            assert needle in gallery, "the gallery does not mention " + needle
        assert "old gallery" not in gallery, "the gallery was not replaced"

        # A published image is repacked losslessly, and a damaged one is refused.
        assert published is not None
        published_bytes = published["shots"][0]["bytes"]
        on_disk = (dest / published["shots"][0]["file"]).stat().st_size
        assert published_bytes == on_disk, "the manifest does not report the published size"
        sample = root / "sample.png"
        write_png(sample, 64, 64, (17, 18, 24))
        stored = sample.stat().st_size
        assert recompress_png(sample) < stored, "recompression did not shrink the file"
        damaged = root / "damaged.png"
        damaged.write_bytes(b"not a png at all")
        try:
            recompress_png(damaged)
            raise AssertionError("a damaged image was accepted")
        except ValueError:
            pass

        # A manifest that names a path outside the folder cannot delete anything.
        outside = root / "outside.png"
        outside.write_bytes(b"keep")
        write_manifest(dest, fake_manifest([fake_shot("../outside.png", "outside", "2222")]))
        remove_previous_images(dest, load_manifest(dest))
        assert outside.is_file(), "a manifest path escaped the folder"

        # A failed run keeps the previous round and leaves its own diagnostics aside.
        before = (dest / GALLERY_NAME).read_text(encoding="utf-8")
        staging = dest / (STAGING_PREFIX + "failed")
        staging.mkdir()
        write_manifest(staging, fake_manifest([], status="failed"))
        kept = keep_failed(dest, staging)
        assert kept.is_dir(), "the failed run left no diagnostics"
        assert kept.name.startswith(FAILED_PREFIX)
        assert (dest / GALLERY_NAME).read_text(encoding="utf-8") == before, (
            "a failed run replaced the gallery")
        assert (dest / "home-empty-1120x720-dark.png").is_file(), (
            "a failed run removed a published image")

        # A staging directory a crashed run left behind is cleaned up.
        stale = dest / (STAGING_PREFIX + "stale")
        stale.mkdir()
        clean_stale_staging(dest)
        assert not stale.exists(), "stale staging was not cleaned"
        assert (dest / "keep-me.txt").is_file()
    print("self-test ok")
    return 0


def main(argv) -> int:
    parser = argparse.ArgumentParser(
        description="Capture the real Shellune UI in a hidden window and publish the gallery.")
    parser.add_argument("--list", action="store_true",
                        help="print the scenarios and their shots, capture nothing")
    parser.add_argument("--quick", action="store_true",
                        help="only the main pages, in the dark palette")
    parser.add_argument("--theme", choices=("dark", "light", "both"),
                        help="which palettes to capture")
    parser.add_argument("--size", action="append", metavar="WIDTH[xHEIGHT]",
                        help="keep the shots at this logical size; repeatable")
    parser.add_argument("--only", action="append", metavar="PATTERN",
                        help="keep the scenarios whose id matches (glob); repeatable")
    parser.add_argument("--out", metavar="DIRECTORY",
                        help="where to publish (default: docs/verification/ui)")
    parser.add_argument("--bin", metavar="PATH",
                        help="the capture binary (default: target/debug/shellune)")
    parser.add_argument("--no-build", action="store_true",
                        help="use the binary as it is, without building")
    parser.add_argument("--self-test", action="store_true",
                        help="check the publishing rules without a window or a build")
    arguments = parser.parse_args(argv)

    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    if arguments.self_test:
        return self_test()

    root = repo_root()
    binary = Path(arguments.bin) if arguments.bin else binary_path(root)

    passthrough = []
    if arguments.list:
        passthrough.append("--list")
    if arguments.quick:
        passthrough.append("--quick")
    if arguments.theme:
        passthrough += ["--theme", arguments.theme]
    for size in arguments.size or []:
        passthrough += ["--size", size]
    for pattern in arguments.only or []:
        passthrough += ["--only", pattern]

    if not arguments.no_build and not arguments.bin:
        binary = build(root)
    if not binary.is_file():
        print("no capture binary at " + str(binary) + "; build it with "
              "cargo build --features ui-capture --bin shellune", file=sys.stderr)
        return 2

    if arguments.list:
        # The tool is a GUI-subsystem program: its stdout is reliable only when it is a
        # file, so the list is written to one and read back.
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "scenarios.json"
            with path.open("w", encoding="utf-8") as sink:
                completed = subprocess.run([str(binary), "--ui-capture", "--list"],
                                           cwd=str(root), stdout=sink)
            print(path.read_text(encoding="utf-8"))
        return completed.returncode

    dest = Path(arguments.out) if arguments.out else root / "docs" / "verification" / "ui"
    if not dest.is_absolute():
        dest = root / dest
    return capture(dest, binary, passthrough)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
