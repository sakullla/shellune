#!/usr/bin/env python3
"""Check that every FreeRDP patch applies only to the pinned source.

For each patch in build_native.FREERDP_PATCHES, on a scratch copy of the files it
touches: a fresh application, a repeat application (harmless), and a refusal when
one of the files was edited by anyone else. The copies come from <work>/src/freerdp,
patched or not; reversing the patch first gives the pinned original.

    python packaging/native/test_patches.py
"""
import argparse
import pathlib
import shutil
import tempfile

import build_native as native


def check(work: pathlib.Path, spec: dict) -> None:
    with tempfile.TemporaryDirectory(prefix="shellune-patch-") as directory:
        source = pathlib.Path(directory)
        for name in spec["files"]:
            target = source / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(work / "src/freerdp" / name, target)
        # Whatever state the work tree is in, end at the pinned original.
        native.apply_freerdp_patch(source, spec)
        native.run(["git", "apply", "--reverse", "--unsafe-paths",
                    "--directory=" + source.as_posix(), str(spec["patch"])],
                   cwd=native.REPO_ROOT)
        for name, (original, _) in spec["files"].items():
            assert native.source_digest(source / name) == original, name

        native.apply_freerdp_patch(source, spec)
        native.apply_freerdp_patch(source, spec)  # Re-running must be harmless.
        for name, (_, patched) in spec["files"].items():
            assert native.source_digest(source / name) == patched, name

        first = next(iter(spec["files"]))
        edited = source / first
        edited.write_text(edited.read_text(encoding="utf-8") + "\n/* unexpected edit */\n",
                          encoding="utf-8")
        try:
            native.apply_freerdp_patch(source, spec)
        except SystemExit as error:
            assert "unexpected " + pathlib.PurePosixPath(first).name in str(error), error
        else:
            raise AssertionError(spec["label"] + ": an edited dependency source was accepted")
    print("PASS: " + spec["label"] + " patch: fresh, repeated, edited source refused")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=pathlib.Path, default=native.REPO_ROOT / "target/native")
    args = parser.parse_args()
    work = args.work.resolve()
    for spec in native.FREERDP_PATCHES:
        check(work, spec)
    pin = native.freerdp_patch_pin()
    for spec in native.FREERDP_PATCHES:
        assert spec["pin"] + "=" + native.sha256_of(spec["patch"]) in pin, pin
    print("PASS: the build pin names every patch: " + pin)


if __name__ == "__main__":
    main()
