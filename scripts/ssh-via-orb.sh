#!/bin/sh
# COOLIFY_RS_SSH_BINARY for dev instances, used when this Mac has no route to the OrbStack VM: the
# script coolify-rs sends on stdin reaches the test VM through `orb` instead of ssh.
while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do shift; done
shift
host=$1
if [ "$host" != "coolify-rs-e2e.orb.local" ]; then
    echo "ssh-via-orb: only the test VM is reachable, not $host" >&2
    exit 255
fi
exec orb -m coolify-rs-e2e -u root bash -se
