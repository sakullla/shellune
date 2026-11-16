#!/usr/bin/env python3
"""Write latest.json, the manifest the Shellune updater reads.

Usage:
    python packaging/updater-manifest.py --version 0.5.0 [--dist dist] \
        [--notes-file docs/release/0.5.0-release-notes.md] [--pub-date <RFC 3339>] \
        [--base-url <url>] [--output dist/latest.json]

Every platform the updater knows (crates/update/src/manifest.rs) must have its
release file and its signature in --dist:

    windows-x86_64            Shellune-<version>-setup.exe
    windows-x86_64-portable   Shellune-<version>-windows-x86_64-portable.zip
    darwin-aarch64            Shellune-<version>-macos-arm64.app.tar.gz
    linux-x86_64              shellune_<version>_amd64.deb

and `<file>.sig` beside each, as `shellune-sign sign <file> --version <version>`
writes it: the base64 of a minisign signature whose trusted comment names
`version:<version>`. A missing file, a missing or empty signature, or a signature
for another version stops the script before anything is written, so a release
never carries a manifest the updater would refuse.

The download addresses point at the tagged release:
https://github.com/sakullla/shellune/releases/download/v<version>/<file>.
"""

import argparse
import base64
import binascii
import datetime
import json
import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RELEASES = "https://github.com/sakullla/shellune/releases/download"

# The same rule as the semver crate the updater parses with (no leading "v").
VERSION = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)


class ManifestError(Exception):
    """Why no manifest can be written."""


def platform_files(version):
    """The release file of every platform key, in manifest order."""
    return [
        ("windows-x86_64", f"Shellune-{version}-setup.exe"),
        ("windows-x86_64-portable", f"Shellune-{version}-windows-x86_64-portable.zip"),
        ("darwin-aarch64", f"Shellune-{version}-macos-arm64.app.tar.gz"),
        ("linux-x86_64", f"shellune_{version}_amd64.deb"),
    ]


def normalize_version(text):
    """The version without a leading "v"; raises ManifestError when it is not one."""
    version = text.strip()
    if version[:1] in ("v", "V"):
        version = version[1:]
    if not VERSION.match(version):
        raise ManifestError(f'"{text}" is not a semantic version')
    return version


def signed_version(signature, file_name):
    """The version named in the trusted comment of a base64 .sig text."""
    try:
        text = base64.b64decode(signature, validate=True).decode("utf-8")
    except (binascii.Error, ValueError) as error:
        raise ManifestError(f"{file_name}.sig is not a base64 signature: {error}") from None
    lines = text.splitlines()
    if len(lines) < 4 or not lines[0].startswith("untrusted comment:"):
        raise ManifestError(f"{file_name}.sig does not hold a minisign signature")
    trusted = next(
        (line[len("trusted comment:"):] for line in lines if line.startswith("trusted comment:")),
        None,
    )
    if trusted is None:
        raise ManifestError(f"{file_name}.sig has no trusted comment")
    for field in re.split(r"[\t ]", trusted.strip()):
        if field.startswith("version:"):
            return field[len("version:"):]
    raise ManifestError(f"{file_name}.sig does not name a version")


def read_signature(dist, file_name, version):
    """The signature of dist/file_name, checked to exist and to name version."""
    path = os.path.join(dist, file_name)
    if not os.path.isfile(path):
        raise ManifestError(f"{path} is missing")
    if os.path.getsize(path) == 0:
        raise ManifestError(f"{path} is empty")
    signature_path = path + ".sig"
    if not os.path.isfile(signature_path):
        raise ManifestError(f"{signature_path} is missing; sign {file_name} first")
    with open(signature_path, encoding="utf-8") as handle:
        signature = handle.read().strip()
    if not signature:
        raise ManifestError(f"{signature_path} is empty")
    named = signed_version(signature, file_name)
    try:
        named = normalize_version(named)
    except ManifestError:
        pass
    if named != version:
        raise ManifestError(
            f"{signature_path} is for version {named}, not {version}; sign it again"
        )
    return signature


def build_manifest(version, dist, notes, pub_date, base_url=None):
    """The manifest as a dict; raises ManifestError for any missing piece."""
    version = normalize_version(version)
    base_url = (base_url or f"{RELEASES}/v{version}").rstrip("/")
    platforms = {}
    problems = []
    for key, file_name in platform_files(version):
        try:
            signature = read_signature(dist, file_name, version)
        except ManifestError as error:
            problems.append(f"{key}: {error}")
            continue
        platforms[key] = {"url": f"{base_url}/{file_name}", "signature": signature}
    if problems:
        raise ManifestError("\n".join(problems))
    return {
        "version": version,
        "notes": notes,
        "pub_date": pub_date,
        "platforms": platforms,
    }


def now_rfc3339():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_manifest(manifest, output):
    """Writes the manifest through a temporary file, so a failure leaves none."""
    folder = os.path.dirname(os.path.abspath(output))
    os.makedirs(folder, exist_ok=True)
    temporary = output + ".tmp"
    with open(temporary, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(manifest, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    os.replace(temporary, output)


def main(argv=None):
    parser = argparse.ArgumentParser(description="write the updater's latest.json")
    parser.add_argument("--version", required=True)
    parser.add_argument("--dist", default=os.path.join(REPO, "dist"))
    parser.add_argument(
        "--notes-file",
        help="release notes (default docs/release/<version>-release-notes.md when it exists)",
    )
    parser.add_argument("--pub-date", help="publication time, RFC 3339 (default: now, UTC)")
    parser.add_argument("--base-url", help=f"download folder (default {RELEASES}/v<version>)")
    parser.add_argument("--output", help="manifest path (default <dist>/latest.json)")
    args = parser.parse_args(argv)

    try:
        version = normalize_version(args.version)
        notes_file = args.notes_file
        if notes_file is None:
            default = os.path.join(REPO, "docs", "release", f"{version}-release-notes.md")
            notes_file = default if os.path.isfile(default) else None
        notes = None
        if notes_file is not None:
            if not os.path.isfile(notes_file):
                raise ManifestError(f"{notes_file} is missing")
            with open(notes_file, encoding="utf-8") as handle:
                notes = handle.read().strip() or None
        manifest = build_manifest(
            version, args.dist, notes, args.pub_date or now_rfc3339(), args.base_url
        )
    except ManifestError as error:
        print(f"updater-manifest: {error}", file=sys.stderr)
        print("updater-manifest: latest.json was not written", file=sys.stderr)
        return 1
    output = args.output or os.path.join(args.dist, "latest.json")
    write_manifest(manifest, output)
    print(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
