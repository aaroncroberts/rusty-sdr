# Claude Code Settings Configuration Reference

Claude Code uses a hierarchical JSON-based configuration system with four precedence levels, hook-based automation, and granular permissions. Settings files control everything from tool permissions to MCP server connections and automated code formatting. **Project-level `.claude/settings.json`** shares team configurations, while **`.claude/settings.local.json`** holds personal overrides that are automatically gitignored.

## File locations and precedence

| Precedence | Location | Purpose | Version Control |
|------------|----------|---------|-----------------|
| 1 (highest) | Enterprise managed paths¹ | Org-wide policies (cannot override) | N/A |
| 2 | CLI arguments | Session-specific overrides | N/A |
| 3 | `.claude/settings.local.json` | Personal project settings | ❌ Auto-gitignored |
| 4 | `.claude/settings.json` | Team-shared project settings | ✅ Committed |
| 5 (lowest) | `~/.claude/settings.json` | User-wide global settings | ❌ Not committed |

**Enterprise managed paths:**
- macOS: `/Library/Application Support/ClaudeCode/managed-settings.json`
- Linux/WSL: `/etc/claude-code/managed-settings.json`
- Windows: `C:\Program Files\ClaudeCode\managed-settings.json`

**Other configuration files:**
- `~/.claude.json` — User preferences, OAuth, MCP servers, project state
- `.mcp.json` — Project-scoped MCP servers (in project root)

---

## Complete settings schema

```json
{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "model": "claude-sonnet-4-20250514",
  "cleanupPeriodDays": 30,
  "includeCoAuthoredBy": true,
  "outputStyle": "Explanatory",
  "autoUpdates": true,
  "verbose": false,
  
  "env": {
    "NODE_ENV": "development",
    "CUSTOM_VAR": "value"
  },
  
  "permissions": {
    "allow": ["Bash(npm run lint)", "Read(~/.zshrc)"],
    "ask": ["Bash(git push:*)"],
    "deny": ["WebFetch", "Read(./.env)", "Bash(curl:*)"],
    "additionalDirectories": ["../docs/"],
    "defaultMode": "acceptEdits",
    "disableBypassPermissionsMode": "disable"
  },
  
  "hooks": {
    "PostToolUse": [{
      "matcher": "Edit|Write",
      "hooks": [{"type": "command", "command": "prettier --write $CLAUDE_FILE_PATHS", "timeout": 30}]
    }]
  },
  
  "sandbox": {
    "enabled": true,
    "autoAllowBashIfSandboxed": true,
    "excludedCommands": ["docker", "git"],
    "network": {
      "allowUnixSockets": ["/var/run/docker.sock"],
      "allowLocalBinding": true
    }
  }
}
```

### All configuration keys

| Key | Type | Description |
|-----|------|-------------|
| `$schema` | string | JSON schema reference |
| `model` | string | Override default model |
| `cleanupPeriodDays` | integer | Session retention (0=disable, default: 30) |
| `includeCoAuthoredBy` | boolean | Git commit attribution (default: true) |
| `outputStyle` | string | System prompt output style |
| `env` | object | Environment variables for sessions |
| `permissions` | object | Permission rules (see below) |
| `hooks` | object | Pre/post tool execution commands |
| `sandbox` | object | Bash sandboxing configuration |
| `apiKeyHelper` | string | Script to generate auth values |
| `forceLoginMethod` | `"claudeai"` \| `"console"` | Restrict login type |
| `companyAnnouncements` | string[] | Startup announcements |
| `statusLine` | object | Custom status line config |
| `disableAllHooks` | boolean | Disable all hooks |
| `enableAllProjectMcpServers` | boolean | Auto-approve all MCP servers |

---

## Permissions configuration

### Rule precedence
`deny` → `allow` → `ask` → `defaultMode`

### Permission modes

| Mode | Behavior |
|------|----------|
| `default` | Allows reads, prompts for other operations |
| `plan` | Read-only analysis |
| `acceptEdits` | Auto-approve file edits, prompt for commands |
| `bypassPermissions` | Skip all permission checks (dangerous) |

### Tool permission patterns

| Tool | Pattern Examples |
|------|------------------|
| `Bash` | `Bash(npm run test:*)`, `Bash(git:*)` |
| `Read` | `Read(./.env)`, `Read(./secrets/**)` |
| `Write` | `Write(./production.*)` |
| `Edit` | `Edit(/docs/**)` |
| `WebFetch` | `WebFetch(domain:api.example.com)` |
| MCP tools | `mcp__puppeteer__*` |

```json
{
  "permissions": {
    "allow": [
      "Read", "Grep", "Glob",
      "Bash(npm run lint)", "Bash(npm run test:*)",
      "Bash(git log:*)", "Bash(git status)"
    ],
    "deny": [
      "Read(./.env)", "Read(./.env.*)", "Read(./secrets/**)",
      "Bash(curl:*)", "Bash(rm -rf:*)", "WebFetch"
    ],
    "ask": ["Write(*)"],
    "additionalDirectories": ["../shared-libs/"]
  }
}
```

---

## Hook system

### Available hook events

