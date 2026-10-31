#!/bin/sh
# Creates one fresh OrbStack VM for the clean install comparison and stops it reaching third parties.
#   vm_prepare.sh <machine>      only coolify-rs-agent-install-up or coolify-rs-agent-install-rs
#
# The block list holds what a running Coolify contacts by itself: telemetry (undead), the public IP
# lookup, the template and changelog downloads, and the password breach check at registration.
# docker-compose.custom.yml is the override both installers read. Everything is refused on 127.0.0.1,
# so both sides see the same failures. The hosts file covers commands the scripts run on the host.
set -eu
VM="$1"
case "$VM" in
coolify-rs-agent-install-up | coolify-rs-agent-install-rs) ;;
*)
    echo "refusing machine $VM" >&2
    exit 1
    ;;
esac

if orb list -q 2>/dev/null | grep -qx "$VM"; then
    echo "machine $VM already exists; delete it first for a clean run" >&2
    exit 1
fi
orb create ubuntu:noble "$VM" >/dev/null
BLOCKED="undead.coolify.io ifconfig.io cdn.coollabs.io api.github.com raw.githubusercontent.com github.com api.pwnedpasswords.com"

orb -m "$VM" -u root bash -s "$BLOCKED" <<'EOF'
set -eu
for host in $1; do echo "127.0.0.1 $host" >>/etc/hosts; done
export DEBIAN_FRONTEND=noninteractive
apt-get update -y -qq >/dev/null
apt-get install -y -qq curl >/dev/null
# The installer creates /data/coolify with these owner and mode when it is missing.
mkdir -p /data/coolify
chown 9999:root /data/coolify
chmod 700 /data/coolify
mkdir -p /data/coolify/source
{
    echo "services:"
    echo "  coolify:"
    echo "    extra_hosts:"
    for host in $1; do echo "      - $host:127.0.0.1"; done
} >/data/coolify/source/docker-compose.custom.yml
EOF
echo "$VM ready"
