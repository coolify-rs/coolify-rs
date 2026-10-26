# ADR 0002: The REST API is the primary compatibility contract

Status: accepted (2026-10-27)

## Context

Upstream's UI is Livewire (server-rendered PHP with server-side component state); it cannot be served by a Rust
backend. The `/api` surface (321 operations, OpenAPI-documented) is machine-readable, versioned and used by external
automation, the MCP bridge and upstream's own Svelte canvases.

## Decision

Compatibility is defined first as: same `/api` routes, middleware semantics, status codes, error bodies and response
bytes (PHP `json_encode` defaults and `serializeApiResponse` ordering), proven by a differential harness against the
official image. Unported upstream routes answer 501 (not 404) so clients can distinguish "missing" from "not yet".

## Consequences

Byte-level compatibility forces some quirks to be reproduced (201 on PATCH, `"success": true` on 403). Each quirk is
listed in API_COMPATIBILITY.md §8. Deliberate divergences are listed in KNOWN_LIMITATIONS.md.
