#!/bin/sh
# Starts an sshd of its own on 127.0.0.1, for the oracle's syncs over a
# real ssh (tools/oracle, --ssh-config), and writes an ssh client
# configuration that reaches it as host "server" with a key of its own.
#
#   scripts/start-sshd.sh DIR [PORT]     # PORT defaults to 2222
#
# Everything goes in DIR: the host and client keys, sshd_config, the
# client's ssh_config and known_hosts, sshd's log (sshd.log) and its
# process id (sshd.pid). It runs as whoever starts it (root in Docker, the
# runner in CI), who is the only user it lets in. Stop it with
#
#   kill "$(cat DIR/sshd.pid)"
set -eu
dir=${1:?usage: $0 DIR [PORT]}
port=${2:-2222}
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
user=$(id -un)

rm -f "$dir/host_key" "$dir/host_key.pub" "$dir/client_key" "$dir/client_key.pub"
ssh-keygen -q -t ed25519 -N '' -f "$dir/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$dir/client_key"
cp "$dir/client_key.pub" "$dir/authorized_keys"
echo "[127.0.0.1]:$port $(cut -d' ' -f1-2 "$dir/host_key.pub")" > "$dir/known_hosts"

cat > "$dir/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $dir/host_key
AuthorizedKeysFile $dir/authorized_keys
PidFile $dir/sshd.pid
AllowUsers $user
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
PermitRootLogin prohibit-password
LogLevel VERBOSE
EOF

cat > "$dir/ssh_config" <<EOF
Host server
    HostName 127.0.0.1
    Port $port
    User $user
    IdentityFile $dir/client_key
    IdentitiesOnly yes
    UserKnownHostsFile $dir/known_hosts
    StrictHostKeyChecking yes
    BatchMode yes
    LogLevel ERROR
EOF

# sshd wants an absolute path, and as root, its privilege separation
# directory.
[ "$(id -u)" -eq 0 ] && mkdir -p /run/sshd
sshd=$(command -v sshd || echo /usr/sbin/sshd)
"$sshd" -f "$dir/sshd_config" -E "$dir/sshd.log"

# Up once a login works.
i=0
until ssh -F "$dir/ssh_config" server true 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -ge 50 ]; then
        echo "sshd on port $port did not start:" >&2
        cat "$dir/sshd.log" >&2
        exit 1
    fi
    sleep 0.1
done
echo "sshd on 127.0.0.1:$port, configuration $dir/ssh_config"
