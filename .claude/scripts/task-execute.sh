#!/usr/bin/env bash
# Simulate executing the Task Execute command (task-execute)
# Usage: task-execute.sh <beads-root-id> [mode]

set -euo pipefail

# Ensure common global locations are on PATH so globally-installed CLIs are found
export PATH="$PATH:/usr/local/bin:/opt/homebrew/bin:$HOME/.local/bin:/usr/local/sbin:/usr/sbin:/bin"

# Helper function to safely read issue notes
# Usage: get_issue_notes <issue_id>
# Returns: The notes field content, or empty string if not found/null
get_issue_notes() {
  local issue_id="$1"
  if [ -z "$issue_id" ]; then
    echo "" >&2
    return 1
  fi
  
  # Get BD_BIN if not already set
  local bd_bin=${BD_BIN:-$(command -v bd 2>/dev/null || true)}
  if [ -z "$bd_bin" ]; then
    echo ""
    return 0
  fi
  
  # Attempt to get issue JSON, handle errors gracefully
  local json_output
  if ! json_output=$("$bd_bin" show --json "$issue_id" 2>/dev/null); then
    # Issue not found or bd CLI error
    echo ""
    return 0
  fi
  
  # Extract notes field using jq, return empty string if null/missing
  local notes
  notes=$(echo "$json_output" | jq -r '.[0].notes // ""' 2>/dev/null || echo "")
  echo "$notes"
  return 0
}

# Helper function to format execution summary for issue notes
# Usage: format_execution_summary <root_id> <mode> <timestamp> <out_file> [feature_id] [task_id] [success]
# Arguments:
#   root_id     - The root issue ID
#   mode        - "apply" or "preview"
#   timestamp   - Execution timestamp (e.g., 20260119T163111Z)
#   out_file    - Path to the log file (absolute or relative)
#   feature_id  - (Optional) Created feature ID (for apply mode)
#   task_id     - (Optional) Created task ID (for apply mode)
#   success     - (Optional) "true" or "false" - test success status
# Returns: Markdown-formatted execution summary
format_execution_summary() {
  local root_id="$1"
  local mode="$2"
  local timestamp="$3"
  local out_file="$4"
  local feature_id="${5:-}"
  local task_id="${6:-}"
  local success="${7:-}"
  
  # Convert absolute path to relative path from repo root
  local repo_root
  repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
  local rel_log_path
  if [[ "$out_file" = "$repo_root"* ]]; then
    rel_log_path="${out_file#$repo_root/}"
  else
    rel_log_path="$out_file"
  fi
  
  # Determine status based on mode and success flag
  local status_line
  if [ "$mode" = "preview" ]; then
    status_line="**Status**: ℹ️ Preview only"
  elif [ -z "$success" ]; then
    # No success flag provided (backward compat)
    status_line="**Status**: ✅ Completed"
  elif [ "$success" = "true" ]; then
    status_line="**Status**: ✅ Tests passed"
  else
    status_line="**Status**: ❌ Tests failed"
  fi
  
  # Build the summary
  local summary
  summary="---
## Execution Summary
**Timestamp**: $timestamp
**Mode**: $mode
$status_line
**Logfile**: [execution log]($rel_log_path)"
  
  # Add created issues section if in apply mode and IDs are provided
  if [ "$mode" = "apply" ] && [ -n "$feature_id" ] && [ -n "$task_id" ]; then
    summary="$summary

**Created Issues**:
- Feature: $feature_id
- Task: $task_id"
  fi
  
  echo "$summary"
  return 0
}

# Helper function to append summary to issue notes
# Usage: append_to_notes <issue_id> <summary_text>
# Arguments:
#   issue_id      - The issue ID to update
#   summary_text  - The formatted summary text to append
# Returns: 0 on success, 1 on failure
# Prints: Success/failure message to stdout
append_to_notes() {
  local issue_id="$1"
  local summary_text="$2"
  
  if [ -z "$issue_id" ]; then
    echo "Error: issue_id is required" >&2
    return 1
  fi
  
  if [ -z "$summary_text" ]; then
    echo "Error: summary_text is required" >&2
    return 1
  fi
  
  # Get BD_BIN if not already set
  local bd_bin=${BD_BIN:-$(command -v bd 2>/dev/null || true)}
  if [ -z "$bd_bin" ]; then
    echo "Error: bd CLI not found" >&2
    return 1
  fi
  
  # Get current notes
  local current_notes
  current_notes=$(get_issue_notes "$issue_id")
  
  # Combine notes with separator if current notes exist
  local combined_notes
  if [ -n "$current_notes" ]; then
    # Add separator between existing notes and new summary
    combined_notes="$current_notes

$summary_text"
  else
    # No existing notes, just use the summary
    combined_notes="$summary_text"
  fi
  
  # Update issue with combined notes, using proper escaping
  # Note: bd update accepts multiline strings via --notes
  if "$bd_bin" update "$issue_id" --notes="$combined_notes" >/dev/null 2>&1; then
    echo "Successfully appended summary to issue $issue_id"
    return 0
  else
    echo "Error: Failed to update notes for issue $issue_id" >&2
    return 1
  fi
}

