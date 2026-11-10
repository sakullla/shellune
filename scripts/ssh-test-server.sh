#!/usr/bin/env bash
# Disposable OpenSSH server for Shellune's SSH integration tests (Linux side).
#
# Usage, as root inside a Linux environment (WSL is enough; scripts/ssh-integration.sh
# is the Windows caller):
#
#   scripts/ssh-test-server.sh start <work-dir> <port>
#   scripts/ssh-test-server.sh rotate <work-dir> <port>
#   scripts/ssh-test-server.sh stop  <work-dir>
#   scripts/ssh-test-server.sh print-identity <work-dir>
#
# "rotate" throws the host key away and starts again, which is how a test
# reproduces a server whose identity changed behind the user's back.
#
# The server is deliberately disposable: one throw-away host key, one throw-away
# user, one throw-away client key and a passphrase-protected copy of it. Nothing
# here touches the machine's own sshd configuration, its authorized_keys files or
# its host keys, so a test can regenerate the host key to reproduce a changed
# server identity without disturbing anything else.
set -euo pipefail

ACTION="${1:-start}"
WORK="${2:?the work directory is required}"
PORT="${3:-2222}"
# One user per work directory, so two test servers started from two directories do
# not overwrite each other's authorized_keys. The name is printed by "start" and
# parsed by the tests; it is derived here so both sides always agree.
TEST_USER="shellune-$(basename "$WORK" | tr -cd 'a-z0-9-' | cut -c1-12)"
TEST_PASSWORD="shellune-test-password"

prepare_user() {
    if ! id "$TEST_USER" >/dev/null 2>&1; then
        useradd -m -s /bin/bash "$TEST_USER"
    fi
    # A fixed, documented password: the tests have to try a wrong one and the right
    # one, and the server only ever listens where the harness points it.
    echo "$TEST_USER:$TEST_PASSWORD" | chpasswd
}

write_config() {
    cat >"$WORK/sshd_config" <<CONFIG
Port $PORT
ListenAddress 0.0.0.0
HostKey $WORK/hostkey
PidFile $WORK/sshd.pid
PasswordAuthentication yes
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitRootLogin no
AllowUsers $TEST_USER
StrictModes no
PrintMotd no
PrintLastLog no
# Jump tests open direct-tcpip through this server. Forwarding is explicit so a
# distro default cannot turn a two-hop connection into a refusal.
AllowTcpForwarding yes
PermitOpen any
PermitListen any
# X11 forwarding: the display tests ask this server for a channel back to the
# client, so the server itself has to allow it and to have xauth to prepare the
# remote DISPLAY. A distro default of "no" would otherwise look like a client bug.
X11Forwarding yes
XAuthLocation /usr/bin/xauth
MaxSessions 50
MaxStartups 50:30:200
# A test deliberately tries a wrong password and then the right key on the same
# host. OpenSSH's per-source penalty would otherwise reject the second attempt for
# a few seconds, which would look like a broken retry instead of rate limiting.
PerSourcePenalties no
LogLevel VERBOSE
Subsystem sftp internal-sftp
CONFIG
    chmod 600 "$WORK/sshd_config"
}

start() {
    mkdir -p "$WORK"
    chmod 700 "$WORK"
    prepare_user

    if [ ! -f "$WORK/hostkey" ]; then
        ssh-keygen -q -t ed25519 -N "" -C shellune-test-host -f "$WORK/hostkey"
    fi
    if [ ! -f "$WORK/client_key" ]; then
        ssh-keygen -q -t ed25519 -N "" -C shellune-test-client -f "$WORK/client_key"
        # The same key again, protected by a passphrase, so "key with passphrase"
        # exercises real decryption instead of a different code path.
        cp "$WORK/client_key" "$WORK/client_key_enc"
        chmod 600 "$WORK/client_key_enc"
        ssh-keygen -q -p -P "" -N "shellune-key-passphrase" -f "$WORK/client_key_enc"
    fi

    install -d -m 700 -o "$TEST_USER" -g "$TEST_USER" "/home/$TEST_USER/.ssh"
    install -m 600 -o "$TEST_USER" -g "$TEST_USER" \
        "$WORK/client_key.pub" "/home/$TEST_USER/.ssh/authorized_keys"

    write_config
    /usr/sbin/sshd -t -f "$WORK/sshd_config"
    if [ -f "$WORK/sshd.pid" ] && kill -0 "$(cat "$WORK/sshd.pid")" 2>/dev/null; then
        kill "$(cat "$WORK/sshd.pid")" || true
        sleep 0.3
    fi
    /usr/sbin/sshd -f "$WORK/sshd_config" -E "$WORK/sshd.log"

    for _ in $(seq 1 50); do
        if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
            echo "listening on 0.0.0.0:$PORT"
            identity
            return 0
        fi
        sleep 0.1
    done
    echo "sshd did not start; log follows" >&2
    cat "$WORK/sshd.log" >&2 || true
    return 1
}

# The listener and every session it forked. Killing only the pid file leaves
# those sessions running, so a jump test would not see the hop drop.
kill_descendants() {
    local pid="$1"
    local child
    local children
    children="$(ps -o pid= --ppid "$pid" 2>/dev/null || true)"
    for child in $children; do
        kill_descendants "$child"
    done
    kill -KILL "$pid" 2>/dev/null || true
}

stop() {
    if [ -f "$WORK/sshd.pid" ]; then
        local pid
        pid="$(cat "$WORK/sshd.pid" 2>/dev/null || true)"
        if [ -n "${pid:-}" ]; then
            kill_descendants "$pid"
        fi
        rm -f "$WORK/sshd.pid"
    fi
}

# What a caller needs to reach this server, in one key=value block.
identity() {
    echo "port=$PORT"
    echo "user=$TEST_USER"
    echo "password=$TEST_PASSWORD"
    echo "fingerprint=$(ssh-keygen -lf "$WORK/hostkey.pub" | awk '{print $2}')"
    echo "host_key=$WORK/hostkey"
    echo "client_key=$WORK/client_key"
    echo "client_key_encrypted=$WORK/client_key_enc"
    echo "client_key_passphrase=shellune-key-passphrase"
}

case "$ACTION" in
start) start ;;
rotate)
    # A new host key means a new server identity on the same address, which is
    # exactly the situation that must not be resolved by connecting anyway.
    rm -f "$WORK/hostkey" "$WORK/hostkey.pub"
    start
    ;;
stop) stop ;;
print-identity) identity ;;
*)
    echo "unknown action: $ACTION" >&2
    exit 2
    ;;
esac
