"""API latency and throughput, Coolify vs coolify-rs, on the same database with the same token.

    python3 compat/bench/api_bench.py compat/reference/secrets/tokens.json:acme_owner_root \
        coolify=http://127.0.0.1:18000 coolify-rs=http://127.0.0.1:18095

Per endpoint and target: 20 warm-up requests, 200 sequential requests on one keep-alive connection
(p50/p95/p99), then 2000 requests over 16 keep-alive connections for throughput. Every response must
be 200, so an error page is never timed. Coolify limits its API to API_RATE_LIMIT (200) requests a
minute per user; raise it on the reference first, or its throughput numbers measure the limiter.
Results go to stdout and api_results_<token>.json in the current directory."""
import http.client
import json
import sys
import threading
import time
from urllib.parse import urlparse

ENDPOINTS = [
    "/api/v1/version",
    "/api/v1/teams/current",
    "/api/v1/projects",
    "/api/v1/servers",
    "/api/v1/applications",
    "/api/v1/services",
    "/api/v1/databases",
    "/api/v1/deployments",
]
SEQ, WARM, CONC, TOTAL = 200, 20, 16, 2000

tokens_file, key = sys.argv[1].split(":")
token = json.load(open(tokens_file))[key]
targets = [a.split("=", 1) for a in sys.argv[2:]]


def conn(base):
    u = urlparse(base)
    return http.client.HTTPConnection(u.hostname, u.port, timeout=60)


def get(c, path):
    c.request("GET", path, headers={"Authorization": f"Bearer {token}", "Accept": "application/json"})
    r = c.getresponse()
    body = r.read()
    return r.status, len(body)


def sequential(base, path):
    c = conn(base)
    for _ in range(WARM):
        status, size = get(c, path)
    assert status == 200, (base, path, status)
    times = []
    for _ in range(SEQ):
        t = time.perf_counter()
        status, _ = get(c, path)
        times.append((time.perf_counter() - t) * 1000)
        assert status == 200, (base, path, status)
    times.sort()

    def q(p):
        return times[min(len(times) - 1, int(p * len(times)))]

    return size, q(0.50), q(0.95), q(0.99)


def throughput(base, path):
    per = TOTAL // CONC
    errors = []

    def worker():
        c = conn(base)
        for _ in range(per):
            status, _ = get(c, path)
            if status != 200:
                errors.append(status)

    threads = [threading.Thread(target=worker) for _ in range(CONC)]
    t = time.perf_counter()
    for th in threads:
        th.start()
    for th in threads:
        th.join()
    return per * CONC / (time.perf_counter() - t), len(errors)


results = {}
for path in ENDPOINTS:
    for name, base in targets:
        size, p50, p95, p99 = sequential(base, path)
        rps, errs = throughput(base, path)
        results.setdefault(path, {})[name] = dict(bytes=size, p50=p50, p95=p95, p99=p99, rps=rps, errors=errs)
        print(f"{path:24} {name:10} {size:>8}B  p50 {p50:7.2f}  p95 {p95:7.2f}  p99 {p99:7.2f} ms  "
              f"{rps:6.0f} req/s  errors {errs}", flush=True)
json.dump(results, open(f"api_results_{key}.json", "w"), indent=1)
