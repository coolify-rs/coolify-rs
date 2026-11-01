#!/bin/bash
# Runs on the coolify-rs VM: the install command a second time. Secrets and data must survive.
set -eu
curl -fsSL http://host.docker.internal:8899/install.sh | USER=root bash
