"""The third-party license texts a package must carry next to Shellune's LICENSE.

A build with the native RDP/VNC backends links FreeRDP, LibVNCServer, OpenSSL and
zlib statically. packaging/native/build_native.py copies their license texts into
<native dist>/licenses; the packaging scripts copy that folder into the package as
`third-party/`. A stub build (no native prefix) links none of them and carries none.

The native prefix is the one crates/desktop/build.rs links: SHELLUNE_DESKTOP_NATIVE
when it is set, else target/native/dist.
"""

import os
import shutil
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The files build_native.py writes; a folder missing one of them is incomplete.
LICENSE_FILES = [
    "README.txt",
    "FreeRDP-LICENSE.txt",
    "LibVNCServer-COPYING.txt",
    "OpenSSL-LICENSE.txt",
    "zlib-LICENSE.txt",
]
FOLDER = "third-party"


def native_dist():
    configured = os.environ.get("SHELLUNE_DESKTOP_NATIVE")
    if configured:
        return os.path.abspath(configured)
    return os.path.join(REPO, "target", "native", "dist")


def license_files(require_native):
    """The license files to ship, or [] for a stub build.

    `require_native` makes a missing native prefix an error: a release must not
    quietly ship a binary without RDP and VNC.
    """
    dist = native_dist()
    manifest = os.path.join(dist, "shellune-native.txt")
    if not os.path.isfile(manifest):
        if require_native:
            sys.exit("no native desktop prefix at " + dist + "; run "
                     "packaging/native/build_native.py before packaging a release")
        print("note:   no native desktop prefix at " + dist
              + "; this is a stub build without third-party libraries")
        return []
    folder = os.path.join(dist, "licenses")
    files = [os.path.join(folder, name) for name in LICENSE_FILES]
    missing = [path for path in files if not os.path.isfile(path)]
    if missing:
        sys.exit("third-party license texts missing: " + ", ".join(missing)
                 + "; re-run packaging/native/build_native.py")
    return files


def copy_licenses(destination, require_native):
    """Copies the license files into `destination` (created). Returns the copies."""
    files = license_files(require_native)
    if not files:
        return []
    os.makedirs(destination, exist_ok=True)
    copied = []
    for path in files:
        target = os.path.join(destination, os.path.basename(path))
        shutil.copyfile(path, target)
        copied.append(target)
    print("licenses: " + destination)
    return copied
