---
description: Generate a structured WBS (Work Breakdown Structure) from a user prompt for creating beads issues
argument-hint: <feature description or work context>
tools:
  - Bash(bd:*)
---

# Task Generation from Prompt

Generate structured work items from a natural language description using work item type best practices. This skill detects the appropriate type, applies the correct description template, establishes hierarchy, and creates issues using `bd` CLI.

## Work Item Type Hierarchy

```
Epic (strategic initiative, multi-sprint)
├── Feature (deliverable capability)
│   ├── Task (actionable work, single session)
│   ├── Bug (defect fix - links to parent Feature)
│   └── Chore (maintenance/cleanup)
```

## Step 1: Detect Work Item Type

Analyze the user's input to determine the appropriate work item type.

### 1a. Check Explicit Type Hints

**Priority 1 - Command flags:**
| Flag | Type |
|------|------|
| `--type=epic` | Epic |
| `--type=feature` | Feature |
| `--type=task` | Task |
| `--type=bug` | Bug |
| `--type=chore` | Chore |

**Priority 2 - Title prefixes (case-insensitive):**
| Prefix | Type |
|--------|------|
| `Epic:` | Epic |
| `Feature:` | Feature |
| `Task:` | Task |
| `Bug:` | Bug |
| `Fix:` | Bug |
| `Chore:` | Chore |
| `Cleanup:` | Chore |

If explicit hint found → use that type, **strip the prefix from the title**, then skip to Step 2.

**IMPORTANT**: Title prefixes are for *input detection only*. Generated issue titles must NOT include type prefixes—the type is encoded in `--type=<type>` metadata.

### 1b. Analyze Implicit Signals

Score each type by keyword matches (check title + description):

**Epic Signals** (+3 points each):
- "initiative", "overhaul", "migration", "redesign system"
- "multi-sprint", "phase 1", "strategic", "transform"
- Multiple feature areas mentioned

**Feature Signals** (+3 points each):
- "add [capability]", "implement [noun]", "support for"
- "enable users to", "introduce", "allow [action]"
- Clear capability boundary described

**Task Signals** (+3 points each):
- "create [file]", "write [code]", "configure [thing]"
- "update [specific]", "modify", "add [small thing]"
- Single deliverable, session-completable

**Bug Signals** (+3 points each):
- "broken", "doesn't work", "fails to", "error when"
- "crash", "wrong", "incorrect", "unexpected"
- "should [verb] but [doesn't]", "used to work"
- Repro steps pattern detected

**Chore Signals** (+3 points each):
- "clean up", "refactor", "remove unused"
- "update dependencies", "deprecate", "rename"
- "tech debt", "housekeeping"

**Scoring:**
1. Count signal matches for each type
2. Highest score wins
3. Tie-breaker (prefer smaller scope): Task > Bug > Chore > Feature > Epic
4. If all scores = 0: default to `task`

### 1c. Determine Scope

Based on detected type, assess WBS need:

| Type | Default Scope | WBS Needed? |
|------|---------------|-------------|
| Epic | Multi-feature | **Always** (decompose to Features) |
| Feature | Multi-task | Usually (decompose to Tasks) |
| Task | Single item | Rarely (single bd create) |
| Bug | Single item | Rarely (single bd create) |
| Chore | Single item | Never |

**WBS Indicators** (override to "needs WBS"):
- "phases", "steps", "first...then..." language
- Multiple distinct deliverables mentioned
- Research/discovery needed before implementation

## Step 2: Apply Description Template

Use the appropriate template based on detected work item type.

### Epic Template

```markdown
## Overview
[High-level description of the strategic initiative]

## Goals
- [Measurable outcome 1]
- [Measurable outcome 2]

## Success Criteria
- [ ] [Indicator 1 - required by bdlint]
- [ ] [Indicator 2]

## Scope
**In Scope**: [What this epic covers]
**Out of Scope**: [What this epic does NOT cover]

## Features
| Feature | Description |
|---------|-------------|
| [Name] | [Brief description] |

---
*Living document - updates as discovery progresses*
```

### Feature Template

```markdown
## Overview
[What capability this feature delivers]

## Scope
**In Scope:**
- [Included functionality]

**Out of Scope:**
- [Explicitly excluded]

## Acceptance Criteria
- [ ] [Testable outcome - required by bdlint]
- [ ] [Testable outcome]
```

### Task Template

```markdown
**INCOMING**: [Context/artifacts from upstream work]
**OUTGOING**: [Deliverables for downstream work]

## Description
[What needs to be done]

## Acceptance Criteria
- [ ] [Testable outcome - required by bdlint]
- [ ] [Testable outcome]
```

### Task Template (Discovery Variant)

For tasks that output research or analysis:

