"""Benchmark the two stacks from compat/bench/stack.sh: startup, idle cost and API load.

    compat/bench/stack.sh up
    python3 compat/bench/bench.py [--quick]

Startup: the application container is stopped and started START_RUNS times, timed to the first healthy
response. Idle: both stacks run untouched for IDLE_SECONDS; memory is sampled with `docker stats` and CPU is
read from each container's cgroup. Load: k6 runs every endpoint at every concurrency, REPEATS times, on its
own CPU cores inside the internal network. CPU per request is the stack's cgroup CPU time (application,
database, Redis) during the run divided by the requests served.
Writes results.json next to this script and prints a summary."""
import json
import os
import statistics
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
QUICK = "--quick" in sys.argv
STACKS = {
    "coolify": {"app": "bench-coolify", "parts": ["bench-coolify", "bench-coolify-db", "bench-coolify-redis"]},
    "coolify-rs": {"app": "bench-rs", "parts": ["bench-rs", "bench-rs-db"]},
}
ENDPOINTS = ["/api/v1/version", "/api/v1/projects", "/api/v1/servers", "/api/v1/databases", "/api/v1/applications"]
CONCURRENCY = [1, 10, 50]
START_RUNS = 2 if QUICK else 5
IDLE_SECONDS = 30 if QUICK else 600
REPEATS = 1 if QUICK else 3
DURATION = "5s" if QUICK else "20s"
WARMUP = "3s"
K6_CPUS = "8-15"
NET = "coolify-rs-bench"

tokens_json = json.load(open(os.path.join(ROOT, "compat/reference/secrets/tokens.json")))
TOKENS = ",".join(tokens_json[k] for k in ["acme_owner_root", "acme_owner_read", "acme_owner_read_write"])


def sh(*args, check=True):
    return subprocess.run(args, check=check, capture_output=True, text=True).stdout


def healthy(app):
    return subprocess.run(
        ["docker", "exec", app, "curl", "-sf", "-o", "/dev/null", "http://127.0.0.1:8080/api/health"],
        capture_output=True,
    ).returncode == 0


def cpu_usec(container):
    stat = sh("docker", "exec", container, "cat", "/sys/fs/cgroup/cpu.stat")
    return int(next(line.split()[1] for line in stat.splitlines() if line.startswith("usage_usec")))


def mem_mib(containers):
    out = sh("docker", "stats", "--no-stream", "--format", "{{.Name}} {{.MemUsage}}", *containers)
    units = {"B": 1 / 1048576, "KiB": 1 / 1024, "MiB": 1, "GiB": 1024}
    result = {}
    for line in out.splitlines():
        name, usage = line.split(" ", 1)
        value = usage.split("/")[0].strip()
        number = value.rstrip("KMGiB")
        result[name] = float(number) * units[value[len(number):]]
    return result


def wait_healthy(app, timeout=300):
    end = time.time() + timeout
    while not healthy(app):
        if time.time() > end:
            raise SystemExit(f"{app} did not become healthy")
        time.sleep(0.5)


def startup():
    out = {}
    for name, s in STACKS.items():
        times = []
        for _ in range(START_RUNS):
            sh("docker", "stop", s["app"])
            t = time.perf_counter()
            sh("docker", "start", s["app"])
            while not healthy(s["app"]):
                time.sleep(0.02)
            times.append(time.perf_counter() - t)
            # Let workers finish starting before the next stop.
            time.sleep(15)
        out[name] = {"runs_s": times, "median_s": statistics.median(times)}
        print(f"startup {name:10} median {statistics.median(times):.2f} s  runs {[round(x, 2) for x in times]}",
              flush=True)
    return out


