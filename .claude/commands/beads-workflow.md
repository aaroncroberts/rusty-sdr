---
description: Multi-step workflow automation for beads issue management using function calls
argument-hint: <workflow type or automation task>
tools:
  - beads_list
  - beads_show
  - beads_create
  - beads_update
  - beads_close
  - beads_dep_add
  - beads_stats
  - beads_ready
---

# Beads Workflow Skill

This skill demonstrates **multi-step workflow automation** using beads function calls. It combines read operations (query) with write operations (create, update, close) to automate common issue management workflows.

## Purpose

Provide reference implementations for:
- Automated workflow execution using function calls
- Combining multiple operations into cohesive workflows
- Error handling and rollback strategies
- State transitions and dependency management

## Available Functions

### Read Operations
- `beads_list` - Query issues with filters
- `beads_show` - Get detailed issue information
- `beads_ready` - Find issues ready to work
- `beads_stats` - Project statistics

### Write Operations
- `beads_create` - Create new issues
- `beads_update` - Update issue fields (status, assignee, notes, priority)
- `beads_close` - Close completed issues
- `beads_dep_add` - Add dependencies between issues

## Common Workflow Patterns

### Workflow 1: Start Working on Issue

**Trigger**: User says "start working on X" or "claim issue X"

**Steps**:
```typescript
// 1. Show current details
const issue = await beads_show({ ids: "todd-bishop-abc" });

// 2. Check if issue is ready (no blockers)
if (issue.blockedBy.length > 0) {
  // Inform user about blockers
  return "Issue is blocked by: " + issue.blockedBy.join(", ");
}

// 3. Check if already assigned
if (issue.assignee && issue.assignee !== currentUser) {
  // Ask for confirmation to reassign
}

// 4. Update status to in_progress and assign
await beads_update({
  id: "todd-bishop-abc",
  status: "in_progress",
  assignee: currentUser
});

// 5. Confirm to user
return "You're now working on: " + issue.title;
```

### Workflow 2: Complete Work and Unblock Dependents

**Trigger**: User says "mark X as done" or "close issue X"

**Steps**:
```typescript
// 1. Get issue details to see what it blocks
const issue = await beads_show({ ids: "todd-bishop-abc" });

// 2. Close the issue
await beads_close({
  ids: "todd-bishop-abc",
  reason: "Completed successfully"
});

// 3. Check what was blocked by this issue
if (issue.blocks.length > 0) {
  // Get details of previously blocked issues
  const unblocked = [];
  for (const blockedId of issue.blocks) {
    const blocked = await beads_show({ ids: blockedId });

    // Check if this was the only blocker
    if (blocked.blockedBy.length === 1) {
      unblocked.push(blocked);
    }
  }

  // 4. Inform user about newly unblocked work
  if (unblocked.length > 0) {
    return `Issue closed! This unblocked ${unblocked.length} issue(s): ` +
           unblocked.map(i => i.id + ": " + i.title).join(", ");
  }
}

return "Issue closed successfully.";
```

### Workflow 3: Bulk Status Update

**Trigger**: User says "close all completed tasks from sprint 5"

**Steps**:
```typescript
// 1. Find matching issues
const issues = await beads_list({
  type: "task",
  status: "in_progress",
  labels: "sprint-5"
});

// 2. Filter to only actually completed ones (ask user or check criteria)
const toClose = issues.filter(/* completion criteria */);

// 3. Close all in sequence
const results = [];
for (const issue of toClose) {
  try {
    await beads_close({
      ids: issue.id,
      reason: "Sprint 5 completed"
    });
    results.push({ id: issue.id, success: true });
  } catch (error) {
    results.push({ id: issue.id, success: false, error });
  }
}

// 4. Report results
const succeeded = results.filter(r => r.success).length;
return `Closed ${succeeded}/${toClose.length} issues.`;
```

### Workflow 4: Create Issue with Dependencies

**Trigger**: User says "create task X that depends on Y"

