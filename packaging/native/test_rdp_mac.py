#!/usr/bin/env python3
"""Run the MAC state-machine regression against the built static FreeRDP libraries."""
import argparse
import pathlib
import shutil
import tempfile

import build_native as native


def check_patch_application(work):
    with tempfile.TemporaryDirectory(prefix="shellune-mac-patch-") as directory:
        source = pathlib.Path(directory)
        target = source / "libfreerdp/core/connection.c"
        target.parent.mkdir(parents=True)
        shutil.copyfile(work / "src/freerdp/libfreerdp/core/connection.c", target)
        native.apply_freerdp_mac_patch(source)
        native.run(["git", "apply", "--reverse", "--unsafe-paths",
                    "--directory=" + source.as_posix(), str(native.FREERDP_MAC_PATCH)],
                   cwd=native.REPO_ROOT)
        native.apply_freerdp_mac_patch(source)
        native.apply_freerdp_mac_patch(source)  # Re-running must be harmless.
        target.write_text(target.read_text() + "\n/* unexpected edit */\n")
        try:
            native.apply_freerdp_mac_patch(source)
        except SystemExit as error:
            assert "unexpected connection.c" in str(error)
        else:
            raise AssertionError("modified dependency source was accepted")
    print("PASS: patch application, repeat application, unexpected-source rejection")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=pathlib.Path, default=native.REPO_ROOT / "target/native")
    parser.add_argument("--build-tools", type=pathlib.Path, default=native.DEFAULT_BUILD_TOOLS)
    args = parser.parse_args()
    work = args.work.resolve()
    check_patch_application(work)
    dist = work / "dist"
    out = work / "build/mac-test"
    out.mkdir(parents=True, exist_ok=True)
    includes, libraries = [], []
    for line in (dist / "shellune-native.txt").read_text().splitlines():
        if line.startswith("include="):
            includes.append('/I' + native.quoted(pathlib.Path(line[8:])))
        elif line.startswith("lib="):
            libraries.append(native.quoted(dist / "lib" / (line[4:] + ".lib")))
        elif line.startswith("syslib="):
            libraries.append(line[7:] + ".lib")
    includes.append('/I' + native.quoted(work / "src/freerdp/libfreerdp"))
    source = pathlib.Path(__file__).resolve().parent / "tests/rdp_mac.c"
    exe = out / "rdp_mac.exe"
    cert = work / "src/freerdp/libfreerdp/crypto/test/rsa_pkcs1_sha256_cert.pem"
    native.run_msvc(args.build_tools, work, "rdp-mac-test", [
        " ".join(["cl /nologo /std:c11 /MD /W3 /DFREERDP_EXPORTS /DWINPR_EXPORTS",
                  "/DWIN32 /D_WIN32_WINNT=0x0600", *includes, native.quoted(source),
                  '/Fo' + native.quoted(out / "rdp_mac.obj"), '/Fe' + native.quoted(exe),
                  "/link", *libraries]),
        native.quoted(exe) + " " + native.quoted(cert),
    ])


if __name__ == "__main__":
    main()
