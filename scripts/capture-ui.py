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
* a round publishes into a directory of its own: a full run into docs/verification/ui, a
  quick or filtered run into a subdirectory of it (quick/, only-home-hosts/ ...), so a
  narrow run can never delete the shots it did not take;
* on success it publishes in three steps: every staged PNG is checked and repacked
  losslessly first, then the whole round is moved into place, and only then are the
  images the previous round registered - and this one did not - removed. A failure in
  either of the first two steps leaves the previous round complete, and the command can
  simply be run again;
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
import hashlib
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
BACKUP_PREFIX = ".backup-"
FULL_ROUND = "full"


class RoundError(Exception):
    """A round that cannot be published. The published round is still the old one."""


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def stamp() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def slug(text: str) -> str:
    """A directory name for one part of a filter, safe on every filesystem."""
    cleaned = "".join(character if character.isalnum() or character in "-_." else "-"
                      for character in text.strip().lower())
    cleaned = "-".join(part for part in cleaned.split("-") if part)
    return cleaned[:60] or "run"


def round_key(quick: bool, theme, sizes, only) -> str:
    """Which round this run takes, as a directory name below the published root.

    A round keeps every shot it took and only ever replaces itself, so a quick run is
    published beside the full round instead of over it: narrowing a run used to delete
    the eighteen shots the narrowed run did not take, which is exactly what a baseline
    must not lose.
    """
    parts = []
    if quick:
        parts.append("quick")
    if theme and theme != "both":
        parts.append("theme-" + slug(theme))
    for size in sizes or []:
        parts.append("size-" + slug(size))
    for pattern in only or []:
        parts.append("only-" + slug(pattern))
    if not parts:
        return FULL_ROUND
    key = "+".join(parts)
    if len(key) > 80:
        # Long filter lists are still one round; the digest keeps the name usable.
        key = key[:60] + "+" + hashlib.sha256(key.encode("utf-8")).hexdigest()[:8]
    return key


def round_directory(root: Path, key: str) -> Path:
    return root if key == FULL_ROUND else root / key


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


def retire_previous(dest: Path, previous, published) -> list:
    """Deletes the PNGs the previous manifest registered, and nothing else.

    This runs only after the new round is completely in place: until then the previous
    round is the one a reader sees, and it has to stay whole. An image this round
    published too has already been replaced in place, so it is not removed.
    """
    removed = []
    for name in registered_files(previous):
        if name in published:
            continue
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


def render_gallery(manifest, previous_report, others=()) -> str:
    """The gallery: one card per shot, filterable by scenario, theme, size and language.

    `others` names the other rounds published beside this one, so a reader who lands on
    the quick round can find the full baseline without knowing the directory layout.
    """
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
    rounds_html = ""
    if others:
        rounds_html = ("<h2>其他轮次</h2>"
                       + '<p class="summary">'
                       + " · ".join('<a href="' + html.escape(href) + '">' + html.escape(label)
                                    + "</a>" for label, href in others)
                       + "</p>")

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
        rounds_html,
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


def staged_names(staging: Path) -> list:
    return sorted(entry.name for entry in staging.iterdir() if entry.is_file())


def check_staged_images(staging: Path) -> None:
    """Checks and repacks every image the round would publish, still inside staging.

    A damaged frame must not be published as evidence. Repacking verifies the chunks and
    the pixels, and it happens before anything is moved, so one bad shot stops the round
    while the published one is still whole and the command can be run again.
    """
    for entry in sorted(staging.iterdir()):
        if not entry.is_file() or entry.suffix.lower() != ".png":
            continue
        try:
            recompress_png(entry)
        except (ValueError, zlib.error) as error:
            raise RoundError("refusing to publish a damaged image: " + str(error)) from error


