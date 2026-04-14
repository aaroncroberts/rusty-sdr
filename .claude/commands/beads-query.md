---
description: Query beads issues using read-only function calls - demonstrates pure function-calling pattern
argument-hint: <query type or search criteria>
tools:
  - beads_list
  - beads_show
  - beads_stats
  - beads_ready
---

# Beads Query Skill

This skill demonstrates **pure function-calling** for querying beads issue data. It uses only read-only operations and showcases structured function invocation patterns for Gemini.

## Purpose

Provide a reference implementation for:
- Read-only beads queries using function calls
- Combining multiple query functions for complex information needs
- Formatting and presenting query results to users
- Error handling in function-based workflows

## Available Functions

### Query Functions (Read-Only)

1. **beads_list** - List issues with filters
   - Filter by: status, type, priority, assignee, labels, parent
   - Sort and limit results
   - Returns: Array of issue summaries

2. **beads_show** - Show detailed issue information
   - Input: Single issue ID or comma-separated IDs
   - Returns: Full issue details including dependencies, logs, labels

3. **beads_ready** - Show issues ready to work
   - No blockers
   - Returns: Issues with status=open and no blocking dependencies

4. **beads_stats** - Get project statistics
   - Returns: Counts by status, type, priority
   - Includes blocked issue count

## Common Query Patterns

### Pattern 1: Find Work to Do

```typescript
// Step 1: Get ready issues
const ready = await beads_ready();

// Step 2: If user wants details on a specific issue
const details = await beads_show({ ids: "beads-abc" });
```

**Use when**: User asks "what should I work on?" or "show me available tasks"

### Pattern 2: Project Status Check

```typescript
// Step 1: Get overall statistics
const stats = await beads_stats();

// Step 2: Optionally drill into specific categories
const inProgress = await beads_list({
  status: "in_progress",
  sort: "priority"
});
```

**Use when**: User asks "how's the project going?" or "what's our progress?"

### Pattern 3: Issue Investigation

```typescript
// Step 1: Find issues matching criteria
const results = await beads_list({
  type: "bug",
  status: "open",
  priority: "0"
});

// Step 2: Get details on found issues
const details = await beads_show({
  ids: results.map(r => r.id).join(",")
});
```

**Use when**: User asks "show me all P0 bugs" or "what features are blocked?"

### Pattern 4: Dependency Analysis

```typescript
// Step 1: Show issue details
const issue = await beads_show({ ids: "beads-abc" });

// Step 2: For each blocker, get its details
for (const blockerId of issue.blockedBy) {
  const blocker = await beads_show({ ids: blockerId });
  // Present blocker information
}

// Step 3: For each dependent, show what's waiting
for (const dependentId of issue.blocks) {
  const dependent = await beads_show({ ids: dependentId });
  // Present dependent information
}
```

**Use when**: User asks "why is this blocked?" or "what depends on this issue?"

## Response Formatting

After executing function calls, format results for readability:

### For beads_ready

```
📋 Ready to Work (3 issues):

1. todd-bishop-abc [P1 task]
   Fix authentication timeout handling

2. todd-bishop-def [P2 feature]
   Add pagination to issues list

3. todd-bishop-ghi [P0 bug]
   Critical: Session data corruption on logout
```

### For beads_stats

```
📊 Project Statistics:

Status:
  • Open: 12
  • In Progress: 3
  • Completed: 47
  • Blocked: 2

Type:
  • Epic: 2
  • Feature: 8
  • Task: 35
  • Bug: 17

Priority:
  • P0: 2 (critical)
  • P1: 15 (high)
  • P2: 28 (medium)
  • P3+: 17 (low/backlog)
```

### For beads_show

```
📌 todd-bishop-abc: Fix authentication timeout handling

Type: task | Priority: P1 | Status: open
Created: 2025-01-15 | Updated: 2025-01-17

Description:
Users are experiencing session timeouts during active usage.
Need to implement keepalive mechanism.

Acceptance Criteria:
- [ ] Sessions extend on user activity
- [ ] Configurable timeout threshold
- [ ] Warning before timeout
- [ ] Test coverage >80%

Dependencies:
  Blocks: todd-bishop-def (Add session middleware)

Labels: backend, auth, p1
```

## Edge Cases to Handle

### Empty Results

```typescript
const ready = await beads_ready();

if (ready.issues.length === 0) {
  // Inform user: "No issues are currently ready to work.
  // All open issues have blocking dependencies."
}
```

### Invalid Issue ID

```typescript
try {
  const result = await beads_show({ ids: "invalid-id" });
} catch (error) {
  // Inform user: "Issue 'invalid-id' not found."
}
```

### Large Result Sets

```typescript
const allIssues = await beads_list({ status: "open" });

if (allIssues.length > 20) {
  // Suggest filtering: "Found 47 open issues. Would you like to
  // filter by type, priority, or assignee?"
}
```

## Example Interactions

### Example 1: Simple Status Check

**User**: "What's ready to work on?"

**Assistant**:
```typescript
// Execute function call
const result = await beads_ready();

// Format and present
```

