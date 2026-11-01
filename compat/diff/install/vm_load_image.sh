#!/bin/bash
# Runs on the VM: loads a docker save file. This stands in for pulling a newer published image.
set -eu
docker load -i "$1" | tail -1