def prepare_round(staging: Path, manifest, previous, label, others) -> dict:
    """Makes the whole round ready to publish, without touching the published one.

    The manifest and the gallery are written into staging too: a round that cannot
    describe itself must not be half published, and the gallery a reader sees only ever
    names the round that is actually on disk.
    """
    check_staged_images(staging)
    prepared = dict(manifest)
    # The run recorded the staging directory it was given; the published manifest names
    # where the images really are, so a reader is never pointed at a directory the next
    # run has already deleted.
    prepared["out"] = label
    shots = [dict(shot) if isinstance(shot, dict) else shot
             for shot in prepared.get("shots") or []]
    prepared["shots"] = shots
    missing = []
    for shot in shots:
        if not isinstance(shot, dict) or not shot.get("file"):
            continue
        path = staging / shot["file"]
        if not path.is_file():
            missing.append(shot["file"])
            continue
        shot["bytes"] = path.stat().st_size
    if missing:
        raise RoundError("the manifest names images staging does not hold: "
                         + ", ".join(sorted(missing)))
    report = compare_with_previous(prepared, previous)
    write_manifest(staging, prepared)
    (staging / GALLERY_NAME).write_text(render_gallery(prepared, report, others),
                                        encoding="utf-8")
    return {"manifest": prepared, "report": report, "names": staged_names(staging)}


def install_order(names) -> list:
    """Images first, then the manifest, then the gallery.

    A reader's gallery must never be newer than the images it shows, so the index is
    installed last and the manifest - which the index is built from - just before it.
    """
    order = [name for name in names if name not in (MANIFEST_NAME, GALLERY_NAME)]
    for tail in (MANIFEST_NAME, GALLERY_NAME):
        if tail in names:
            order.append(tail)
    return order


def install_round(dest: Path, staging: Path, prepared, move=os.replace) -> list:
    """Moves a prepared round into place, and puts the old one back if a move fails.

    Staging lives inside the published directory, so a move is a rename on one
    filesystem: each file appears complete or not at all. What it replaces is kept in a
    backup until the whole round is in place, because a half moved round is worse than
    the old one: the gallery a reader has open would link to images that are gone.
    """
    backup = dest / (BACKUP_PREFIX + stamp())
    moved = []
    replaced = []
    installed = False
    try:
        for name in install_order(prepared["names"]):
            target = dest / name
            if target.exists():
                backup.mkdir(parents=True, exist_ok=True)
                move(target, backup / name)
                replaced.append(name)
            move(staging / name, target)
            moved.append(name)
        installed = True
    except OSError as error:
        for name in moved:
            # Put what this round moved back into staging first: the run can then simply
            # be repeated instead of captured again.
            try:
                move(dest / name, staging / name)
            except OSError:
                try:
                    (dest / name).unlink()
                except OSError:
                    pass
        for name in replaced:
            try:
                move(backup / name, dest / name)
            except OSError:
                print("could not put " + name + " back")
        raise RoundError("could not move the round into place: " + str(error)) from error
    finally:
        # A backup a rollback could not empty is the only copy of something: it stays.
        if backup.exists() and (installed or not any(backup.iterdir())):
            shutil.rmtree(backup, ignore_errors=True)
    return moved


def publish(dest: Path, staging: Path, manifest, previous, label, others=()) -> dict:
    """Publishes this round: prepare all of it, move all of it, and only then retire."""
    prepared = prepare_round(staging, manifest, previous, label, others)
    moved = install_round(dest, staging, prepared)
    # Only now: the previous round was the one on disk until this line, and it had to
    # stay complete for as long as that was true.
    removed = retire_previous(dest, previous, set(prepared["names"]))
    shutil.rmtree(staging, ignore_errors=True)
    report = prepared["report"]
    report["moved"] = moved
    report["removed"] = removed
    return report


def sibling_rounds(root: Path, dest: Path) -> list:
    """The other rounds a reader can reach, as (label, href) pairs for the gallery."""
    rounds = []
    if dest != root and (root / GALLERY_NAME).is_file():
        rounds.append((FULL_ROUND, "../" + GALLERY_NAME))
    if root.is_dir():
        for entry in sorted(root.iterdir()):
            if not entry.is_dir() or entry == dest or entry.name.startswith("."):
                continue
            if (entry / GALLERY_NAME).is_file():
                prefix = "../" if dest != root else ""
                rounds.append((entry.name, prefix + entry.name + "/" + GALLERY_NAME))
    return rounds


