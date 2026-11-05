#!/usr/bin/env bash
# Runs Shellune's SSH integration tests against a real OpenSSH server.
#
# The tests in tests/ssh_openssh.rs start and stop disposable servers themselves
# through scripts/ssh-test-server.sh, so this script exists to run them the way a
# reviewer would: show which OpenSSH is being used, run the file with one test at a
# time (so a failure names the case that failed), and stop any server left behind.
#
# Requirements:
#   * Windows: WSL with Debian, where this script installs nothing. The disposable
#     server is /usr/sbin/sshd from the distribution, started with its own host key,
#     its own user and its own authorized_keys.
#   * Any other host: the tests need a disposable OpenSSH server started the same
#     way; without one they print why they cannot run and claim nothing.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

echo "== OpenSSH under WSL =="
MSYS_NO_PATHCONV=1 wsl.exe -d Debian -u root -- /usr/sbin/sshd -V 2>&1 | head -3 || true

echo
echo "== cargo test --test ssh_openssh =="
cargo test --test ssh_openssh -- --test-threads=1 --nocapture
status=$?

echo
echo "== stopping disposable servers =="
for work in target/ssh-harness/shared target/ssh-harness/changed; do
    absolute="$repo_root/$work"
    MSYS_NO_PATHCONV=1 wsl.exe -d Debian -u root -- bash \
        "$repo_root/scripts/ssh-test-server.sh" stop \
        "$(printf '%s' "$absolute" | sed 's|^[A-Za-z]:|/mnt/&|; s|/mnt/\([A-Za-z]\)|/mnt/\L\1|; s|\\\\|/|g')" \
        >/dev/null 2>&1 || true
done

exit $status
