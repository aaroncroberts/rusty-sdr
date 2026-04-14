# Beads Cleanup & WBS Reorganization

Analyze and reorganize beads into a coherent Epic/Feature/Task hierarchy with proper dependencies.

## Arguments

```
$ARGUMENTS
```

**Supported arguments:**
- `--status=<state>` - Filter by status: `open`, `in_progress`, `closed`, `all` (default: `open`)
- `--analyze` - Analysis only, no changes (default behavior without args)
- `--execute` - Execute the cleanup plan
- `--domain=<name>` - Focus on specific domain (e.g., `grants`, `planny`, `business-plan`)

## Examples

```bash
/beads-cleanup                           # Analyze open beads
/beads-cleanup --status=all              # Analyze all beads
/beads-cleanup --status=in_progress      # Check in-progress work
/beads-cleanup --execute                 # Execute cleanup after analysis
/beads-cleanup --domain=grants --execute # Clean up grants domain only
```

## Cleanup Process

### Phase 1: Analysis

1. **Inventory** - List all beads in target state(s)
   ```bash
   todd-carl list --status=<state> --limit 0
   todd-carl stats
   todd-carl blocked
   ```

2. **Categorize by Domain** - Group beads into logical areas:
   - Business Plan (phases, sections, content)
   - Grants & Funding (applications, research, monitoring)
   - Vendor Outreach (materials, contacts, deadlines)
   - App/Tooling (features, bugs, infrastructure)
   - Product Development (flavors, branding, packaging)
   - Operations (compliance, SOPs, permits)

3. **Identify Issues**:
   - **Orphans**: Tasks with no parent epic/feature
   - **Stale**: Passed deadlines, outdated P0s
   - **Mistyped**: Epics that should be features (or vice versa)
   - **Blocked chains**: Dependencies that should be resolved
   - **Duplicates**: Similar/overlapping tasks

### Phase 2: Design WBS Structure

Create hierarchy:
```
EPIC (strategic initiative)
├── FEATURE (deliverable capability)
│   ├── TASK (actionable work item)
│   ├── BUG (defect fix)
│   └── CHORE (maintenance)
```

Dependency types:
- `parent-child` - Hierarchy (epic→feature→task)
- `blocks` - Critical path (must complete first)
- `validates` - Testing relationship
- `relates` - Loose association

### Phase 3: Execute (if --execute)

1. **Close stale beads**
   ```bash
   todd-carl close <id> --reason="<explanation>"
   ```

2. **Create missing parents**
   ```bash
   todd-carl create --title="<name>" --type=feature --priority=<N>
   ```

3. **Retype misclassified**
   ```bash
   todd-carl update <id> --type=<correct-type>
   ```

4. **Wire dependencies**
   ```bash
   todd-carl dep add <child> <parent> --type=parent-child
   todd-carl dep add <blocked> <blocker>
   ```

5. **Adjust priorities**
   ```bash
   todd-carl update <id> --priority=<N>
   ```

## Output Format

### Analysis Report

```markdown
## Beads Cleanup Analysis

**Scope**: [status filter]
**Total beads**: N

### Domain Breakdown
| Domain | Open | In Progress | Blocked | Orphaned |
|--------|------|-------------|---------|----------|
| ...    | ...  | ...         | ...     | ...      |

### Issues Found
- **Stale (N)**: [list with IDs]
- **Orphans (N)**: [list with IDs]
- **Mistyped (N)**: [list with IDs]
- **Blocked chains (N)**: [explanation]

### Proposed Actions
1. Close: [IDs and reasons]
2. Create: [new parents needed]
3. Retype: [ID: old→new]
4. Wire: [dependency additions]
5. Reprioritize: [ID: old→new]

### Recommended WBS
[Tree structure showing proposed hierarchy]
```

## Best Practices

- **Don't over-organize** - Some flat structure is fine for small task sets
- **Preserve history** - Use close reasons to document why beads were closed
- **Unblock chains** - Prioritize resolving blocking dependencies
- **Parent-child for structure** - Use for organization, not sequencing
- **Blocking for critical path** - Use only when work truly can't start

## Quick Reference

```bash
# Analysis
todd-carl list --status=open --limit 0    # All open
todd-carl stats                           # Summary
todd-carl blocked                         # Blocked items
todd-carl show <id>                       # Details + deps
todd-carl dep tree <id>                   # Dependency tree

# Modification
todd-carl close <id> --reason="..."       # Close with reason
todd-carl close <id1> <id2> ...           # Batch close
todd-carl update <id> --type=feature      # Change type
todd-carl update <id> --priority=2        # Change priority
todd-carl update <id> --status=open       # Reopen

# Dependencies
todd-carl dep add <a> <b> --type=parent-child  # A is child of B
todd-carl dep add <a> <b>                       # A blocked by B (default)
todd-carl dep remove <a> <b>                    # Remove dependency
```
