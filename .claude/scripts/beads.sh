#!/usr/bin/env bash
# Simple bead helper skill wrapper (simulated)
# Usage: beads.sh <beads-id> <action>

set -euo pipefail

ID=${1:-}
ACTION=${2:-info}
OUT_DIR="$(pwd)/.claude/tmp"
mkdir -p "$OUT_DIR"
TS=$(date -u +%Y%m%dT%H%M%SZ)
OUT_FILE="$OUT_DIR/beads-$TS.txt"

chmod 755 "$OUT_DIR" || true

if [ -z "$ID" ]; then
  echo "Usage: $0 <beads-id> [info|status|simulate-create]"
  exit 2
fi

case "$ACTION" in
  info)
    echo "Beads skill: info for $ID" > "$OUT_FILE"
    echo "Found: $ID (simulated)" >> "$OUT_FILE"
    echo "No real API calls performed." >> "$OUT_FILE"
    ;;
  status)
    echo "Beads skill: status for $ID" > "$OUT_FILE"
    echo "Status: open (simulated)" >> "$OUT_FILE"
    ;;
  simulate-create)
    echo "Beads skill: simulate create under $ID" > "$OUT_FILE"
    echo "Would run: bd create --title=\"Example\" --parent=$ID --silent" >> "$OUT_FILE"
    ;;
  *)
    echo "Unknown action: $ACTION" >&2
    exit 2
    ;;
esac

echo "Beads simulation output written to: $OUT_FILE"
exit 0