ROOT=${1:-}
# Default to apply unless --dry-run is provided
MODE=apply
shift || true
DRY_RUN=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run|-n|--preview)
      DRY_RUN=true; shift;
      ;;
    --mode)
      MODE="$2"; shift 2;;
    preview)
      DRY_RUN=true; shift;;
    apply)
      DRY_RUN=false; shift;;
    *)
      # ignore unknown extras
      shift;;
  esac
done
if [ "$DRY_RUN" = true ]; then
  MODE=preview
else
  MODE=apply
fi
OUT_DIR="$(pwd)/.claude/tmp"
mkdir -p "$OUT_DIR"
chmod 755 "$OUT_DIR" || true
TS=$(date -u +%Y%m%dT%H%M%SZ)
OUT_FILE="$OUT_DIR/task-execute-$TS.txt"

chmod +x "$OUT_DIR" || true

if [ -z "$ROOT" ]; then
  echo "Usage: $0 <beads-root-id> [--dry-run]"
  exit 2
fi

cat > "$OUT_FILE" <<EOF
Task Execute Simulation
Timestamp: $TS
Root: $ROOT
Mode: $MODE

Planned bd commands (dry-run):
EOF

cat >> "$OUT_FILE" <<'CMDS'
# Example commands that would be run in apply mode:
# bd create --title="Feature: Example under $ROOT" --type=feature --parent=$ROOT --silent
# bd create --title="Task: Implement part A" --type=task --parent=beads-xxx --silent
# bd dep add beads-xxx beads-yyy
CMDS

echo "Simulated task execution; output written to: $OUT_FILE"
if [ "$MODE" = "preview" ]; then
  echo "Preview mode: no changes made. To apply, run without --dry-run."
  
  # Append summary to ROOT issue in preview mode
  SUMMARY=$(format_execution_summary "$ROOT" "$MODE" "$TS" "$OUT_FILE")
  if ! append_to_notes "$ROOT" "$SUMMARY" 2>/dev/null; then
    echo "Warning: Failed to append summary to issue $ROOT (non-fatal)" >&2
  fi
  
  exit 0
fi

# APPLY MODE: perform actions using bd CLI and run tests. This will attempt to create
# a feature and a task and then run tests; if tests pass, close the root bead.
BD_BIN=${BD_BIN:-$(command -v bd 2>/dev/null || true)}
if [ -z "$BD_BIN" ]; then
  echo "bd CLI not found on PATH. Please ensure bd is installed and on PATH, or set BD_BIN to its path." >&2
  exit 2
fi

set -o pipefail

echo "APPLY MODE: creating feature and task under $ROOT"
FEATURE_ID=$("$BD_BIN" create --title="Feature: Implementation (task-execute)" --type=feature --parent="$ROOT" --silent 2>/dev/null | head -n1 || true)
if [ -z "$FEATURE_ID" ]; then
  echo "Failed to create feature under $ROOT" >&2
  exit 3
fi

TASK_ID=$("$BD_BIN" create --title="Task: Implement changes for $ROOT" --type=task --parent="$FEATURE_ID" --silent 2>/dev/null | head -n1 || true)
if [ -z "$TASK_ID" ]; then
  echo "Failed to create implementation task under $FEATURE_ID" >&2
  exit 4
fi

echo "Created feature $FEATURE_ID and task $TASK_ID"

# Run the project's unit tests (TDD green check)
echo "Running unit tests (npm run lab:test)"
if npm run lab:test; then
  echo "Tests passed — closing root bead $ROOT"
  
  # Append success summary to ROOT issue
  SUMMARY=$(format_execution_summary "$ROOT" "$MODE" "$TS" "$OUT_FILE" "$FEATURE_ID" "$TASK_ID" "true")
  if ! append_to_notes "$ROOT" "$SUMMARY" 2>/dev/null; then
    echo "Warning: Failed to append summary to issue $ROOT (non-fatal)" >&2
  fi
  
  "$BD_BIN" close "$ROOT" --reason="Completed by task-execute (tests passed)" || true
  echo "Closed $ROOT"
  echo "Applied changes: feature $FEATURE_ID, task $TASK_ID; root closed: $ROOT"
  exit 0
else
  echo "Tests failed — leaving issues open for manual work"
  
  # Append failure summary to ROOT issue
  SUMMARY=$(format_execution_summary "$ROOT" "$MODE" "$TS" "$OUT_FILE" "$FEATURE_ID" "$TASK_ID" "false")
  if ! append_to_notes "$ROOT" "$SUMMARY" 2>/dev/null; then
    echo "Warning: Failed to append summary to issue $ROOT (non-fatal)" >&2
  fi
  
  echo "You can inspect created items: $FEATURE_ID, $TASK_ID"
  exit 5
fi

exit 0