```markdown
**OUTPUT**: [Specific deliverable type, e.g., "Implementation task list"]

## Research Scope
- [Area to investigate 1]
- [Area to investigate 2]

## Deliverable
[What this task produces for downstream work]
```

### Bug Template

```markdown
## Steps to Reproduce
1. [Precondition/setup]
2. [Action taken]
3. [Result observed]

## Expected Behavior
[What should happen]

## Actual Behavior
[What happens instead]

## Environment
- Version: [if applicable]
- Platform: [OS/browser/device]

## Acceptance Criteria
- [ ] Bug no longer reproduces
- [ ] [Additional verification]
```

### Chore Template

```markdown
## Cleanup Scope
[What is being cleaned/refactored/updated]

## Context
Related to: [parent task/feature this supports, if any]

## Completion Criteria
- [ ] [What defines done]
```

### Template Selection Logic

```
Detected Type → Template → Required Sections (bdlint)
─────────────────────────────────────────────────────
Epic          → Epic     → Success Criteria
Feature       → Feature  → Acceptance Criteria
Task          → Task     → Acceptance Criteria
Bug           → Bug      → Steps to Reproduce, Acceptance Criteria
Chore         → Chore    → (none required)
```

## Step 3: Bug Parent Resolution

When creating a bug, find the related Feature to attach it to.

### 3a. Check for Explicit Parent

If `--parent=<id>` provided, validate and use it.

### 3b. Extract Search Keywords

From the bug description, extract component identifiers:
- Component names: "login", "dashboard", "export", "API"
- Code identifiers: CamelCase or snake_case terms
- Skip generic words: "the", "is", "when", "broken"

### 3c. Search for Related Features

```bash
bd search "<keywords>" --type=feature --status=open --limit=5 --json
```

**Score each result:**
- Title keyword match: +3 points per keyword
- Description keyword match: +1 point per keyword
- Updated in last 30 days: +2 points

### 3d. Handle Results

| Confidence | Action |
|------------|--------|
| High (score ≥ 6) | Auto-link to highest-scoring feature |
| Medium (score 3-5) | Suggest linkage, ask for confirmation |
| Low (< 3) or none | Search closed features, then create standalone with warning |

### Example: Bug Parent Resolution

```bash
# User input: "The export to CSV doesn't include headers"
# Keywords: "export", "CSV"

features=$(bd search "export CSV" --type=feature --status=open --json)
# Returns: "Feature: Data Export" (score: 6)

bug_id=$(bd create "CSV export missing headers" --type=bug \
  --parent=bd-abc123 \
  --description="## Steps to Reproduce
1. Navigate to data export page
2. Click 'Export to CSV'
3. Open downloaded file

## Expected Behavior
CSV file should include column headers

## Actual Behavior
CSV starts with data, no headers

## Acceptance Criteria
- [ ] CSV export includes headers" --silent)

echo "Created bug $bug_id linked to Feature: Data Export"
```

## Step 4: Establish Hierarchy & Dependencies

### Parent-Child Relationships

Use `--parent` to establish organizational hierarchy:

| Parent Type | Valid Children |
|-------------|---------------|
| Epic | Feature |
| Feature | Task, Bug, Chore |
| Task | Chore |
| Bug | Chore |

```bash
epic=$(bd create "Initiative" --type=epic --silent)
feat=$(bd create "A" --type=feature --parent=$epic --silent)
task=$(bd create "Step 1" --type=task --parent=$feat --silent)
```

### Blocking Dependencies

Use `bd dep add <blocked> <blocker>` for execution order:

```bash
# Task B cannot start until Task A completes
bd dep add $task_b $task_a
```

**When to use blocking deps:**
- Task B needs output from Task A (data dependency)
- Tasks touch same files (resource conflict)
- Review must complete before merge

### Validation Dependencies

Use `--type=validates` for test relationships:

```bash
bd dep add $test_task $impl_task --type=validates
```

### Discovery → Implementation Pattern

When work requires research before building:

```bash
epic=$(bd create "Project" --type=epic --silent)
disc=$(bd create "Discovery" --type=feature --parent=$epic --silent)
impl=$(bd create "Implementation" --type=feature --parent=$epic --silent)
bd dep add $impl $disc  # Implementation blocked by Discovery
```

### Dependency Decision Tree

```
Does Task B need Task A's output?
├─ YES → bd dep add B A
│
Is Task B a test for Task A?
├─ YES → bd dep add B A --type=validates
│
Is this just for organization?
├─ YES → Use --parent instead
│
└─ NO to all → Keep parallel (no dependency)
```

## Step 5: Create Issues

Execute `bd create` commands with captured IDs.

**CRITICAL**: Use `--silent` flag and capture IDs in shell variables:

