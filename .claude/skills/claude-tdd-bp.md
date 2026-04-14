# CLAUDE.md TDD & Code Quality Configuration Guide

Claude Code's CLAUDE.md file is the **highest-leverage configuration point** for AI-assisted development—it's loaded into every conversation, making well-crafted TDD and code quality rules essential for consistent, high-quality output. The key insight from research: **linters provide rules; documentation provides hints**—AI can ignore suggestions but cannot bypass CI pipeline failures. Combine CLAUDE.md directives with hooks and automated tooling for enforceable quality gates.

## Core principles

- **Keep CLAUDE.md under 300 lines**; root file ideally <60 lines—every token consumes context
- **Use hooks for deterministic enforcement**, CLAUDE.md for behavioral guidance
- **Never send an LLM to do a linter's job**—code style belongs in tooling, not instructions
- **Use progressive disclosure**—tell Claude where to find info, not all info upfront
- **Add emphasis with "IMPORTANT" or "YOU MUST"** for critical rules only
- **One instruction per bullet**—short, declarative, unambiguous
- **Test behavior, not implementation**—AI-generated mocks often test internals incorrectly
- **Enforce context isolation** in TDD—use subagents to prevent "cheating" on test design
- **Reference files with `@path/to/file`** instead of copying snippets that become stale
- **Commit to version control**—share team context via git; use `.local.md` for personal prefs

## TDD configuration template

```markdown
# Project: [Name]

## Commands
- `npm test` - Run tests
- `npm run lint` - Run ESLint + Prettier check
- `npm run typecheck` - TypeScript strict mode

## Testing Standards
- Framework: Vitest + Testing Library
- Coverage: 80% lines, 75% branches minimum
- Test files: `*.test.ts` alongside source

## TDD Workflow (MANDATORY)
1. RED: Write ONE failing test defining expected behavior
2. GREEN: Write minimal code to pass—nothing more
3. REFACTOR: Improve while tests stay green
4. IMPORTANT: NEVER write implementation before tests exist

## Code Style
- ES modules only (import/export), never CommonJS
- Type hints required on all functions
- Max function length: 50 lines
- Cyclomatic complexity: ≤10

## Do Not
- Commit directly to `main`
- Mock internal modules—test integration instead
- Write multiple tests before implementation
```

## Code quality rules

### Complexity thresholds
| Metric | Standard | Strict (AI code) |
|--------|----------|------------------|
| Cyclomatic complexity | ≤15 | ≤10 |
| Cognitive complexity | ≤15 | ≤10 |
| Function length | 50 lines | 20 lines |
| Max parameters | 5 | 3-4 |
| Max nesting depth | 4 | 3 |

### Documentation requirements
| Language | Tool | Requirement |
|----------|------|-------------|
| TypeScript | ESLint + jsdoc plugin | JSDoc on all public APIs |
| Python | Ruff + pydocstyle | Google-style docstrings |
| Go | golint | Package + exported function comments |
| Rust | rustdoc | `///` on public items |

### Coverage thresholds
| Metric | Minimum | Critical paths |
|--------|---------|----------------|
| Line | 80% | 100% |
| Branch | 75% | 100% |
| Function | 90% | 100% |

## Language-specific configs

### TypeScript (ESLint)
```json
{
  "rules": {
    "@typescript-eslint/explicit-function-return-type": "error",
    "@typescript-eslint/no-explicit-any": "error",
    "complexity": ["error", 10],
    "max-lines-per-function": ["error", { "max": 50 }],
    "max-depth": ["error", 4]
  }
}
```

### Python (pyproject.toml)
```toml
[tool.ruff]
line-length = 88
select = ["E", "F", "I", "N", "UP", "S", "B", "C4", "PL", "RUF"]

[tool.ruff.mccabe]
max-complexity = 10

[tool.mypy]
strict = true
disallow_untyped_defs = true

[tool.pytest.ini_options]
addopts = "--cov=src --cov-fail-under=80"
```