def keep_failed(dest: Path, staging: Path) -> Path:
    """Keeps a failed run's diagnostics beside the published round, without publishing them."""
    target = dest / (FAILED_PREFIX + stamp())
    suffix = 1
    while target.exists():
        target = dest / (FAILED_PREFIX + stamp() + "-" + str(suffix))
        suffix += 1
    shutil.move(str(staging), str(target))
    return target


def drop_empty_round(directory: Path) -> None:
    """Removes a round directory this run created and published nothing into.

    A run that captured nothing must not leave a directory behind that a reader could
    mistake for a round.
    """
    try:
        if directory.is_dir() and not any(directory.iterdir()):
            directory.rmdir()
    except OSError:
        pass


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


def capture(published: Path, dest: Path, binary: Path, passthrough) -> int:
    """Captures one round and publishes it into `dest`, beside the other rounds."""
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
        drop_empty_round(dest)
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
    try:
        report = publish(dest, staging, manifest, previous, label_for(dest),
                         sibling_rounds(published, dest))
    except RoundError as error:
        # The published round is untouched: the run may simply be repeated once what the
        # message names is fixed.
        kept = keep_failed(dest, staging)
        print("not published: " + str(error))
        print("the published round is unchanged; the staged round is kept at " + str(kept))
        return 1
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


