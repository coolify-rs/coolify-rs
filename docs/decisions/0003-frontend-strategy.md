# ADR 0003: Frontend strategy

Status: accepted (2026-10-27); revisit at milestone 6

## Context

The brief prefers retaining the upstream frontend. Reconnaissance found 272 Livewire components and 511 Blade views;
~193 routes render full-page Livewire components whose state and authorization live in PHP. Only the Svelte canvases
use JSON (via `/canvas/api`, which maps onto the REST controllers).

## Decision

Do not attempt to run the Livewire UI against coolify-rs. Until a frontend exists, coolify-rs runs side-by-side with
upstream as an API server on the same database. A frontend is a separately scoped project that consumes the REST API;
the Svelte canvas code is the first candidate for reuse (it already speaks the REST API).

## Consequences

User-facing parity is out of scope until the API covers the main workflows. Screenshot-based regression testing is
deferred to the frontend project.
