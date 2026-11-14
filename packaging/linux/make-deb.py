#!/usr/bin/env python3
"""Package a built Linux binary as a deb.

Usage:
    python3 packaging/linux/make-deb.py --binary <path to shellune> \
        [--version 0.4.4] [--arch amd64] [--out dist]

Result: dist/shellune_<version>_<arch>.deb, which installs /usr/bin/shellune,
the desktop entry and the hicolor icon. dpkg-shlibdeps computes Depends from the
binary, so the build host must have the runtime libraries it links installed --
the same packages the release workflow installs for the build headers. Requires
dpkg-deb and dpkg-shlibdeps (dpkg-dev). Nothing is signed.
"""

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

HOMEPAGE = "https://github.com/sakullla/shellune"
DEFAULT_MAINTAINER = "sakullla <sakullla@users.noreply.github.com>"
DESCRIPTION = "Modern terminal for local shells, SSH, and SFTP"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(command):
    print("$ " + " ".join(command))
    subprocess.run(command, check=True)


def shlib_depends(binary):
    # dpkg-shlibdeps reads debian/control from the directory it runs in, which a
    # package built outside a source tree does not have; an empty one is enough.
    work = tempfile.mkdtemp(prefix="shellune-shlibdeps-")
    os.makedirs(os.path.join(work, "debian"))
    open(os.path.join(work, "debian", "control"), "w").close()
    try:
        proc = subprocess.run(["dpkg-shlibdeps", "-O", binary],
                              capture_output=True, text=True, cwd=work)
    except FileNotFoundError:
        sys.exit("dpkg-shlibdeps not found; install dpkg-dev (Linux only)")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if proc.returncode != 0:
        sys.exit("dpkg-shlibdeps failed on " + binary + ":\n" + proc.stderr)
    for line in proc.stdout.splitlines():
        if line.startswith("shlibs:Depends="):
            return line.split("=", 1)[1].strip()
    sys.exit("dpkg-shlibdeps printed no dependency line for " + binary)


def main():
    parser = argparse.ArgumentParser(description="package Shellune as a deb")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--version", default="0.4.4")
    parser.add_argument("--arch", default="amd64")
    parser.add_argument("--maintainer", default=DEFAULT_MAINTAINER)
    parser.add_argument("--out", default=os.path.join(REPO, "dist"))
    args = parser.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        sys.exit("binary not found: " + binary)

    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    stage = os.path.join(out, "deb-staging")
    if os.path.isdir(stage):
        shutil.rmtree(stage)

    bin_dir = os.path.join(stage, "usr", "bin")
    share = os.path.join(stage, "usr", "share")
    os.makedirs(bin_dir)
    os.makedirs(os.path.join(share, "applications"))
    os.makedirs(os.path.join(share, "icons", "hicolor", "1024x1024", "apps"))
    os.makedirs(os.path.join(share, "doc", "shellune"))

    target = os.path.join(bin_dir, "shellune")
    shutil.copy2(binary, target)
    os.chmod(target, 0o755)
    shutil.copy2(os.path.join(REPO, "packaging", "linux", "shellune.desktop"),
                 os.path.join(share, "applications", "shellune.desktop"))
    shutil.copy2(os.path.join(REPO, "assets", "icons", "shellune.png"),
                 os.path.join(share, "icons", "hicolor", "1024x1024", "apps", "shellune.png"))
    shutil.copy2(os.path.join(REPO, "LICENSE"),
                 os.path.join(share, "doc", "shellune", "copyright"))

    depends = shlib_depends(target)
    control = (
        "Package: shellune\n"
        "Version: {version}\n"
        "Section: utils\n"
        "Priority: optional\n"
        "Architecture: {arch}\n"
        "Maintainer: {maintainer}\n"
        "Depends: {depends}\n"
        "Homepage: {homepage}\n"
        "Description: {description}\n"
        " 连接远程，也专注本地。\n"
    ).format(version=args.version, arch=args.arch, maintainer=args.maintainer,
             depends=depends, homepage=HOMEPAGE, description=DESCRIPTION)
    os.makedirs(os.path.join(stage, "DEBIAN"))
    with open(os.path.join(stage, "DEBIAN", "control"), "w", encoding="utf-8") as f:
        f.write(control)

    deb = os.path.join(out, "shellune_{}_{}.deb".format(args.version, args.arch))
    run(["dpkg-deb", "--root-owner-group", "--build", stage, deb])
    shutil.rmtree(stage)

    digest = sha256_file(deb)
    with open(deb + ".sha256", "w", encoding="ascii") as f:
        f.write("{}  {}\n".format(digest, os.path.basename(deb)))

    print("package: " + deb)
    print("depends: " + depends)
    print("sha256:  " + digest)


if __name__ == "__main__":
    main()
