#!/usr/bin/env python3
"""Fixture, screenshots and cleanup of integrations_ui.mjs:
    COOLIFY_RS_BASE=http://127.0.0.1:18091 python3 compat/ui/integrations_ui_run.py
Tokens and links are rows only; no page here asks a provider for anything."""
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "diff"))
from integrations_diff import ENVIRONMENT, PROJECT, cleanup, create_app, login, ref_create_token, sql  # noqa: E402

CASES = [
    ("doppler-service", "doppler", "dp.st.fixture", {}, None),
    ("doppler-account", "doppler", "dp.sa.fixture", {}, {"project": "proj", "config": "prd"}),
    ("infisical", "infisical", "fixture", {"client_id": "c", "base_url": "https://infisical.example.test"}, {"project_id": "p", "environment": "prod", "secret_path": "/x"}),
    ("vault", "vault", "fixture", {"base_url": "https://vault.example.test"}, {"mount": "secret", "path": "app/x"}),
]


def main():
    ui = login()
    apps = []
    for name, provider, token, meta, settings in CASES:
        r = ref_create_token(provider, f"ui-{name}", token, ["secrets"], meta)
        uuid, app_id = create_app(f"ui-int-{name}")
        code, body = ui("PUT", f"/ui/resources/application/{uuid}/secret-manager", {"integration_token_uuid": r["uuid"]})
        assert code == 200, body
        if settings:
            sql(f"UPDATE secret_manager_links SET settings = '{json.dumps(settings)}'::json WHERE resourceable_type = 'App\\Models\\Application' AND resourceable_id = {app_id}")
        apps.append({"name": name, "uuid": uuid})
    uuid, app_id = create_app("ui-int-empty")
    apps.append({"name": "unlinked", "uuid": uuid})
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump({"project": PROJECT, "environment": ENVIRONMENT, "apps": apps}, f)
    rs_port = os.environ.get("COOLIFY_RS_BASE", "http://127.0.0.1:18091").rsplit(":", 1)[1]
    out = subprocess.run(["node", "integrations_ui.mjs", f"--rs={rs_port}", f"--fixture={f.name}"], cwd=HERE, capture_output=True, text=True, timeout=300)
    print(out.stdout[-1500:], out.stderr[-500:])
    names = out.stdout.strip().splitlines()[-1].split() if out.stdout.strip() else []
    subprocess.run(["python3", "pair.py", *names], cwd=HERE, check=False)
    os.unlink(f.name)


if __name__ == "__main__":
    import signal
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda *_: sys.exit(1))
    try:
        main()
    finally:
        cleanup()
