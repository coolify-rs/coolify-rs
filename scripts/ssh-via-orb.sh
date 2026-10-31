#!/bin/sh
# COOLIFY_RS_SSH_BINARY for dev instances, used when this Mac has no route to the OrbStack VM: the
# script coolify-rs sends on stdin reaches the test VM through `orb` instead of ssh.
while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do shift; done
shift
host=$1
case "$host" in
    coolify-rs-e2e.orb.local | coolify-rs-agent-*.orb.local) ;;
    *)
        echo "ssh-via-orb: only the test VMs are reachable, not $host" >&2
        exit 255
        ;;
esac
# coolify-rs runs ssh with its own HOME; orb finds its socket under the real one and hangs without it.
HOME=$(eval echo "~$(id -un)")
export HOME
# compat/ui/server_actions.mjs compares the scripts a VM receives: it creates this file for the length of a run.
log="$HOME/.cache/coolify-rs-dev/ssh-log/$host"
if [ -f "$log" ]; then
    echo "####" >>"$log"
    tee -a "$log" | orb -m "${host%.orb.local}" -u root bash -se
    exit $?
fi
exec orb -m "${host%.orb.local}" -u root bash -se