```bash
issue_id=$(bd create --title="..." --type=<type> --description="..." --silent)
```

### Example: Epic with Discovery/Implementation

```bash
# 1. Create Epic
epic=$(bd create "User Authentication System" --type=epic --priority=1 \
  --description="## Overview
Implement complete user authentication with OAuth support.

## Goals
- Secure login/logout
- OAuth2 provider support
- Session management

## Success Criteria
- [ ] Users can log in via email/password
- [ ] OAuth login works with Google/GitHub
- [ ] Sessions persist across browser refresh" --silent)

# 2. Create Discovery Feature
disc=$(bd create "Auth Discovery" --type=feature --parent=$epic \
  --description="## Overview
Research and design authentication architecture.

## Acceptance Criteria
- [ ] Auth providers evaluated
- [ ] Architecture documented
- [ ] Implementation plan created" --silent)

# 3. Create Implementation Feature (blocked)
impl=$(bd create "Auth Implementation" --type=feature --parent=$epic \
  --description="## Overview
Build authentication system based on discovery findings.

## Acceptance Criteria
- [ ] Login/logout functional
- [ ] OAuth integration working
- [ ] Tests passing" --silent)
bd dep add $impl $disc

# 4. Discovery tasks
research=$(bd create "Research auth providers" --type=task --parent=$disc \
  --description="**OUTPUT**: Provider comparison document

## Research Scope
- Auth0 vs Firebase vs custom
- Cost analysis
- Integration complexity

## Deliverable
Recommended provider with rationale" --silent)

design=$(bd create "Design auth architecture" --type=task --parent=$disc \
  --description="**INCOMING**: Provider recommendation
**OUTGOING**: Architecture diagram

## Acceptance Criteria
- [ ] Token flow documented
- [ ] API endpoints specified
- [ ] Database schema defined" --silent)
bd dep add $design $research
```

### Example: Single Bug

```bash
bug=$(bd create "Login button unresponsive on mobile" --type=bug --priority=1 \
  --parent=$auth_feature \
  --description="## Steps to Reproduce
1. Open app on iOS Safari
2. Navigate to login page
3. Tap 'Login' button

## Expected Behavior
Button should trigger login flow

## Actual Behavior
Nothing happens on tap

## Environment
- iOS 17.2
- Safari

## Acceptance Criteria
- [ ] Login button works on iOS Safari
- [ ] Tested on iOS 16+ devices" --silent)
```

### Example: Single Task

```bash
task=$(bd create "Configure CI pipeline" --type=task --priority=2 \
  --description="**INCOMING**: Repository setup complete
**OUTGOING**: Working CI pipeline

## Description
Set up GitHub Actions for automated testing and deployment.

## Acceptance Criteria
- [ ] Tests run on PR
- [ ] Lint checks pass
- [ ] Build succeeds" --silent)
```

## Output Format

### Success: Single Item
```
Applied WBS to new <type>: <id>
```

### Success: WBS Created
```
Applied WBS to new <root-type>: <root-id>
├── <child-type>: <child-id>
│   └── <grandchild-type>: <grandchild-id>
└── <child-type>: <child-id>
```

### Partial Failure
```
⚠️ WBS partially applied.

Created:
- <type>: <id> ✓

Failed:
- <intended-item>: <error-message>

Recovery:
- Resume with: bd create --parent=<last-success-id> ...
- Or clean up: bdclose <created-ids> --reason="WBS creation failed"
```

### Bug Parent Not Found
```
⚠️ No parent Feature found for bug.

Searched: "<keywords>"
Closest match: None with confidence > 3

Options:
1. Create standalone: bd create "<title>" --type=bug
2. Specify parent: bd create "<title>" --type=bug --parent=<feature-id>
3. Create parent first: bd create "<title>" --type=feature
```

## Quick Reference

| Type | Required Sections | Parent Search | Typical Children |
|------|-------------------|---------------|------------------|
| Epic | Success Criteria | No | Features |
| Feature | Acceptance Criteria | No | Tasks, Bugs, Chores |
| Task | Acceptance Criteria | No | Chores (rare) |
| Bug | Steps to Reproduce, Acceptance | Yes (Feature) | Chores (rare) |
| Chore | None | Optional | None |

## Priority Guidelines

| Priority | When to Use |
|----------|-------------|
| P0 (0) | Blocks other work, critical path |
| P1 (1) | Core functionality, high value |
| P2 (2) | Standard priority (default) |
| P3 (3) | Nice to have, polish |
| P4 (4) | Future consideration, backlog |

## Related Skills

- `/task-execute` - Execute a beads task
- `/beads-cleanup` - Reorganize existing beads structure
- `/beads-workflow` - Multi-step beads operations
