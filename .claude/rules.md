# lib-todd-data Agent Rules

## Branch Management

### Working Branch
- **ALWAYS work on `aaron/agentic-coder` branch**
- Never work directly on `main` branch
- Never delete the `aaron/agentic-coder` branch after merge

### Branch Protection
```bash
# Verify you're on the correct branch before any work
git branch --show-current  # Must show: aaron/agentic-coder
```

## Pre-Commit Discipline

### Required Checks Before ANY Commit

**MUST pass ALL of the following in order:**

1. **Compile Check**
   ```bash
   cd packages/todd-data && pnpm build
   ```
   - Both ESM and CJS must compile without errors
   - Zero TypeScript errors allowed

2. **Type Check**
   ```bash
   pnpm typecheck
   ```
   - Must pass with no type errors

3. **Lint Check**
   ```bash
   pnpm lint
   ```
   - Must pass all linting rules
   - Fix any linting errors before commit

4. **Test Suite**
   ```bash
   pnpm test
   ```
   - All tests must pass
   - Test coverage must be maintained (>80% for new code)

### Pre-Commit Checklist

```
[ ] 1. Build succeeds (pnpm build)
[ ] 2. Type check passes (pnpm typecheck)
[ ] 3. Lint passes (pnpm lint)
[ ] 4. Tests pass (pnpm test)
[ ] 5. Beads synced (bd sync)
[ ] 6. Commit message written to file
[ ] 7. All changes staged (git add -A)
[ ] 8. Commit with message from file
[ ] 9. Beads synced again (bd sync)
```

## Commit Message Strategy

### ALWAYS Use File-Based Commit Messages

**DO NOT** write commit messages directly in the command line with `-m` flag.

**ALWAYS** use this workflow:

```bash
# 1. Write commit message to temporary file
cat > /tmp/commit-msg.txt << 'EOF'
feat: implement D1 adapter with SQLite support

- Create D1Adapter implementing StorageAdapter interface
- Add SQL query builder for filter translation
- Implement local testing with better-sqlite3
- Add comprehensive D1 adapter tests
- Achieve >85% test coverage

Closes lib-todd-data-xyz
EOF

# 2. Review the message
cat /tmp/commit-msg.txt

# 3. Stage all changes
git add -A

# 4. Commit using the file
git commit -F /tmp/commit-msg.txt

# 5. Clean up
rm /tmp/commit-msg.txt
```

### Commit Message Format

Follow Conventional Commits:

```
<type>(<scope>): <subject>

<body>

<footer>
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation only
- `style`: Formatting, missing semicolons, etc
- `refactor`: Code change that neither fixes a bug nor adds a feature
- `test`: Adding or updating tests
- `chore`: Build process or auxiliary tool changes

**Footer:**
- `Closes lib-todd-data-XXX` - Reference beads issues
- `BREAKING CHANGE:` - Breaking changes

## Release Process

### Pre-Release Requirements

**MUST complete these steps in order:**

1. **Sync with Main**
   ```bash
   # 1. Ensure working branch is clean
   git status
   
   # 2. Fetch latest from remote
   git fetch origin
   
   # 3. Pull main into your branch
   git pull origin main
   ```

2. **Resolve Conflicts (if any)**
   ```bash
   # Resolve all merge conflicts
   # Test EVERYTHING after conflict resolution
   pnpm build
   pnpm typecheck
   pnpm lint
   pnpm test
   ```

3. **Verify Everything Works**
   - All builds pass
   - All tests pass
   - All type checks pass
   - All lints pass

4. **Get Authorization**
   - **MUST get explicit authorization from Aaron before proceeding with release**
   - Do NOT create releases without authorization

5. **Create Release PR**
   ```bash
   # After authorization and all checks pass:
   git push origin aaron/agentic-coder
   
   # Create PR via GitHub UI or gh CLI
   gh pr create --base main --head aaron/agentic-coder \
     --title "Release v0.X.X" \
     --body-file /tmp/release-notes.md
   ```

### Release Artifacts

Our CI/CD will automatically:
- Build the package
- Run all quality checks
- Create GitHub Release with artifacts
- Publish to GitHub iCitadel package registry (@icitadel scope)

## CI/CD Pipeline

### CI (Continuous Integration)

Runs on every push and PR to `aaron/agentic-coder` and `main`:

1. **Build** - `pnpm build`
2. **Type Check** - `pnpm typecheck`
3. **Lint** - `pnpm lint`
4. **Test** - `pnpm test`

All must pass before PR can be merged.

### CD (Continuous Deployment)

Runs on merge to `main` or manual release trigger:

1. **Build artifacts**
2. **Create GitHub Release**
3. **Attach build artifacts**
4. **Publish to GitHub Packages**
   - Registry: `https://npm.pkg.github.com`
   - Scope: `@icitadel`
   - Package: `@icitadel/todd-data`

## Session Close Protocol

**CRITICAL**: Before ending any session, complete this checklist:

```bash
# 1. Check status
git status

# 2. Stage all changes
git add -A

# 3. Sync beads
bd sync

# 4. Write commit message to file
cat > /tmp/commit-msg.txt << 'EOF'
[your commit message]
EOF

# 5. Commit
git commit -F /tmp/commit-msg.txt && rm /tmp/commit-msg.txt

# 6. Sync beads again
bd sync

# 7. Push to remote
git push origin aaron/agentic-coder
```

**NEVER** say "done" or "complete" without pushing to remote.

## Common Commands Reference

### Quick Check All
```bash
# Run all pre-commit checks
pnpm build && pnpm typecheck && pnpm lint && pnpm test
```

### Beads Workflow
```bash
# Check ready work
bd ready

# Claim task
bd update <id> --status=in_progress

# Close task
bd close <id>

# Sync (do this before AND after commits)
bd sync
```

### Git Workflow
```bash
# Always verify branch
git branch --show-current

# Check status
git status

# Stage all
git add -A

# Commit from file
git commit -F /tmp/commit-msg.txt

# Push
git push origin aaron/agentic-coder
```

## Error Recovery

### If Build Fails
1. Read the error carefully
2. Fix the TypeScript/build error
3. Re-run build
4. DO NOT commit until build succeeds

### If Tests Fail
1. Investigate the failing test
2. Fix the issue (code or test)
3. Re-run tests
4. DO NOT commit until tests pass

### If Lint Fails
1. Run `pnpm lint` to see errors
2. Fix linting issues
3. Re-run lint
4. DO NOT commit until lint passes

### If You Accidentally Commit
```bash
# Undo last commit (keeps changes)
git reset --soft HEAD~1

# Fix the issues
# Then commit properly
```

## Key Principles

1. **Quality First**: Never sacrifice quality for speed
2. **Test Everything**: All code changes must have tests
3. **Document Changes**: Update docs when behavior changes
4. **Incremental Progress**: Commit working increments, not broken code
5. **Clear Communication**: Commit messages explain WHY, not just WHAT
6. **Branch Discipline**: Always work on `aaron/agentic-coder`
7. **Authorization Required**: Get approval before releases
8. **CI/CD Trust**: Let the pipeline validate your work

## Remember

- You are a professional engineer
- Quality and correctness matter
- The discipline ensures smooth workflows
- Following these rules prevents problems
- Aaron trusts you to maintain high standards
