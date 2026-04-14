#!/usr/bin/env bash
# Example task-generate script for copilot-skill-runner
# This is a harmless example that writes a small file in the repo's tmp directory

set -euo pipefail

REPO_ROOT="$(pwd)"
OUT_DIR="$REPO_ROOT/.claude/tmp"
mkdir -p "$OUT_DIR"

TS=$(date -u +%Y%m%dT%H%M%SZ)
OUT_FILE="$OUT_DIR/task-generate-output-$TS.txt"

cat > "$OUT_FILE" <<EOF
Task Generate Example
Timestamp: $TS
Invoked by: copilot-skill-runner example
EOF

echo "Wrote example output to: $OUT_FILE"
exit 0
