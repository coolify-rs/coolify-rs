#!/usr/bin/env python3
"""Differential test of Sentinel metrics, fleet metrics, the dashboard charts and resource analytics.

Both sides read one source: a dedicated VM (compat/diff/metrics_vm.sh) with a real Sentinel, Traefik and
containers, then a stand-in Sentinel (metrics_fake_sentinel.py) whose values depend on the timestamp only.
Rows belong to two users of their own; cleanup deletes exactly the ids seed printed, also after a crash.

    COOLIFY_RS_BASE=http://127.0.0.1:18087 python3 compat/diff/metrics_diff.py          full run
    python3 compat/diff/metrics_diff.py --keep                                          leaves rows for the screens
    python3 compat/diff/metrics_diff.py --cleanup                                       deletes what a run left
    python3 compat/diff/metrics_diff.py --sentinel=real|fake                            switches the VM for compat/ui/metrics_compare.mjs
Add --down to also delete the VM.
"""

import http.cookiejar
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("COOLIFY_RS_BASE", "http://127.0.0.1:18087")
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
STATE = os.path.join(os.environ.get("TMPDIR", "/tmp"), "coolify-metrics-diff.json")
VM = os.path.join(ROOT, "compat/diff/metrics_vm.sh")
PHP = os.path.join(ROOT, "compat/diff/metrics_reference.php")
FAKE = os.path.join(ROOT, "compat/diff/metrics_fake_sentinel.py")
VM_HOST = "coolify-rs-agent-metrics.orb.local"
TOKENS = {"busy": "MetricsStreamBusy0123456789abcdef", "calm": "MetricsStreamCalm0123456789abcdef", "old": "MetricsStreamOld00123456789abcdef", "bad": "MetricsStreamWrong123456789abcdef"}
UUIDS = {"app1": "metricsapp1aaaaaaaaaaaaaa", "app2": "metricsapp2aaaaaaaaaaaaaa", "db": "metricsdb1aaaaaaaaaaaaaaa", "service": "metricssvc1aaaaaaaaaaaaaa"}

results = []


def record(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"\n  {str(detail)[:1800]}"), flush=True)


def php(mode, **args):
    with open(PHP, "rb") as script:
        run = subprocess.run(
            ["docker", "exec", "-i", "-e", f"MODE={mode}", "-e", "ARGS=" + json.dumps(args), "coolify-rs-reference-coolify-1", "php"],
            stdin=script, capture_output=True, timeout=240,
        )
    out = run.stdout.decode(errors="replace")
    if "@@JSON@@" not in out:
        raise RuntimeError(f"{mode}: {out[-1500:]} {run.stderr.decode(errors='replace')[-500:]}")
    return json.loads(out.split("@@JSON@@", 1)[1].splitlines()[0])


