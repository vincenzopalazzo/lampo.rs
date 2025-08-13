# GitHub Copilot PR Review Rules for Lampo.rs

## Commit Message Standards

When reviewing pull requests, ensure all commits follow these standards:

### Subject Line Requirements
- **MUST** complete the sentence: "If applied, this commit will _____"
- **MUST** be capitalized (first letter uppercase)
- **MUST NOT** end with a period
- **MUST** be 50 characters or less
- **MAY** include an optional category prefix (e.g., `cli:`, `node:`, `tests:`, `docs:`)

#### Examples
✅ Good: `Add support for .gif files`
✅ Good: `tests: Modernize CLN integration tests`
❌ Bad: `Adding support for .gif files` (wrong verb form)
❌ Bad: `add support for .gif files` (not capitalized)
❌ Bad: `Add support for .gif files.` (has period)

### Commit Body Guidelines
- **SHOULD** be included for complex changes
- **MUST** be separated from subject by blank line
- **MUST** wrap text at ~72 characters
- **MUST** use imperative mood ("Fix bug" not "Fixed bug")
- **MAY** include bullet points with hanging indent
- **SHOULD** explain the "why" behind changes, not just "what"

### Sign-off Requirements
- **MUST** include DCO sign-off for significant code contributions
- Use `Signed-off-by: Name <email>` format
- Can be added with `git commit -s`

## PR Review Checklist

When reviewing PRs, verify:

### Code Quality
- [ ] Each commit represents a single, focused change
- [ ] No `fixup!` commits present
- [ ] All tests pass for each commit
- [ ] Code follows Rust conventions and project style

### Commit Messages
- [ ] Subject line follows format requirements
- [ ] Complex changes have descriptive commit bodies
- [ ] Imperative mood used throughout
- [ ] Category prefixes used consistently when applicable

### Testing
- [ ] New features include tests
- [ ] Existing tests updated for breaking changes
- [ ] Integration tests cover critical paths
- [ ] No flaky or timing-dependent tests introduced

### Documentation
- [ ] Public APIs documented with rustdoc
- [ ] Complex logic includes inline comments
- [ ] Breaking changes noted in commit message
- [ ] README updated if user-facing changes

## Automated Feedback Templates

### Poor Commit Message
```
The commit message doesn't follow project conventions:
- Subject should complete: "If applied, this commit will _____"
- Use imperative mood (e.g., "Add" not "Added" or "Adding")
- Capitalize first letter
- Keep under 50 characters
- No period at the end

Please amend the commit message to follow the guidelines in CONTRIBUTING.md
```

### Missing Commit Body
```
This appears to be a complex change that would benefit from a commit body explaining:
- Why this change was necessary
- What approach was taken
- Any trade-offs or considerations

Please add a descriptive commit body (separated by blank line from subject).
```

### Missing Tests
```
This PR introduces new functionality but lacks corresponding tests.
Please add tests covering:
- Happy path scenarios
- Error conditions
- Edge cases

Tests should be added in the same commit as the feature when possible.
```

### Commit Organization
```
This PR contains multiple unrelated changes in a single commit.
Please split into separate commits, each addressing a single concern:
- One commit per logical change
- Each commit should compile and pass tests
- Use `git rebase -i` to reorganize if needed
```

## Integration with GitHub Actions

These rules should be enforced through:
1. Commit message linting in CI
2. Automated PR comments for violations
3. Required checks before merge
4. Squash merge policies for external contributors

## References
- Full guidelines: [CONTRIBUTING.md](https://github.com/vincenzopalazzo/lampo.rs/blob/main/CONTRIBUTING.md)
- Rust style guide: https://rust-lang.github.io/api-guidelines/
- Conventional Commits (optional reference): https://www.conventionalcommits.org/