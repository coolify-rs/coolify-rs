# Performance: Coolify and coolify-rs

Measured on 2026-11-03 with `compat/bench/memory.sh` and `compat/bench/api_bench.py`.

## Setup

- Machine: Apple M5 Max, 36 GB, Docker in OrbStack's Linux VM (18 CPUs, 16 GB).
- Coolify: the official image `coollabsio/coolify:sha-b5823c7` as shipped. That means production mode with
  opcache, plus Redis, Horizon, the scheduler and Reverb running in its container.
- coolify-rs: the production image from this repository's `Dockerfile` (release build, mimalloc), with
  the scheduler turned off.
- Both run on the same Docker network and use the same PostgreSQL 15 database. Requests come from the Mac
  through Docker's port forwarding, which is the same path for both.
- The data is a team with 113 applications, 7 databases, 9 projects and 2 servers. Every response has the same size on both
  sides, and the applications, deployments and projects lists were checked byte for byte.
- Coolify limits its API to 200 requests a minute per user. For the runs, that limit was raised in
  the reference's config cache, so the numbers measure Coolify and not its limiter. coolify-rs has no
  API rate limit (see docs/KNOWN_LIMITATIONS.md).

## Startup and memory

| | Coolify | coolify-rs |
|---|---|---|
| Container start to first healthy response | 1.64 s | 0.19 s |
| Memory at idle | 411 MiB + 10 MiB Redis | 37 MiB |
| Peak memory, 16 clients reading the 1.7 MB applications list | 690 MiB | 166 MiB |
| Throughput of that load | 67 req/s | 277 req/s |
| Memory right after the load | 683 MiB | 168 MiB |
| Image size (unpacked / compressed) | 932 MB / 210 MB | 357 MB / 142 MB |

Memory is the container's usage as `docker stats` reports it. Coolify's idle figure includes its
queue workers and scheduler. coolify-rs ran with its scheduler off, because both instances share one
database and its servers. Its scheduler runs in the same process, so it adds no separate workers.

## API latency

Each row is 200 sequential requests on one connection (milliseconds), then 2,000 requests over 16
connections.

| Endpoint | Response | Coolify p50 / p95 | coolify-rs p50 / p95 | Coolify req/s | coolify-rs req/s |
|---|---|---|---|---|---|
| `GET /api/v1/version` | 19 B | 13.0 / 14.6 | 1.7 / 2.1 | 595 | 735 |
| `GET /api/v1/teams/current` | 355 B | 13.4 / 16.5 | 1.9 / 2.6 | 612 | 935 |
| `GET /api/v1/projects` | 792 B | 13.7 / 16.4 | 2.0 / 2.5 | 585 | 1,065 |
| `GET /api/v1/servers` | 9 KB | 17.6 / 23.6 | 2.2 / 2.6 | 505 | 1,161 |
| `GET /api/v1/applications` | 1.7 MB | 128.0 / 139.1 | 25.8 / 28.7 | 63 | 275 |
| `GET /api/v1/services` | 2 B | 16.6 / 19.0 | 2.7 / 3.0 | 461 | 1,320 |
| `GET /api/v1/databases` | 92 KB | 55.6 / 69.8 | 13.1 / 16.0 | 150 | 308 |
| `GET /api/v1/deployments` | 2 B | 21.2 / 24.8 | 2.4 / 3.0 | 352 | 1,388 |

On small responses, a single request is 6 to 9 times faster. Throughput rises less than latency
because both servers update the token's `last_used_at` on every request, as Sanctum does. Sixteen
clients sharing one token therefore wait on commits to the same row. The throughput numbers vary by
about 30% between runs, while the latency numbers repeat within a few percent.

## Not measured

- Deployment time. A deployment mostly waits on `docker build` and image pulls on the server, which
  are the same commands on both sides, and the network during these runs was too unstable to time
  them fairly.
- The web UI. It is a static bundle served by coolify-rs; Coolify renders Livewire pages per request.
- A smaller server. These numbers come from a fast machine. On a 1–2 vCPU VPS both sides are slower,
  but the difference in memory stays.
