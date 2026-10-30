#!/usr/bin/env python3
"""Records the expected results of tests/service_parser.rs on the reference instance."""
import json
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
CONTAINER = "coolify-rs-reference-coolify-1"
REFERENCE = "/var/www/html/tests/Fixtures/parser/service-reference.yaml"
UPSTREAM = os.path.join(HERE, "../../../../../.upstream/snapshot/tests/Fixtures/parser/service-reference.yaml")

DOMAINS = [{"name": "uptime-kuma", "url": "https://kuma.example.test:3001"}, "parse"]
CASES = [
    {"name": "uptime-kuma", "template": "uptime-kuma", "steps": ["parse", "parse", *DOMAINS]},
    {"name": "wordpress-with-mariadb", "template": "wordpress-with-mariadb"},
    {"name": "minio", "template": "budibase"},
    {"name": "logto", "template": "logto"},
    {"name": "garage", "template": "garage"},
    {"name": "pocketbase", "template": "pocketbase"},
    {"name": "appwrite", "template": "appwrite"},
    {"name": "supabase", "template": "supabase"},
    {"name": "reference", "compose_file": "/tmp/service-reference.yaml"},
    {"name": "reference-escape-off", "compose_file": "/tmp/service-reference.yaml", "service": {"is_container_label_escape_enabled": False}},
    {"name": "reference-exact-traefik", "compose_file": "/tmp/service-reference.yaml", "server_settings": {"generate_exact_labels": True}, "proxy_type": "TRAEFIK"},
    {"name": "reference-exact-caddy", "compose_file": "/tmp/service-reference.yaml", "server_settings": {"generate_exact_labels": True}, "proxy_type": "CADDY"},
    {"name": "invalid-yaml", "compose": "services:\n  a: [\n"},
    {"name": "unsafe-name", "compose": "services:\n  'a;b':\n    image: nginx\n"},
    {"name": "comments", "compose": "services:\n  web:\n    image: nginx\n    environment:\n      - A=${A:-x} # first comment\n      - B=${B} # second\n      - SERVICE_URL_WEB_80=/api # path\n"},
]


def run(*args, **kw):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kw).stdout


def main():
    subprocess.run(["docker", "cp", os.path.join(HERE, "harness.php"), f"{CONTAINER}:/tmp/service_parser_harness.php"], check=True)
    subprocess.run(["docker", "cp", UPSTREAM, f"{CONTAINER}:/tmp/service-reference.yaml"], check=True)
    os.makedirs(os.path.join(HERE, "expected"), exist_ok=True)
    for case in CASES:
        path = f"/tmp/service_parser_case_{case['name']}.json"
        subprocess.run(["docker", "exec", "-i", CONTAINER, "sh", "-c", f"cat > {path}"], input=json.dumps(case), text=True, check=True)
        out = run("docker", "exec", CONTAINER, "php", "/tmp/service_parser_harness.php", path)
        start = out.index("{\n")
        with open(os.path.join(HERE, "expected", case["name"] + ".json"), "w") as f:
            f.write(out[start:])
        subprocess.run(["docker", "exec", "-u", "root", CONTAINER, "rm", "-f", path], check=True)
        print("recorded", case["name"])
    subprocess.run(["docker", "exec", "-u", "root", CONTAINER, "rm", "-f", "/tmp/service_parser_harness.php", "/tmp/service-reference.yaml"], check=True)


if __name__ == "__main__":
    main()