### Go (.golangci.yml)
```yaml
linters:
  enable: [gocyclo, gocognit, gocritic, errcheck, staticcheck]
linters-settings:
  gocyclo:
    min-complexity: 15
  funlen:
    lines: 60
```

## Common patterns

### TDD slash command (`.claude/commands/tdd.md`)
```markdown
Follow strict TDD for: $ARGUMENTS

1. Write a failing test defining expected behavior
2. Run test, verify it fails for the RIGHT reason
3. Write minimal code to pass
4. Run tests, verify passing
5. Refactor if needed
6. Commit with descriptive message

CRITICAL: No implementation before Step 1 completes.
```

### TDD Guard hook (`.claude/settings.json`)
```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Write|Edit|MultiEdit",
      "hooks": [{
        "type": "command",
        "command": "tdd-guard"
      }]
    }]
  }
}
```

### Hierarchical structure (monorepos)
```
project/
├── CLAUDE.md                  # Shared: commands, core patterns
├── frontend/
│   └── CLAUDE.md             # React/TS specifics
├── backend/
│   └── CLAUDE.md             # Python/API specifics
└── .claude/
    ├── settings.json          # Hooks, permissions
    └── commands/
        └── tdd.md            # TDD workflow
```

## Anti-patterns

| Anti-pattern | Problem | Solution |
|--------------|---------|----------|
| Bloated CLAUDE.md (>10k words) | Degrades model performance | Keep <300 lines; use imports |
| Code style in CLAUDE.md | LLMs are slow/expensive linters | Use hooks + formatters |
| Auto-generated via `/init` | Lacks precision | Manually craft every line |
| Negative-only constraints | Agent gets stuck | Provide alternatives |
| Mocking internal modules | Tests implementation, not behavior | Use integration tests |
| All TDD phases in one context | AI "cheats" on test design | Use subagents for isolation |
| Copying code snippets | Becomes stale | Reference with `@path/to/file` |

## Integration examples

### Pre-commit hooks (`.pre-commit-config.yaml`)
```yaml
repos:
  - repo: https://github.com/astral-sh/ruff-pre-commit
    rev: v0.1.6
    hooks:
      - id: ruff
        args: [--fix]
      - id: ruff-format

  - repo: https://github.com/pre-commit/mirrors-eslint
    rev: v8.54.0
    hooks:
      - id: eslint
        types: [typescript]

  - repo: https://github.com/gtkacz/coverage-pre-commit
    rev: v0.1.0
    hooks:
      - id: coverage-pre-commit
        args: [--fail-under=80]
```

### GitHub Actions quality gate
```yaml
name: Quality Gate
on: [pull_request]

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npm ci
      - run: npm run lint
      - run: npm run typecheck
      - run: npm run test -- --coverage --coverageThreshold='{"global":{"lines":80}}'
```

### Husky + lint-staged (`package.json`)
```json
{
  "lint-staged": {
    "*.{ts,tsx}": ["eslint --fix", "prettier --write"],
    "*.{json,md}": ["prettier --write"]
  }
}
```

## Key resources

| Resource | URL |
|----------|-----|
| TDD Guard | github.com/nizos/tdd-guard |
| Claude-Flow Templates | github.com/ruvnet/claude-flow/wiki/CLAUDE-MD-Templates |
| CLAUDE.md Examples | github.com/ArthurClune/claude-md-examples |
| Awesome Claude Code | github.com/hesreallyhim/awesome-claude-code |
| MCP SDK CLAUDE.md | github.com/modelcontextprotocol/python-sdk/blob/main/CLAUDE.md |
| Anthropic Best Practices | anthropic.com/engineering/claude-code-best-practices |

## Conclusion

Effective CLAUDE.md configurations combine **behavioral guidance** (TDD workflow, architecture decisions) with **automated enforcement** (hooks, linters, CI gates). The file hierarchy supports progressive disclosure—root-level rules apply universally while subdirectory files contain domain-specific context. For TDD specifically, **TDD Guard** provides real-time enforcement via hooks, blocking implementation before tests exist. Keep instructions concise, use emphasis sparingly, and iterate based on observed behavior. The goal is enforceable quality gates, not aspirational documentation.