#!/usr/bin/env python3
"""Package a built Linux binary as a tarball with a desktop entry.

Usage:
    python3 packaging/linux/make-tarball.py --binary <path to shellune> \
        [--version 0.2.1] [--out dist]

Result: dist/Shellune-<version>-linux-x86_64.tar.gz containing

    Shellune-<version>-linux-x86_64/shellune
    Shellune-<version>-linux-x86_64/LICENSE
    Shellune-<version>-linux-x86_64/shellune.desktop
    Shellune-<version>-linux-x86_64/README.txt

Nothing is signed. The tarball has never been produced on the verification machine
(Windows), so Linux packaging is scaffolding rather than evidence.
"""

import argparse
import hashlib
import os
import shutil
import sys
import tarfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

README = """Shellune
========

连接远程，也专注本地。
A modern terminal for local shells, SSH, and SFTP.

Version @@V@@ for Linux x86_64.

Install
-------
    install -Dm755 shellune ~/.local/bin/shellune
    install -Dm644 shellune.desktop ~/.local/share/applications/shellune.desktop
    install -Dm644 shellune.png ~/.local/share/icons/hicolor/1024x1024/apps/shellune.png

Run it with the shellune command. A Vulkan 1.3 driver and an X11 or Wayland session
are what the GPUI renderer needs.
"""


def main():
    parser = argparse.ArgumentParser(description="package Shellune for Linux")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--version", default="0.2.1")
    parser.add_argument("--out", default=os.path.join(REPO, "dist"))
    args = parser.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        sys.exit("binary not found: " + binary)

    stem = "Shellune-{}-linux-x86_64".format(args.version)
    stage = os.path.join(os.path.abspath(args.out), stem)
    if os.path.isdir(stage):
        shutil.rmtree(stage)
    os.makedirs(stage)

    shutil.copy2(binary, os.path.join(stage, "shellune"))
    shutil.copy2(os.path.join(REPO, "assets", "icons", "shellune.png"), stage)
    os.chmod(os.path.join(stage, "shellune"), 0o755)
    shutil.copy2(os.path.join(REPO, "LICENSE"), os.path.join(stage, "LICENSE"))
    shutil.copy2(os.path.join(REPO, "packaging", "linux", "shellune.desktop"),
                 os.path.join(stage, "shellune.desktop"))
    with open(os.path.join(stage, "README.txt"), "w", encoding="utf-8") as f:
        f.write(README.replace("@@V@@", args.version))

    archive = stage + ".tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(stage, arcname=stem)

    h = hashlib.sha256()
    with open(archive, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    with open(archive + ".sha256", "w", encoding="ascii") as f:
        f.write("{}  {}\n".format(h.hexdigest(), os.path.basename(archive)))

    print("archive: " + archive)
    print("sha256:  " + h.hexdigest())


if __name__ == "__main__":
    main()