def round_snapshot(directory: Path) -> dict:
    """Every published file of a round and its bytes, so a test can say it did not move."""
    return {entry.name: entry.read_bytes()
            for entry in sorted(directory.iterdir())
            if entry.is_file()}


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
        retire_previous(dest, load_manifest(dest), set())
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

        # A round that cannot be checked never touches the published one, and the run can
        # simply be repeated once the damage is fixed. This is the order that matters: the
        # gallery a reader already has open must never name an image that is gone.
        before = round_snapshot(dest)
        staging = dest / (STAGING_PREFIX + "damaged")
        staging.mkdir()
        write_png(staging / "home-empty-1120x720-dark.png", 32, 18, (1, 2, 3))
        (staging / "terminal-local-1120x720-dark.png").write_bytes(b"half a png")
        damaged_manifest = fake_manifest([
            fake_shot("home-empty-1120x720-dark.png", "home-empty", "3333"),
            fake_shot("terminal-local-1120x720-dark.png", "terminal-local", "4444"),
        ])
        write_manifest(staging, damaged_manifest)
        previous = load_manifest(dest)
        try:
            publish(dest, staging, damaged_manifest, previous, "test/ui")
            raise AssertionError("a damaged round was published")
        except RoundError:
            pass
        assert round_snapshot(dest) == before, "a damaged round changed the published one"
        # The same round publishes once the file is whole again.
        write_png(staging / "terminal-local-1120x720-dark.png", 32, 18, (1, 2, 3))
        report = publish(dest, staging, damaged_manifest, previous, "test/ui")
        assert report["moved"], "the repaired round did not publish"
        reprinted = load_manifest(dest)
        assert reprinted is not None
        assert [shot["pixel_crc32"] for shot in reprinted["shots"]] == ["3333", "4444"], (
            "the repaired round did not replace the manifest")

        # A move that fails half way puts the previous round back, and leaves the staged
        # round whole for the retry.
        before = round_snapshot(dest)
        staging = dest / (STAGING_PREFIX + "half")
        staging.mkdir()
        for name in ("home-empty-1120x720-dark.png", "terminal-local-1120x720-dark.png"):
            write_png(staging / name, 32, 18, (9, 9, 9))
        half_manifest = fake_manifest([
            fake_shot("home-empty-1120x720-dark.png", "home-empty", "5555"),
            fake_shot("terminal-local-1120x720-dark.png", "terminal-local", "6666"),
        ])
        write_manifest(staging, half_manifest)
        previous = load_manifest(dest)
        attempts = []

        def failing_move(source, target):
            attempts.append(str(target))
            if len(attempts) == 2:
                raise OSError("the volume went away")
            os.replace(source, target)

        prepared = prepare_round(staging, half_manifest, previous, "test/ui", ())
        try:
            install_round(dest, staging, prepared, move=failing_move)
            raise AssertionError("a failed move was accepted")
        except RoundError:
            pass
        assert round_snapshot(dest) == before, "a failed move left the round half published"
        report = publish(dest, staging, half_manifest, previous, "test/ui")
        assert report["moved"], "the round did not publish when it was repeated"

        # A quick round publishes beside the full one and replaces only itself: the
        # baseline keeps the shots the quick run did not take.
        published_root = root / "gallery"
        full = published_root
        full.mkdir()
        for index in range(3):
            write_png(full / ("home-%d-1120x720-dark.png" % index), 32, 18, (5, 5, 5))
        write_manifest(full, fake_manifest([
            fake_shot("home-%d-1120x720-dark.png" % index, "home-%d" % index, "aa%02d" % index)
            for index in range(3)]))
        (full / GALLERY_NAME).write_text("full gallery", encoding="utf-8")
        full_before = round_snapshot(full)

        quick = round_directory(published_root, round_key(True, None, None, None))
        assert quick == published_root / "quick", quick
        quick.mkdir()
        staging = quick / (STAGING_PREFIX + "quick")
        staging.mkdir()
        write_png(staging / "home-0-1120x720-dark.png", 32, 18, (5, 5, 5))
        quick_manifest = fake_manifest([fake_shot("home-0-1120x720-dark.png", "home-0", "aa00")])
        write_manifest(staging, quick_manifest)
        publish(quick, staging, quick_manifest, None, "gallery/quick",
                sibling_rounds(published_root, quick))
        assert round_snapshot(full) == full_before, "the quick round changed the full one"
        assert (quick / "home-0-1120x720-dark.png").is_file(), "the quick shot is missing"
        quick_gallery = (quick / GALLERY_NAME).read_text(encoding="utf-8")
        assert 'href="../index.html"' in quick_gallery, (
            "the quick gallery cannot reach the full round")
        # The full round, published at the root, has to find the rounds beside it too.
        assert ("quick", "quick/" + GALLERY_NAME) in sibling_rounds(published_root, published_root), (
            "the full gallery cannot reach the quick round")
        # A second quick round retires only what the quick round itself published.
        write_png(quick / "home-9-1120x720-dark.png", 32, 18, (6, 6, 6))
        write_manifest(quick, fake_manifest([
            fake_shot("home-0-1120x720-dark.png", "home-0", "aa00"),
            fake_shot("home-9-1120x720-dark.png", "home-9", "bb99"),
        ]))
        staging = quick / (STAGING_PREFIX + "again")
        staging.mkdir()
        write_png(staging / "home-0-1120x720-dark.png", 32, 18, (5, 5, 5))
        once_more = fake_manifest([fake_shot("home-0-1120x720-dark.png", "home-0", "aa00")])
        write_manifest(staging, once_more)
        publish(quick, staging, once_more, load_manifest(quick), "gallery/quick")
        assert not (quick / "home-9-1120x720-dark.png").exists(), "a retired image was kept"
        assert round_snapshot(full) == full_before, "the quick round changed the full one"

        # A round is named after what was asked for, so two narrow runs never publish over
        # each other and neither of them can land on the baseline.
        assert round_key(False, None, None, None) == FULL_ROUND
        assert round_key(False, "both", None, None) == FULL_ROUND
        assert round_key(True, None, None, None) == "quick"
        assert round_key(False, "light", None, None) == "theme-light"
        assert round_key(False, None, ["720x480"], None) == "size-720x480"
        assert round_key(False, "dark", None, ["home-hosts"]) == "theme-dark+only-home-hosts"
        long_key = round_key(False, None, None, ["a" * 40, "b" * 40])
        assert len(long_key) <= 80, long_key

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
                        help="the published root (default: docs/verification/ui); a full "
                             "run publishes there, a quick or filtered run publishes in a "
                             "subdirectory of it")
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

    published = Path(arguments.out) if arguments.out else root / "docs" / "verification" / "ui"
    if not published.is_absolute():
        published = root / published
    # A full run publishes at the root and every narrower run publishes beside it, in a
    # directory of its own: a quick run replaces the quick round, never the baseline it
    # did not shoot.
    key = round_key(arguments.quick, arguments.theme, arguments.size, arguments.only)
    dest = round_directory(published, key)
    print("round " + key + " publishes to " + str(dest))
    return capture(published, dest, binary, passthrough)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