def idle():
    print(f"idle: settling 120 s, then sampling {IDLE_SECONDS} s", flush=True)
    time.sleep(0 if QUICK else 120)
    parts = [p for s in STACKS.values() for p in s["parts"]]
    cpu0 = {p: cpu_usec(p) for p in parts}
    samples = {p: [] for p in parts}
    t0 = time.time()
    while time.time() - t0 < IDLE_SECONDS:
        for p, v in mem_mib(parts).items():
            samples[p].append(v)
        time.sleep(5)
    elapsed = time.time() - t0
    cpu = {p: (cpu_usec(p) - cpu0[p]) / 1e6 / elapsed for p in parts}
    out = {}
    for name, s in STACKS.items():
        out[name] = {
            p: {"mem_median_mib": statistics.median(samples[p]), "mem_max_mib": max(samples[p]),
                "cpu_cores": cpu[p]}
            for p in s["parts"]
        }
        total = sum(statistics.median(samples[p]) for p in s["parts"])
        print(f"idle {name:10} memory {total:7.1f} MiB (" +
              ", ".join(f"{p} {statistics.median(samples[p]):.1f}" for p in s["parts"]) +
              f")  CPU {sum(cpu[p] for p in s['parts']) * 100:.2f}% of a core", flush=True)
    return out


def k6(app, endpoint, vus, duration, summary):
    args = ["docker", "run", "--rm", "--network", NET, "--cpuset-cpus", K6_CPUS, "-v", f"{HERE}:/bench",
            "-e", f"BASE=http://{app}:8080", "-e", f"ENDPOINT={endpoint}", "-e", f"VUS={vus}",
            "-e", f"DURATION={duration}", "-e", "TOKENS", "grafana/k6:0.57.0", "run", "--quiet",
            "--summary-export", f"/bench/{summary}", "/bench/load.js"]
    subprocess.run(args, check=True, capture_output=True, env={**os.environ, "TOKENS": TOKENS})
    path = os.path.join(HERE, summary)
    data = json.load(open(path))
    os.remove(path)
    return data["metrics"]


def load():
    out = []
    for endpoint in ENDPOINTS:
        for vus in CONCURRENCY:
            for name, s in STACKS.items():
                runs = []
                for _ in range(REPEATS):
                    k6(s["app"], endpoint, vus, WARMUP, ".warmup.json")
                    peak = {"mib": 0.0}
                    stop = threading.Event()

                    def sample():
                        while not stop.is_set():
                            peak["mib"] = max(peak["mib"], mem_mib([s["app"]])[s["app"]])

                    sampler = threading.Thread(target=sample)
                    sampler.start()
                    cpu0 = sum(cpu_usec(p) for p in s["parts"])
                    m = k6(s["app"], endpoint, vus, DURATION, ".run.json")
                    cpu = (sum(cpu_usec(p) for p in s["parts"]) - cpu0) / 1000
                    stop.set()
                    sampler.join()
                    d, reqs, checks = m["http_req_duration"], m["http_reqs"], m["checks"]
                    if checks["fails"]:
                        raise SystemExit(f"{name} {endpoint}: {checks['fails']} responses were not 200")
                    runs.append({"rps": reqs["rate"], "requests": reqs["count"], "med_ms": d["med"],
                                 "p95_ms": d["p(95)"], "p99_ms": d["p(99)"], "cpu_ms_per_req": cpu / reqs["count"],
                                 "peak_mib": peak["mib"]})
                row = {"endpoint": endpoint, "vus": vus, "stack": name, "runs": runs}
                for k in ["rps", "med_ms", "p95_ms", "p99_ms", "cpu_ms_per_req", "peak_mib"]:
                    row[k] = statistics.median(r[k] for r in runs)
                out.append(row)
                spread = (max(r["rps"] for r in runs) - min(r["rps"] for r in runs)) / row["rps"] * 100
                print(f"{endpoint:22} {vus:3} VUs {name:10} {row['rps']:8.0f} req/s (±{spread:3.0f}%)  "
                      f"p50 {row['med_ms']:7.2f}  p95 {row['p95_ms']:7.2f}  p99 {row['p99_ms']:7.2f} ms  "
                      f"CPU {row['cpu_ms_per_req']:6.2f} ms/req  peak {row['peak_mib']:6.0f} MiB", flush=True)
    return out


for s in STACKS.values():
    wait_healthy(s["app"])
results = {"quick": QUICK, "startup": startup()}
for s in STACKS.values():
    wait_healthy(s["app"])
results["idle"] = idle()
results["load"] = load()
json.dump(results, open(os.path.join(HERE, "results-quick.json" if QUICK else "results.json"), "w"), indent=1)
