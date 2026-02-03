---
name: review
description: Review a GitHub pull request for a Rust codebase. Focuses on security, performance, test coverage, and Rust idioms. Runs in GitHub Actions and posts comments directly to the PR.
allowed-tools: Bash(gh:*), Bash(git diff*), Bash(git log*), Bash(git show*), Read, Grep, Glob
---

# Rust Pull Request Review

Review the current PR with a focus on **high-signal, actionable feedback**. Avoid nitpicks and style preferences unless they impact correctness or maintainability.

## Usage

```
/review
```

This skill runs in GitHub Actions and automatically reviews the current PR, posting inline comments and a summary directly to GitHub.

## Best Practices Reference

Before reviewing, familiarize yourself with Apollo's Rust best practices by reading the `rust-best-practices` skill. Read ALL relevant chapters in the same turn in parallel. Reference these files when providing feedback:

- [Chapter 1 - Coding Styles and Idioms](../rust-best-practices/references/chapter_01.md): Borrowing vs cloning, Copy trait, Option/Result handling, iterators, comments
- [Chapter 2 - Clippy and Linting](../rust-best-practices/references/chapter_02.md): Clippy configuration, important lints, workspace lint setup
- [Chapter 3 - Performance Mindset](../rust-best-practices/references/chapter_03.md): Profiling, avoiding redundant clones, stack vs heap, zero-cost abstractions
- [Chapter 4 - Error Handling](../rust-best-practices/references/chapter_04.md): Result vs panic, thiserror vs anyhow, error hierarchies
- [Chapter 5 - Automated Testing](../rust-best-practices/references/chapter_05.md): Test naming, one assertion per test, snapshot testing
- [Chapter 6 - Generics and Dispatch](../rust-best-practices/references/chapter_06.md): Static vs dynamic dispatch, trait objects
- [Chapter 7 - Type State Pattern](../rust-best-practices/references/chapter_07.md): Compile-time state safety, when to use it
- [Chapter 8 - Comments vs Documentation](../rust-best-practices/references/chapter_08.md): When to comment, doc comments, rustdoc
- [Chapter 9 - Understanding Pointers](../rust-best-practices/references/chapter_09.md): Thread safety, Send/Sync, pointer types

Additionally, you MUST ALWAYS read @.claude/MEMORIES.md which is a long term memory file of feedback you have been given on your reviews on previous PRs. You should utilize and consider these memories when determining what feedback to leave on the PR.

## Pull Request Context

Fetch PR information:

PR Metadata: !`gh pr view --json number,title,body,author,baseRefName,headRefName,commits,headRepository,headRepositoryOwner`
PR Diff: !`gh pr diff`
PR Level Comments: !`gh pr view --json comments`
Get existing inline review comments: `gh api repos/{owner}/{repo}/pulls/{pr_number}/comments`

## Important: Review Only

**DO NOT:**

- Pull down the branch or checkout the code
- Run tests, clippy, cargo build, or any other commands
- Attempt to run or validate the code locally

All automated checks (tests, linting, clippy, formatting) are handled by GitHub Actions. Focus exclusively on reviewing the code diffs.

## Review Process

**You have a maximum of 10 turns to complete this review.** Allocate turns strategically:

- Turn 1: Gather all context (parallel commands)
- Turns 2-4: Analysis and evaluation
- Turns 5+: Post comments and summary

1. **Gather all context** (single turn - run all commands in parallel)
   - Fetch PR metadata, diff, and all existing comments
   - This gives you complete information before starting analysis

2. **Analyze the changes**
   - Read the PR description and linked issues to understand intent
   - Review the complete diff to identify the scope
   - Review existing feedback to note what's already been raised
   - **Do not duplicate comments** that have already been made
   - Consider context from existing discussions when evaluating code
   - Important: If a user has responded to a comment/issue stating that they will not be following that suggestion, do not argue with the user, and do not bring up that suggestion again within this PR

3. **Manual review** using the criteria below, referencing the best practices documents
   - Focus on high-signal findings only
   - Skip issues already raised by other reviewers
   - Batch findings into cohesive inline comments

## Review Criteria

### Security (Blocking)

- **Unsafe blocks**: Is `unsafe` necessary? Is the invariant documented with `// SAFETY:`? Is there a safe alternative?
- **Input validation**: Is user/external input validated before use?
- **Error exposure**: Are internal errors or stack traces exposed to users?
- **Unwrap on external data**: `unwrap()` or `expect()` on user input, network data, or file contents (see Chapter 4)
- **Secret handling**: Are credentials, tokens, or keys properly handled (not logged, not in errors)?
- **SQL/Command injection**: Is external input used in queries or shell commands without sanitization?

### Performance (Should Fix)

Reference Chapter 1 and Chapter 3 for details:

- **Unnecessary clones**: `.clone()` where a borrow would suffice
- **Allocation in hot paths**: `String` or `Vec` allocations in loops or frequently-called functions
- **Inefficient iterators**: `.collect()` followed by iteration, when chaining would work (see Chapter 1.5)
- **Blocking in async**: Synchronous I/O or heavy computation in async functions without `spawn_blocking`
- **Missing `Cow`**: Could `Cow<str>` or `Cow<[T]>` avoid allocations? (see Chapter 3.2)
- **Large structs by value**: Passing large structs by value instead of reference (see Chapter 3.3)
- **Early allocation**: Using `unwrap_or`, `map_or` instead of `_else` variants when allocation is involved (see Chapter 1.4)