**Steps**:
```typescript
// 1. Verify the dependency exists
const dependency = await beads_show({ ids: "todd-bishop-yyy" });

// 2. Create the new issue
const result = await beads_create({
  title: "Implement feature X",
  type: "task",
  priority: "2",
  description: "Feature implementation with dependency",
  parent: "todd-bishop-epic"
});

// Extract new issue ID from result
const newId = result.id; // e.g., "todd-bishop-zzz"

// 3. Add dependency relationship
await beads_dep_add({
  issue_id: newId,
  depends_on: "todd-bishop-yyy"
});

// 4. Confirm creation
return `Created ${newId} (blocked by ${dependency.id} until completed)`;
```

### Workflow 5: Triage and Prioritize

**Trigger**: User says "triage all new bugs"

**Steps**:
```typescript
// 1. Find new bugs (open, unassigned, no priority)
const newBugs = await beads_list({
  type: "bug",
  status: "open"
  // Filter for unassigned or default priority
});

// 2. For each bug, get details and assess
for (const bug of newBugs) {
  const details = await beads_show({ ids: bug.id });

  // 3. Determine priority based on description/labels
  let priority = "2"; // default
  if (details.description.includes("critical") ||
      details.labels.includes("security")) {
    priority = "0";
  } else if (details.description.includes("user-facing")) {
    priority = "1";
  }

  // 4. Update priority
  await beads_update({
    id: bug.id,
    priority: priority,
    notes: "Triaged: Set priority based on severity"
  });
}

// 5. Summary
return `Triaged ${newBugs.length} bugs.`;
```

### Workflow 6: Sprint Planning

**Trigger**: User says "plan sprint 6 with 5 high-priority tasks"

**Steps**:
```typescript
// 1. Find candidate issues (high priority, ready to work)
const ready = await beads_ready();

// 2. Filter to high-priority only
const candidates = ready.issues.filter(issue =>
  issue.priority === "0" || issue.priority === "1"
);

// 3. Select top N by priority
const selected = candidates.slice(0, 5);

// 4. Update each with sprint label and assignment
for (const issue of selected) {
  await beads_update({
    id: issue.id,
    labels: "sprint-6",
    notes: "Added to Sprint 6"
  });
}

// 5. Create sprint epic if needed
const sprintEpic = await beads_create({
  title: "Epic: Sprint 6",
  type: "epic",
  priority: "1",
  description: `Sprint 6 goals and tracking.
Selected issues: ${selected.map(i => i.id).join(", ")}`
});

// 6. Link issues to sprint epic
for (const issue of selected) {
  await beads_update({
    id: issue.id,
    parent: sprintEpic.id
  });
}

return `Sprint 6 planned with ${selected.length} issues.`;
```

## Error Handling Patterns

### Pattern 1: Validation Before Write

```typescript
// Always validate before making changes
const issue = await beads_show({ ids: issueId });

// Check preconditions
if (issue.status === "completed") {
  return "Error: Issue is already completed.";
}

if (issue.blockedBy.length > 0) {
  return "Error: Cannot start work on blocked issue.";
}

// Proceed with update
await beads_update({ id: issueId, status: "in_progress" });
```

### Pattern 2: Partial Failure Recovery

```typescript
const results = { succeeded: [], failed: [] };

for (const issueId of issueIds) {
  try {
    await beads_close({ ids: issueId });
    results.succeeded.push(issueId);
  } catch (error) {
    results.failed.push({ id: issueId, error: error.message });
  }
}

// Report partial success
if (results.failed.length > 0) {
  return `Completed ${results.succeeded.length}, failed ${results.failed.length}. ` +
         `Failed: ${results.failed.map(f => f.id).join(", ")}`;
}
```

### Pattern 3: Rollback on Failure