class Session:
    """A browser session on coolify-rs: `/ui/session` allows few logins a minute, so one per user."""

    def __init__(self, email):
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        secret = open(os.path.join(ROOT, "compat/reference/secrets/ui-login.txt")).read()
        password = re.search(r"^password:\s*(.+)$", secret, re.M).group(1).strip()
        answer = self.call("POST", "/ui/session", {"email": email, "password": password})
        self.csrf = answer.get("csrf") or answer.get("csrf_token") or ""
        if "csrf" not in answer and "csrf_token" not in answer:
            raise SystemExit("login failed")

    def call(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(BASE + path, data=data, method=method)
        request.add_header("Accept", "application/json")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        if method != "GET" and getattr(self, "csrf", ""):
            request.add_header("X-CSRF-Token", self.csrf)
        try:
            with self.opener.open(request, timeout=120) as response:
                return json.loads(response.read() or b"null")
        except urllib.error.HTTPError as e:
            return {"__status": e.code, "__body": e.read().decode(errors="replace")[:300]}


def close(a, b):
    """Numbers are equal when both sides print the same value at 10 significant digits."""
    if isinstance(a, bool) or isinstance(b, bool):
        return a is b
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return f"{float(a):.10g}" == f"{float(b):.10g}"
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(close(x, y) for x, y in zip(a, b))
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(close(a[k], b[k]) for k in a)
    return a == b


def first_difference(a, b, path="$"):
    if isinstance(a, dict) and isinstance(b, dict):
        if a.keys() != b.keys():
            return f"{path}: keys {sorted(set(a) ^ set(b))}"
        for key in a:
            found = first_difference(a[key], b[key], f"{path}.{key}")
            if found:
                return found
        return None
    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            return f"{path}: length ref {len(a)} rs {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            found = first_difference(x, y, f"{path}[{i}]")
            if found:
                return found
        return None
    return None if close(a, b) else f"{path}: ref {a!r} rs {b!r}"


def same(name, ref, rs):
    found = first_difference(ref, rs)
    record(name, found is None, found)


def minute():
    return int(time.time()) // 60


def in_one_minute(read):
    """The stand-in's data moves on the full minute. A read across that border is read again; that is
    the moving window of the data, never a tolerance on a value."""
    for _ in range(4):
        # Start early enough in the minute for the slower side to finish within it.
        if time.time() % 60 > 25:
            time.sleep(61 - time.time() % 60)
        start = minute()
        values = read()
        if minute() == start:
            return values
    raise RuntimeError("could not read within one minute")


def touch(state):
    ids = state["server_ids"]
    php(
        "touch",
        ids=list(ids.values()),
        running=[UUIDS["app1"], UUIDS["app2"], UUIDS["db"], "metricsappcompose00000000", "metricsappmetricsoff00000"],
        reachable=[v for k, v in ids.items() if k != "offline"],
    )


def seed():
    state = php("seed", vm=VM_HOST, tokens=TOKENS, uuids=UUIDS)
    with open(STATE, "w") as f:
        json.dump(state, f)
    if "seed_error" in state:
        cleanup()
        raise SystemExit("seed failed: " + state["seed_error"])
    return state


def cleanup():
    if os.path.exists(STATE):
        state = json.load(open(STATE))
        if state.get("deployment"):
            php("deployment_off", id=state["deployment"])
        php("cleanup", ids=state["ids"])
        os.remove(STATE)
        print("cleaned up seeded rows")


def real_phase(state, rs):
    """A real Sentinel, Traefik and containers. Raw samples never change, so a timestamp both sides saw
    must carry one value; the ends of the windows differ by the seconds between the reads."""
    labels = state["labels"]
    env = dict(os.environ, APP1_ID=str(labels["app1"]), APP2_ID=str(labels["app2"]), DB_ID=str(labels["db"]), SERVICE_ID=str(labels["service"]), SERVICE_APP_ID=str(labels["service_app"]))
    subprocess.run([VM, "up", TOKENS["busy"]], check=True, capture_output=True, env=env)
    subprocess.run([VM, "run"], input=b"sleep 15", check=True)
    subprocess.run([VM, "traffic"], check=True)
    touch(state)
    # Sentinel flushes the access log in batches.
    time.sleep(75)
    if rs is None:
        return
    busy = state["servers"]["busy"]
    who = {"user": state["user"], "team": state["team"]}

    def overlap(name, ref, mine):
        ref_points, rs_points = dict(map(tuple, ref or [])), dict(map(tuple, mine or []))
        shared = set(ref_points) & set(rs_points)
        bad = [t for t in shared if not close(ref_points[t], rs_points[t])]
        record(f"real {name}: {len(shared)} shared samples", len(shared) > 0 and not bad, f"ref {len(ref_points)} rs {len(rs_points)} differing {bad[:3]}")

    ref = php("server_series", uuid=busy, mins=5, **who)
    mine = rs.call("GET", f"/ui/server-page/{busy}/metrics/data?interval=5")
    for key in ["cpuSeries", "memorySeries", "loadSeries", "networkRxSeries", "networkTxSeries"]:
        overlap(f"server {key}", ref[key], mine[key])
    # This Sentinel reports no root mount on this VM, so both sides have an empty series.
    same("real server diskSeries", ref["diskSeries"], mine["diskSeries"])
    for kind, uuid in [("application", UUIDS["app1"]), ("database", UUIDS["db"])]:
        ref = php("resource_series", kind=kind, uuid=uuid, mins=5, **who)
        mine = rs.call("GET", f"/ui/resources/{kind}/{uuid}/metrics/data?interval=5")
        overlap(f"{kind} cpu", ref["cpu"], mine["cpu"])
        overlap(f"{kind} memory", ref["memory"], mine["memory"])
    ref = php("dashboard_chart", uuid=busy, **who)
    mine = rs.call("GET", f"/ui/dashboard/server-metrics/{busy}")
    overlap("dashboard cpu", ref["cpu"], mine["cpu"])
    overlap("dashboard memory", ref["memory"], mine["memory"])

    def traffic_same(name, ref, mine):
        ref = {k: v for k, v in ref.items()}
        # The globe and table derivations are `shape_common`'s, compared by analytics_compare.mjs.
        same(name, ref, {k: mine.get(k) for k in ref})

    for range_ in ["24h", "7d"]:
        traffic_same(f"real dashboard traffic {range_}", php("dashboard_traffic", range=range_, **who), rs.call("GET", f"/ui/dashboard/traffic?range={range_}"))
        for kind, uuid in [("application", UUIDS["app1"]), ("service", UUIDS["service"])]:
            ref = php("resource_analytics", kind=kind, uuid=uuid, range=range_, **who)
            mine = rs.call("GET", f"/ui/resources/{kind}/{uuid}/analytics?range={range_}")
            mine["breakdowns"] = {k: [{f: row.get(f) for f in ("value", "requests", "bytesOut")} for row in rows] for k, rows in (mine.get("breakdowns") or {}).items()}
            ref["breakdowns"] = {k: [{f: row.get(f) for f in ("value", "requests", "bytesOut")} for row in rows] for k, rows in (ref.get("breakdowns") or {}).items()}
            mine.pop("device", None)
            ref.pop("device", None)
            traffic_same(f"real {kind} analytics {range_}", ref, mine)
    for kind, uuid in [("application", "metricsappmetricsoff00000"), ("service", "metricssvcoff000000000000")]:
        traffic_same(f"{kind} analytics off", php("resource_analytics", kind=kind, uuid=uuid, **who), rs.call("GET", f"/ui/resources/{kind}/{uuid}/analytics"))


def start_fake(state):
    containers = [UUIDS["app1"], UUIDS["app2"], UUIDS["db"], UUIDS["service"], "metricssvcapp100000000000", "web-" + UUIDS["service"], state["other_team_app"], "removed-container", "coolify-proxy", "coolify-sentinel"] + [f"extra{i:02}" for i in range(20)]
    profiles = {TOKENS["busy"]: "busy", TOKENS["calm"]: "calm", TOKENS["old"]: "old"}
    source = open(FAKE, "rb").read()
    script = (
        "docker rm -f coolify-sentinel >/dev/null 2>&1\n"
        "cat > /root/metrics_fake_sentinel.py <<'FAKE_EOF'\n" + source.decode() + "\nFAKE_EOF\n"
        f"docker run -d --name coolify-sentinel --network none -e 'PROFILES={json.dumps(profiles)}' -e 'CONTAINERS={json.dumps(containers)}' "
        "-v /root/metrics_fake_sentinel.py:/fake.py:ro --entrypoint sh metrics-fake-sentinel -c 'exec python /fake.py' >/dev/null\n"
        "sleep 2\n"
    )
    build = b"docker image inspect metrics-fake-sentinel >/dev/null 2>&1 || printf 'FROM python:3.13-alpine\\nRUN apk add --no-cache curl\\n' | docker build -q -t metrics-fake-sentinel - >/dev/null\n"
    subprocess.run([VM, "run"], input=build, check=True)
    subprocess.run([VM, "run"], input=script.encode(), check=True)


def fake_phase(state, rs, rs2):
    start_fake(state)
    who = {"user": state["user"], "team": state["team"]}
    touch(state)
    servers = state["servers"]
    # `/summary` stays in both caches for a minute after the real Sentinel left.
    time.sleep(62)

    def fleet_rs(range_="24h", server="", cmetric="cpu", session=rs):
        return session.call("GET", f"/ui/metrics?range={range_}&server={server}&cmetric={cmetric}")

    def compare_fleet(name, range_="24h", server="", cmetric="cpu"):
        def read():
            # Sentinel answers are cached for a minute on each side, keyed by the minute of the window.
            return php("fleet", range=range_, server=server, cmetric=cmetric, **who), fleet_rs(range_, server, cmetric)

        ref, mine = in_one_minute(read)
        ref["serverOptions"] = [{"value": k, "label": v} for k, v in ref["serverOptions"].items()] if isinstance(ref["serverOptions"], dict) else ref["serverOptions"]
        ref_top = ref.pop("top")
        raw = mine.pop("containersRaw")
        key = {"memory": "memUsed", "disk": "diskBytes", "network": "net"}.get(cmetric, "cpu")
        # `rankContainers` runs in the browser here: the same stable sort over what the server sent.
        top = sorted(raw, key=lambda c: -(c.get(key) or 0))[:20]
        same(f"fleet {name}", ref, mine)
        # `route()` is absolute upstream; the same path is the same link in a single page app.
        for row in ref_top:
            if row.get("link"):
                row["link"] = re.sub(r"^https?://[^/]+", "", row["link"])
        same(f"fleet {name} top containers", ref_top, top)

    compare_fleet("all servers 24h")
    compare_fleet("all servers 7d", range_="7d")
    compare_fleet("all servers 30d", range_="30d")
    compare_fleet("one server", server=servers["busy"])
    compare_fleet("old sentinel", server=servers["old"])
    compare_fleet("wrong token", server=servers["badtoken"])
    compare_fleet("offline server", server=servers["offline"])
    compare_fleet("metric memory", cmetric="memory")
    compare_fleet("metric disk", cmetric="disk")
    compare_fleet("metric network", cmetric="network")
    compare_fleet("unknown metric and range", range_="weird", cmetric="weird")
    compare_fleet("unknown server", server="nosuchserver")

    def compare_server(name, uuid, mins):
        def read():
            return php("server_series", uuid=uuid, mins=mins, **who), rs.call("GET", f"/ui/server-page/{uuid}/metrics/data?interval={mins}")

        return in_one_minute(read)

    for mins in [60, 720, 10080, 43200]:
        ref, mine = compare_server("busy", servers["busy"], mins)
        for key in ref:
            # `from` of these windows is by the second, so the first point can fall in or out.
            r, m = ref[key], mine[key]
            if r and m and len(r) != len(m) and mins <= 720:
                shared = {p[0] for p in r} & {p[0] for p in m}
                r = [p for p in r if p[0] in shared]
                m = [p for p in m if p[0] in shared]
            same(f"server {key} last {mins} minutes", r, m)
    ref, mine = compare_server("old", servers["old"], 60)
    for key in ["diskSeries", "loadSeries", "networkRxSeries", "networkTxSeries"]:
        same(f"old sentinel {key}", ref[key], mine[key])
    for kind, uuid in [("application", UUIDS["app1"]), ("database", UUIDS["db"])]:
        for mins in [5, 720, 43200]:

            def read():
                return php("resource_series", kind=kind, uuid=uuid, mins=mins, **who), rs.call("GET", f"/ui/resources/{kind}/{uuid}/metrics/data?interval={mins}")

            ref, mine = in_one_minute(read)
            for key in ["cpu", "memory"]:
                r, m = ref[key], mine[key]
                if mins <= 720 and len(r) != len(m):
                    shared = {p[0] for p in r} & {p[0] for p in m}
                    r = [p for p in r if p[0] in shared]
                    m = [p for p in m if p[0] in shared]
                same(f"{kind} {key} last {mins} minutes", r, m)

    bad = rs.call("GET", f"/ui/server-page/{servers['badtoken']}/metrics/data?interval=5")
    record("wrong token gives upstream's message on the server page", "Unauthorized, please check your metrics token" in str(bad), bad)
    for uuid, name in [(servers["offline"], "offline"), (servers["busy"], "busy")]:
        ref = php("dashboard_chart", uuid=uuid, **who)
        mine = rs.call("GET", f"/ui/dashboard/server-metrics/{uuid}")
        same(f"dashboard chart {name}", ref, mine) if ref is None else record(f"dashboard chart {name} has points", bool(mine["cpu"]) and bool(ref["cpu"]), mine)

    # No server has metrics: the empty state, and enabling them.
    who2 = {"user": state["user2"], "team": state["team2"]}
    ref = php("fleet", **who2)
    mine = fleet_rs(session=rs2)
    ref.pop("top")
    # Without servers the page stops before the numbers, so `mount` never calls `loadData`.
    for key in ["kpis", "serverRows", "chartData"]:
        ref.pop(key)
    same("fleet of a team without metrics", ref, mine)


def main():
    if "--cleanup" in sys.argv:
        cleanup()
        if "--down" in sys.argv:
            subprocess.run([VM, "down"], check=False)
        return
    if "--touch" in sys.argv:
        touch(json.load(open(STATE)))
        return
    if "--metrics-off" in sys.argv:
        state = json.load(open(STATE))
        php("metrics_off", server=state["server_ids"]["second"])
        return
    if "--deployment-on" in sys.argv:
        state = json.load(open(STATE))
        state["deployment"] = php("deployment_on", uuid=UUIDS["app1"])
        json.dump(state, open(STATE, "w"))
        return
    if "--deployment-off" in sys.argv:
        state = json.load(open(STATE))
        php("deployment_off", id=state.pop("deployment"))
        json.dump(state, open(STATE, "w"))
        return
    if "--audit" in sys.argv:
        state = json.load(open(STATE))
        print(json.dumps(php("audit_of", team=state["team2"])))
        return
    switch = next((a.split("=")[1] for a in sys.argv if a.startswith("--sentinel=")), None)
    if switch:
        state = json.load(open(STATE))
        if switch == "real":
            real_phase(state, None)
        else:
            start_fake(state)
            touch(state)
        return
    cleanup()
    state = seed()
    try:
        rs = Session("metricsbot@example.test")
        rs2 = Session("metricsbot2@example.test")
        real_phase(state, rs)
        fake_phase(state, rs, rs2)
    finally:
        if "--keep" not in sys.argv:
            cleanup()
            if "--down" in sys.argv:
                subprocess.run([VM, "down"], check=False)
    passed = sum(results)
    print(f"{passed}/{len(results)} passed")
    sys.exit(0 if passed == len(results) else 1)


if __name__ == "__main__":
    main()
