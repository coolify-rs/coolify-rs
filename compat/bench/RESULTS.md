# Performance: Coolify and coolify-rs

Measured on 2026-11-03 at commit 4b6d7d0. The raw numbers are in `results.json`. To reproduce:

```bash
docker build -t coolify-rs:bench .
compat/bench/stack.sh up        # needs the reference environment from compat/reference for the data copy
python3 compat/bench/bench.py   # about an hour; --quick for a short check
compat/bench/stack.sh down
```

## Setup

Each side runs as its own stack, shaped like a small VPS:

| | Coolify stack | coolify-rs stack |
|---|---|---|
| Application | official image `coollabsio/coolify:sha-b5823c7`, production mode, Horizon, scheduler and Reverb running | image from this repository's `Dockerfile`, scheduler running |
| Also in the stack | PostgreSQL 15, Redis 7 | PostgreSQL 15 |
| CPU | 2 cores shared by the whole stack (`--cpuset-cpus`) | 2 other cores, likewise |
| Memory limit | 4 GB for the application | 4 GB for the application |

- **Data:** both databases start as copies of the same database: a team with 113 applications,
  7 databases, 9 projects and 2 servers. The same API tokens work on both, and responses have the
  same size on both sides.
- **Network:** internal, with no internet access and no route to the servers in the data. The
  schedulers run their jobs, and server checks fail as they would for unreachable servers.
- **Rate limit:** Coolify's API limit (`API_RATE_LIMIT`, 200 requests a minute per user) is raised so
  the numbers measure Coolify and not the limiter. coolify-rs has no API rate limit.
- **Load:** k6 0.57 runs on 8 more cores inside the same network, so Docker's port forwarding is not
  measured. Each virtual user keeps one of three API tokens of the team.
- **Host:** Apple M5 Max, with Docker in OrbStack's Linux VM (18 CPUs, 16 GB). Other containers on
  this machine were not pinned and could share the stacks' cores, which is the main source of the
  run-to-run spread below.

## Startup and idle

Startup is the time from `docker start` of the application to its first healthy response, as the
median of 5 runs. Idle figures are medians over 10 minutes, measured after 2 minutes of settling.
Memory is from `docker stats`; CPU is from each container's cgroup.

| | Coolify | coolify-rs |
|---|---|---|
| Startup | 1.62 s | 0.22 s |
| Application memory at idle | 366 MiB (+ 6 MiB Redis) | 30 MiB |
| PostgreSQL memory at idle | 83 MiB | 118 MiB |
| Whole stack at idle | 455 MiB | 148 MiB |
| CPU at idle, whole stack | 3.6% of a core | 0.5% of a core |

coolify-rs's PostgreSQL uses more memory because its pool keeps up to 10 connections open, and each one
is a PostgreSQL process. Coolify's PHP workers open fewer.

## API under load

Each cell is the median of 3 runs of 20 seconds, each after a 3-second warm-up.
- **Latency:** milliseconds, p50 / p95.
- **CPU:** the whole stack's CPU time per request, from the cgroups.
- **Peak:** the application's highest memory during the run.

### 1 client

| Endpoint (response size) | Coolify req/s | coolify-rs req/s | Coolify p50 / p95 | coolify-rs p50 / p95 | Coolify CPU/req | coolify-rs CPU/req |
|---|---|---|---|---|---|---|
| `/api/v1/version` (19 B) | 89 | 641 | 10.9 / 13.9 | 1.3 / 2.4 | 11.5 ms | 0.51 ms |
| `/api/v1/projects` (792 B) | 94 | 739 | 10.2 / 13.7 | 1.2 / 1.8 | 10.8 ms | 0.43 ms |
| `/api/v1/servers` (9 KB) | 87 | 757 | 10.9 / 14.7 | 1.2 / 1.7 | 12.2 ms | 0.54 ms |
| `/api/v1/databases` (92 KB) | 28 | 93 | 35.3 / 41.3 | 10.4 / 13.6 | 36.0 ms | 9.4 ms |
| `/api/v1/applications` (1.7 MB) | 10 | 61 | 95.5 / 125.2 | 15.7 / 19.1 | 110.3 ms | 15.4 ms |

### 10 clients

| Endpoint | Coolify req/s | coolify-rs req/s | Coolify p50 / p95 | coolify-rs p50 / p95 | Coolify peak | coolify-rs peak |
|---|---|---|---|---|---|---|
| `/api/v1/version` | 165 | 1,430 | 59.6 / 84.9 | 5.8 / 13.2 | 412 MiB | 41 MiB |
| `/api/v1/projects` | 178 | 1,155 | 55.3 / 75.8 | 6.8 / 14.4 | 420 MiB | 46 MiB |
| `/api/v1/servers` | 165 | 1,285 | 60.1 / 78.9 | 6.6 / 13.8 | 411 MiB | 48 MiB |
| `/api/v1/databases` | 43 | 319 | 224.0 / 302.3 | 31.1 / 50.2 | 435 MiB | 47 MiB |
| `/api/v1/applications` | 18 | 124 | 543.7 / 663.1 | 77.6 / 125.9 | 553 MiB | 104 MiB |

### 50 clients

| Endpoint | Coolify req/s | coolify-rs req/s | Coolify p50 / p95 | coolify-rs p50 / p95 | Coolify peak | coolify-rs peak |
|---|---|---|---|---|---|---|
| `/api/v1/version` | 127 | 1,304 | 391.7 / 498.0 | 31.9 / 71.3 | 430 MiB | 45 MiB |
| `/api/v1/projects` | 185 | 1,954 | 263.6 / 313.6 | 18.5 / 50.4 | 416 MiB | 24 MiB |
| `/api/v1/servers` | 160 | 1,284 | 307.6 / 370.8 | 33.6 / 60.5 | 464 MiB | 47 MiB |
| `/api/v1/databases` | 43 | 338 | 1,157 / 1,371 | 141.3 / 205.9 | 479 MiB | 56 MiB |
| `/api/v1/applications` | 18 | 126 | 2,794 / 3,238 | 390.4 / 456.4 | 773 MiB | 165 MiB |

## Reading the numbers

- **CPU is the main difference.** On small endpoints, a request costs Coolify 11–16 ms of CPU and
  coolify-rs about 0.5 ms. On a 2-core server, that sets how many requests each can serve: Coolify
  levels off at 130–190 req/s, and coolify-rs at 1,100–2,600.
- **Run-to-run spread.**
  - Coolify's throughput varied by up to 16% between runs, and coolify-rs's by 5–76%. coolify-rs was
    the noisier one because at its throughput, cores borrowed by other containers on the host
    matter more.
  - In every cell, the slowest coolify-rs run beat the fastest Coolify run.
  - Latency and CPU per request moved little between runs.
- **Large lists.** These are the closest results, at 3–7× apart. coolify-rs reproduces upstream's
  queries, with each list mirroring the order Eloquent produces.

## Not measured

- **Deployments.** A deployment mostly waits on `docker build` and image pulls on the server, and
  those run the same commands on both sides.
- **The web UI.** coolify-rs serves a static bundle and the JSON behind it, while Coolify renders
  Livewire pages on the server. Comparing them needs a browser-driven benchmark.
- **Real hardware.** These are cores pinned inside a VM on a laptop. A run on two identical cloud
  servers would be the cleaner comparison.