```typescript
const createdIds = [];

try {
  // Create multiple related issues
  const epic = await beads_create({ title: "Epic", type: "epic" });
  createdIds.push(epic.id);

  const feature = await beads_create({
    title: "Feature",
    type: "feature",
    parent: epic.id
  });
  createdIds.push(feature.id);

  const task = await beads_create({
    title: "Task",
    type: "task",
    parent: feature.id
  });
  createdIds.push(task.id);

} catch (error) {
  // Rollback: close all created issues
  for (const id of createdIds) {
    await beads_close({
      ids: id,
      reason: "Rollback due to error: " + error.message
    });
  }

  return "Workflow failed and was rolled back.";
}
```

## Example Workflows

### Example 1: Daily Standup Automation

**User**: "Prepare standup report"

**Workflow**:
```typescript
// Get stats
const stats = await beads_stats();

// Get my in-progress work
const myWork = await beads_list({
  status: "in_progress",
  assignee: currentUser
});

// Get completed since yesterday
const completed = await beads_list({
  status: "completed",
  assignee: currentUser
  // Filter by updated: "since yesterday"
});

// Get blocked items
const blocked = await beads_list({
  status: "blocked",
  assignee: currentUser
});

// Format report
return `
📊 Standup Report for ${currentUser}

Yesterday:
${completed.map(i => "✅ " + i.title).join("\n")}

Today:
${myWork.map(i => "🔄 " + i.title).join("\n")}

Blockers:
${blocked.map(i => "🚫 " + i.title + " (blocked by: " + i.blockedBy.join(", ") + ")").join("\n")}
`;
```

### Example 2: Release Preparation

**User**: "Prepare v1.2.0 release"

**Workflow**:
```typescript
// 1. Create release epic
const release = await beads_create({
  title: "Epic: v1.2.0 Release",
  type: "epic",
  priority: "1",
  description: "Track v1.2.0 release preparation"
});

// 2. Create release tasks
const tasks = [
  { title: "Update CHANGELOG.md", priority: "1" },
  { title: "Run full test suite", priority: "0" },
  { title: "Update version in package.json", priority: "1" },
  { title: "Create release notes", priority: "2" },
  { title: "Tag release in git", priority: "0" }
];

const taskIds = [];
for (const task of tasks) {
  const result = await beads_create({
    title: task.title,
    type: "task",
    priority: task.priority,
    parent: release.id,
    labels: "release,v1.2.0"
  });
  taskIds.push(result.id);
}

// 3. Set up dependencies (sequential execution)
for (let i = 1; i < taskIds.length; i++) {
  await beads_dep_add({
    issue_id: taskIds[i],
    depends_on: taskIds[i - 1]
  });
}

// 4. Summary
return `Created release ${release.id} with ${taskIds.length} tasks.
First task (${taskIds[0]}) is ready to start.`;
```

### Example 3: Bug Triage Workflow

**User**: "Triage new bugs from last week"

**Workflow**:
```typescript
// 1. Find new bugs
const newBugs = await beads_list({
  type: "bug",
  status: "open"
  // Filter: created in last week
});

// 2. Categorize by severity (from description/labels)
const critical = [];
const high = [];
const medium = [];

for (const bug of newBugs) {
  const details = await beads_show({ ids: bug.id });

  if (details.labels.includes("security") ||
      details.description.includes("data loss")) {
    critical.push(bug);
  } else if (details.labels.includes("user-facing")) {
    high.push(bug);
  } else {
    medium.push(bug);
  }
}

// 3. Update priorities
for (const bug of critical) {
  await beads_update({
    id: bug.id,
    priority: "0",
    notes: "Triaged as critical"
  });
}

for (const bug of high) {
  await beads_update({
    id: bug.id,
    priority: "1",
    notes: "Triaged as high priority"
  });
}

for (const bug of medium) {
  await beads_update({
    id: bug.id,
    priority: "2",
    notes: "Triaged as medium priority"
  });
}

// 4. Report
return `
Bug Triage Complete:

🔴 Critical (P0): ${critical.length}
🟠 High (P1): ${high.length}
🟡 Medium (P2): ${medium.length}

Total triaged: ${newBugs.length}
`;
```

## Best Practices

