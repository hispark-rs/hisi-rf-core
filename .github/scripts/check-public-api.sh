#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BASELINE="$ROOT/.github/public-api/all-features.txt"
EXPECTED_VERSION="cargo-public-api 0.52.0"

actual_version="$(cargo public-api --version 2>/dev/null || true)"
if [ "$actual_version" != "$EXPECTED_VERSION" ]; then
    echo "ERROR: expected $EXPECTED_VERSION, found ${actual_version:-not installed}" >&2
    exit 1
fi

host="$(rustc -vV | sed -n 's/^host: //p')"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cd "$ROOT"
cargo metadata --locked --format-version 1 --no-deps >/dev/null
cargo public-api -sss --color=never \
    --target "$host" \
    --all-features \
    > "$tmp/actual.txt"

if ! diff -u "$BASELINE" "$tmp/actual.txt"; then
    echo "ERROR: the hisi-rf-core public API changed" >&2
    echo "Review the diff; update the baseline only with an intentional API change." >&2
    exit 1
fi

echo "hisi-rf-core public API matches the all-features baseline"
