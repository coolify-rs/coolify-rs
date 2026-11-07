#!/bin/bash
# Runs on the VM: the documented install command, plus the image file because nothing is published yet.
set -eu
curl -fsSL http://host.docker.internal:8899/install.sh |
    USER=root COOLIFY_RS_CDN=http://host.docker.internal:8899 COOLIFY_RS_REGISTRY=docker.io COOLIFY_RS_IMAGE_FILE="$1" bash