Response:
```
You have 3 issues ready to work:

1. todd-bishop-xyz [P0 bug] - Session data corruption
2. todd-bishop-abc [P1 task] - Fix auth timeouts
3. todd-bishop-def [P2 feature] - Add pagination

Would you like details on any of these?
```

### Example 2: Multi-Step Investigation

**User**: "Show me all P0 bugs and tell me what's blocking them"

**Assistant**:
```typescript
// Step 1: Find P0 bugs
const bugs = await beads_list({
  type: "bug",
  priority: "0",
  status: "open"
});

// Step 2: Get details including blockers
for (const bug of bugs) {
  const details = await beads_show({ ids: bug.id });

  // Step 3: If blocked, get blocker details
  if (details.blockedBy.length > 0) {
    for (const blockerId of details.blockedBy) {
      await beads_show({ ids: blockerId });
    }
  }
}
```

Response:
```
Found 2 P0 bugs:

🔴 todd-bishop-xyz: Session data corruption
   Status: BLOCKED by todd-bishop-aaa
   Blocker: [P1 task] Implement session storage refactor

🔴 todd-bishop-mmm: API returning 500 on user search
   Status: Ready to work (no blockers)
```

### Example 3: Project Health Check

**User**: "How are we doing on this sprint?"

**Assistant**:
```typescript
// Get overall stats
const stats = await beads_stats();

// Get in-progress work
const active = await beads_list({ status: "in_progress" });

// Get blocked issues
const blocked = await beads_list({ status: "blocked" });
```

Response:
```
📊 Sprint Health:

Completed: 47 issues
In Progress: 3 issues
Open: 12 issues
Blocked: 2 issues (⚠️ needs attention)

Active Work:
• todd-bishop-abc - Joe (P1 task)
• todd-bishop-def - Sarah (P2 feature)
• todd-bishop-ghi - Unassigned (P0 bug)

Blockers to Resolve:
• todd-bishop-xyz blocked by todd-bishop-aaa
• todd-bishop-mmm blocked by todd-bishop-nnn
```

## Best Practices

1. **Start Broad, Then Narrow**: Use beads_stats or beads_ready first, then drill into specifics
2. **Batch Related Queries**: Get all needed IDs, then fetch details in one call using comma-separated IDs
3. **Format Results**: Always present data in human-readable format, not raw JSON
4. **Suggest Next Steps**: After presenting data, offer relevant follow-up actions
5. **Handle Errors Gracefully**: Validate IDs exist before showing details

## Anti-Patterns to Avoid

❌ **Don't fetch details unnecessarily**:
```typescript
// Bad: Fetching full details when list summary is enough
const issues = await beads_list({ status: "open" });
for (const issue of issues) {
  await beads_show({ ids: issue.id }); // Wasteful!
}
```

✅ **Do use list summaries when possible**:
```typescript
// Good: List provides enough info for summary view
const issues = await beads_list({ status: "open" });
// Present summary, only fetch details if user asks
```

❌ **Don't make redundant calls**:
```typescript
// Bad: Calling stats when list gives you the count
const stats = await beads_stats();
const openCount = stats.status.open;
const openIssues = await beads_list({ status: "open" });
```

✅ **Do use the right tool**:
```typescript
// Good: List already gives you count
const openIssues = await beads_list({ status: "open" });
// Use openIssues.length
```

## Function Signatures

### beads_list

```typescript
beads_list({
  status?: "open" | "in_progress" | "completed" | "blocked",
  type?: "epic" | "feature" | "task" | "bug",
  priority?: "0" | "1" | "2" | "3" | "4",
  assignee?: string,
  labels?: string, // comma-separated
  parent?: string,
  sort?: "priority" | "created" | "updated",
  limit?: number
}): Promise<Issue[]>
```

### beads_show

```typescript
beads_show({
  ids: string // Single ID or comma-separated IDs
}): Promise<IssueDetails | IssueDetails[]>
```

### beads_ready

```typescript
beads_ready(): Promise<{
  issues: Issue[],
  count: number
}>
```

### beads_stats

```typescript
beads_stats(): Promise<{
  status: { open: number, in_progress: number, completed: number },
  type: { epic: number, feature: number, task: number, bug: number },
  priority: { p0: number, p1: number, p2: number, p3: number, p4: number },
  blocked: number
}>
```

## Testing Checklist

When testing this skill with Gemini:

- [ ] beads_ready returns correct ready issues
- [ ] beads_list filters work correctly (status, type, priority)
- [ ] beads_show handles single and multiple IDs
- [ ] beads_stats returns accurate counts
- [ ] Error handling for invalid IDs works
- [ ] Results are formatted in human-readable way
- [ ] Multi-step queries work correctly (e.g., dependency analysis)
- [ ] Edge cases handled (empty results, large result sets)

## Related Skills

- `/beads-workflow` - Multi-step workflow automation with write operations
- `/task-generate` - Create WBS structures using beads functions
- `/task-execute` - Execute beads tasks with Claude Code integration
