#!/usr/bin/env bash
# Reverse interop check: payloads encrypted by coolify-rs must be decryptable
# by upstream Laravel. Requires the reference environment (reference-up.sh).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
key="base64:hPAFGDXpvHW0ZnwoP/YKkC1WJ+9Bv2jZCwc1hGGaI3s="
payload="$(cargo run -q --manifest-path "$root/Cargo.toml" -p coolify-compat --example encrypt -- "$key" "interop ✓")"
docker exec -w /var/www/html coolify-rs-reference-coolify-1 php -r '
require "vendor/autoload.php";
$e = new Illuminate\Encryption\Encrypter(base64_decode(substr($argv[1],7)), "aes-256-cbc");
$p = $e->decryptString($argv[2]);
if ($p !== "interop ✓") { fwrite(STDERR, "mismatch\n"); exit(1); }
echo "PASS: Laravel decrypted coolify-rs payload\n";
' "$key" "$payload"