1. **Read Before Write**: Always fetch current state before updating
2. **Validate Preconditions**: Check blockers, status, assignee before operations
3. **Handle Partial Failures**: Track successes and failures separately
4. **Use Transactions Where Possible**: Group related operations
5. **Provide Clear Feedback**: Report what was done and what's next
6. **Respect Dependencies**: Don't start work on blocked issues
7. **Update Notes**: Document why changes were made

## Anti-Patterns to Avoid

❌ **Don't skip validation**:
```typescript
// Bad: Updating without checking current state
await beads_update({ id: issueId, status: "in_progress" });
```

✅ **Do validate first**:
```typescript
// Good: Check current state and blockers
const issue = await beads_show({ ids: issueId });
if (issue.blockedBy.length > 0) {
  return "Cannot start: Issue is blocked";
}
await beads_update({ id: issueId, status: "in_progress" });
```

❌ **Don't ignore errors**:
```typescript
// Bad: Fire and forget
for (const id of ids) {
  beads_close({ ids: id }); // No await, no error handling
}
```

✅ **Do track results**:
```typescript
// Good: Await and handle errors
const results = [];
for (const id of ids) {
  try {
    await beads_close({ ids: id });
    results.push({ id, success: true });
  } catch (error) {
    results.push({ id, success: false, error });
  }
}
```

## Function Signatures (Write Operations)

### beads_create

```typescript
beads_create({
  title: string,
  type: "epic" | "feature" | "task" | "bug",
  priority: "0" | "1" | "2" | "3" | "4",
  description?: string,
  assignee?: string,
  parent?: string,
  labels?: string // comma-separated
}): Promise<{ id: string, ... }>
```

### beads_update

```typescript
beads_update({
  id: string,
  status?: "open" | "in_progress" | "completed" | "blocked",
  assignee?: string,
  priority?: "0" | "1" | "2" | "3" | "4",
  notes?: string,
  labels?: string,
  parent?: string
}): Promise<{ success: boolean }>
```

### beads_close

```typescript
beads_close({
  ids: string, // Single or comma-separated
  reason?: string
}): Promise<{ success: boolean, closed: string[] }>
```

### beads_dep_add

```typescript
beads_dep_add({
  issue_id: string,   // The dependent issue (gets blocked)
  depends_on: string  // The blocker issue (must complete first)
}): Promise<{ success: boolean }>
```

## State Transition Rules

Valid status transitions:
- `open` → `in_progress` (start work)
- `in_progress` → `completed` (finish work via close)
- `in_progress` → `open` (pause work)
- `blocked` → `open` (blockers resolved automatically)

Invalid transitions:
- `open` → `completed` (must go through in_progress)
- `completed` → `open` (cannot reopen, create new issue)

## Workflow Testing Checklist

When testing this skill with Gemini:

- [ ] Start work workflow validates blockers
- [ ] Complete work workflow notifies about unblocked issues
- [ ] Bulk updates handle partial failures correctly
- [ ] Create with dependencies establishes correct relationships
- [ ] Triage workflow categorizes and updates correctly
- [ ] Sprint planning selects appropriate issues
- [ ] Error handling provides clear feedback
- [ ] Rollback works when workflow fails mid-way
- [ ] Status transitions follow valid rules
- [ ] Notes are added for audit trail

## Related Skills

- `/beads-query` - Read-only query operations
- `/task-generate` - Generate WBS with automated issue creation
- `/task-execute` - Execute individual beads tasks

## Common User Requests

| User Says | Workflow |
|-----------|----------|
| "Start working on X" | Start Work (validate → update status → assign) |
| "Mark X as done" | Complete Work (close → report unblocked) |
| "Close all sprint 5 tasks" | Bulk Status Update |
| "Create task X depending on Y" | Create with Dependencies |
| "Triage new bugs" | Triage and Prioritize |
| "Plan sprint 6" | Sprint Planning |
| "What did I complete today?" | Daily Standup (read-only) |
| "Prepare release X" | Release Preparation |