| Hook Type | Supports Matchers | Fires When |
|-----------|-------------------|------------|
| `PreToolUse` | ✅ Tool names | Before tool execution |
| `PostToolUse` | ✅ Tool names | After tool completes |
| `PermissionRequest` | ✅ Tool names | During permission dialog |
| `UserPromptSubmit` | ❌ | Before prompt processing |
| `Stop` | ❌ | Main agent finishes |
| `SubagentStop` | ❌ | Subagent (Task) finishes |
| `SessionStart` | ✅ `startup`, `resume`, `clear`, `compact` | Session starts/resumes |
| `SessionEnd` | ❌ | Session ends |
| `PreCompact` | ✅ `manual`, `auto` | Before compaction |
| `Notification` | ✅ Notification types | When notifications sent |

### Hook configuration syntax

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Bash",
      "hooks": [{
        "type": "command",
        "command": ".claude/hooks/validate-bash.sh",
        "timeout": 60
      }]
    }],
    "PostToolUse": [{
      "matcher": "Edit|MultiEdit|Write",
      "hooks": [{
        "type": "command",
        "command": "npx prettier --write $CLAUDE_FILE_PATHS"
      }]
    }],
    "Stop": [{
      "hooks": [{
        "type": "command",
        "command": ".claude/hooks/quality-check.sh"
      }]
    }]
  }
}
```

### Hook exit codes

| Exit Code | Behavior |
|-----------|----------|
| **0** | Success — stdout shown in verbose mode |
| **2** | Blocking error — stderr fed to Claude |
| **Other** | Non-blocking — stderr shown, continues |

### Environment variables in hooks

| Variable | Description |
|----------|-------------|
| `CLAUDE_PROJECT_DIR` | Absolute project root path |
| `CLAUDE_FILE_PATHS` | Space-separated relevant files |
| `CLAUDE_TOOL_OUTPUT` | Tool output (PostToolUse only) |
| `CLAUDE_ENV_FILE` | Env persistence file (SessionStart only) |

---

## MCP server configuration

MCP servers are configured in `~/.claude.json` (user scope) or `.mcp.json` (project scope).

### Server types

```json
{
  "mcpServers": {
    "filesystem": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
    },
    "notion": {
      "type": "http",
      "url": "https://mcp.notion.com/mcp",
      "headers": {"Authorization": "Bearer ${NOTION_TOKEN}"}
    },
    "postgres": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-postgres"],
      "env": {"POSTGRES_CONNECTION_STRING": "${DATABASE_URL}"}
    }
  }
}
```

### CLI commands

```bash
claude mcp add --transport http notion https://mcp.notion.com/mcp
claude mcp add --transport stdio github --env GITHUB_TOKEN=xxx -- npx -y @modelcontextprotocol/server-github
claude mcp list
claude mcp remove server-name
/mcp  # Check status inside Claude Code
```

---

## .settings.json vs .settings.local.json

| Aspect | `.claude/settings.json` | `.claude/settings.local.json` |
|--------|-------------------------|-------------------------------|
| Purpose | Team-shared project settings | Personal project preferences |
| Version control | ✅ Committed to git | ❌ Automatically gitignored |
| Precedence | Lower (4th) | Higher (3rd) |
| Use case | Shared permissions, project rules | Local experiments, personal overrides |

**Recommended pattern:**
- `.settings.json`: Team-wide tool permissions, shared hooks, project standards
- `.settings.local.json`: Personal model preferences, experimental features, local paths

---

## Custom slash commands

Place Markdown files in `.claude/commands/` (project) or `~/.claude/commands/` (user).

```markdown
<!-- .claude/commands/fix-issue.md -->
---
description: Fix a GitHub issue
allowed-tools: Read, Grep, Glob, Bash(gh:*)
---
Fix issue #$1 with priority $2.
Use `gh issue view` to get details then implement the fix.
```

Namespaced commands: `.claude/commands/test/generate.md` → `/test:generate`

---

## Anti-patterns to avoid

| Don't | Do |
|-------|-----|
| Commit API keys in `.mcp.json` | Use `${ENV_VAR}` expansion |
| Put `.mcp.json` inside `.claude/` | Place in project root |
| Skip `cmd /c` wrapper on Windows | `"command": "cmd", "args": ["/c", "npx", ...]` |
| Trust hook inputs blindly | Validate and sanitize all inputs |
| Use unquoted shell variables | Always `"$VAR"` not `$VAR` |

---

## Official resources

| Resource | URL |
|----------|-----|
| GitHub Repository | https://github.com/anthropics/claude-code |
| Settings Documentation | https://code.claude.com/docs/en/settings |
| Hooks Reference | https://code.claude.com/docs/en/hooks |
| MCP Documentation | https://code.claude.com/docs/en/mcp |
| JSON Schema | https://json.schemastore.org/claude-code-settings.json |
| Best Practices | https://www.anthropic.com/engineering/claude-code-best-practices |
| Slash Commands | https://code.claude.com/docs/en/slash-commands |
| Security Guide | https://code.claude.com/docs/en/security |

## Conclusion

The Claude Code settings system provides **layered configuration** through four precedence levels, enabling team-shared standards while preserving individual flexibility. The **hook system's 10 event types** enable powerful automation—from pre-execution validation to post-edit formatting. For security-sensitive environments, combine **granular permission rules** with **sandbox configuration** and **enterprise managed policies**. The separation between `.settings.json` (committed) and `.settings.local.json` (gitignored) solves the collaboration-vs-personalization tension elegantly. All MCP server configuration belongs in `.mcp.json` at the project root, using environment variable expansion for credentials.