#!/usr/bin/env python3
"""Deletes services by uuid through the reference API, as the row and VM cleanup of a comparison run.

services_delete.py <uuid> [uuid ...]; only pass uuids you created."""
import json
import os
import sys
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOKENS = json.load(open(os.path.join(ROOT, "compat/reference/secrets/tokens.json")))

for uuid in sys.argv[1:]:
    url = f"http://127.0.0.1:18000/api/v1/services/{uuid}?delete_configurations=true&delete_volumes=true&docker_cleanup=false&delete_connected_networks=true"
    r = urllib.request.Request(url, method="DELETE")
    r.add_header("Authorization", "Bearer " + TOKENS["acme_owner_root"])
    r.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(r) as resp:
            print(uuid, resp.status, resp.read()[:120])
    except urllib.error.HTTPError as e:
        print(uuid, e.code, e.read()[:200])