### Test Coverage (Should Fix)

Reference Chapter 5 for testing best practices:

- **New public functions**: Are they tested?
- **Error paths**: Are `Err` cases and edge conditions tested? (see Chapter 4.6)
- **Changed logic**: If behavior changed, are tests updated to reflect it?
- **Integration tests**: For public API changes, are there integration tests?
- **Test naming**: Do test names describe the behavior being tested? (see Chapter 5.1)
- **One assertion per test**: Are tests focused on a single behavior?

### Correctness & Rust Idioms (Blocking/Should Fix)

Reference Chapter 1, 4, and 6:

- **Panics in library code**: `unwrap()`, `expect()`, `panic!()` in library code that should return `Result`
- **Ignored errors**: `let _ = fallible_operation()` without justification
- **Missing `#[must_use]`**: Functions returning `Result` or important values
- **Lock poisoning**: Proper handling of `Mutex`/`RwLock` poisoning (see Chapter 9)
- **Iterator invalidation**: Modifying collections while iterating
- **Integer overflow**: Arithmetic on user-controlled values without checked/saturating ops
- **Lifetime issues**: Unnecessary `'static` bounds, overly restrictive lifetimes
- **Clone traps**: Auto-cloning inside loops, cloning references instead of taking ownership (see Chapter 1.1)

### Error Handling (Should Fix)

Reference Chapter 4:

- **thiserror for libraries**: Are custom errors using `thiserror` with proper `#[from]` and `#[error]` attributes?
- **anyhow in libraries**: Is `anyhow` being used in library code? (should use `thiserror` instead)
- **Error propagation**: Is `?` used instead of verbose `match` chains?
- **Error context**: Are errors descriptive and actionable?

### API Design (Consider)

Reference Chapter 6 and 7:

- **Breaking changes**: Are public API changes intentional and documented?
- **Error types**: Are custom errors descriptive and actionable?
- **Builder pattern**: For structs with many optional fields
- **Naming**: Does it follow Rust conventions (snake_case, clear verb prefixes)?
- **Static vs dynamic dispatch**: Is `dyn Trait` used only when necessary? (see Chapter 6)
- **Type state pattern**: Could compile-time state validation improve safety? (see Chapter 7)

### Documentation (Consider)

Reference Chapter 8:

- **Public API docs**: Are public items documented with `///`?
- **Error/Panic sections**: Do fallible functions document their error conditions?
- **Comments explain why**: Are comments explaining the "why" not the "what"?
- **TODOs have issues**: Are TODO comments linked to tracked issues?

## Output Format

Post review comments directly on the PR using inline comments on specific lines of code:

```bash
# For line-specific comments:
gh api repos/{owner}/{repo}/pulls/{pr_number}/comments \
  -f body="**[Blocking]** Issue description here. See Chapter X for details." \
  -f commit_id="<commit_sha>" \
  -f path="src/foo.rs" \
  -f line=42

# For general PR comments (summary, test coverage assessment):
gh pr comment {pr_number} --body "## Review Summary\n\n...\n\n---\n_Reviewed by Claude Code {model}_"
```

Each inline comment should:

- Start with severity: `**[Blocking]**`, `**[Should Fix]**`, or `**[Consider]**`
- Explain the problem clearly
- Reference the relevant best practice chapter when applicable
- Suggest a fix
- Important: For duplicate findings in multiple locations within a file (E.g. multiple tests have the same issue), leave a single online comment referencing the various places in the file it needs to be fixed instead of leaving several inline comments about the same issue.

After posting inline comments, add a summary comment with:

- Overall assessment (summary of the change, keep this to a couple of sentences maximum)
- Findings (bullet point, concise summary of issues found)
- Test coverage assessment
- Final recommendation (Approve / Approve with suggestions / Request changes)
- Sign-off line at the end: `_Reviewed by Claude Code {model}_` where `{model}` is the current model name (e.g., "Opus 4.5")

If you have already posted a summary comment on this PR previously, any follow up summary comments should be concise and focused on what changed:

- Overall assessment (summary of the changes, keep this to a couple of sentences maximum)
- Any new findings (if any, if none do not include this section)
- Changes to test coverage assessment (if any, if none do not include this section)
- Final recommendation
- Sign-off line at the end: `_Reviewed by Claude Code {model}_` where `{model}` is the current model name (e.g., "Opus 4.5")

## Guidelines for High-Signal Reviews

- **Be specific**: Always reference exact file and line numbers
- **Explain why**: Don't just say "this is wrong" - explain the consequence
- **Reference best practices**: Link to the relevant chapter when suggesting changes
- **Suggest fixes**: Provide concrete code examples when helpful
- **Skip the obvious**: Don't mention things clippy would catch (run it instead)
- **Focus on this PR**: Don't request unrelated refactoring
- **Acknowledge good work**: Note well-designed solutions briefly
- **Don't Overwhelm**: Stick to a maximum of the top 5 most important findings.

## Posting Review Comments

**Batch all findings into a single turn:**

1. Collect all inline comments (blocking, should-fix, and consider issues) during analysis
2. Post all inline comments in one go using multiple `gh api` calls
3. Post the summary comment as the final action

This approach reduces turn count by avoiding iterative comment posting.
