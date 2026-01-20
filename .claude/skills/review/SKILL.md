---
name: review
description: Review a GitHub pull request for a Rust codebase. Focuses on security, performance, test coverage, and Rust idioms. Use when reviewing PRs or evaluating proposed changes.
allowed-tools: Bash(gh:*), Bash(git diff*), Bash(git log*), Bash(git show*), Read, Grep, Glob
---

# Rust Pull Request Review

Review the specified PR with a focus on **high-signal, actionable feedback**. Avoid nitpicks and style preferences unless they impact correctness or maintainability.

## Usage

```
/review [pr_number_or_url]
```

Examples:
- `/review` - reviews the current branch's PR
- `/review 577`
- `/review https://github.com/apollographql/apollo-mcp-server/pull/577`

## Best Practices Reference

Before reviewing, familiarize yourself with Apollo's Rust best practices. Reference these files when providing feedback:

- [Chapter 1 - Coding Styles and Idioms](best-practices/chapter_01.md): Borrowing vs cloning, Copy trait, Option/Result handling, iterators, comments
- [Chapter 2 - Clippy and Linting](best-practices/chapter_02.md): Important lints, fixing vs silencing warnings
- [Chapter 3 - Performance Mindset](best-practices/chapter_03.md): Profiling, avoiding redundant clones, stack vs heap, zero-cost abstractions
- [Chapter 4 - Error Handling](best-practices/chapter_04.md): Result vs panic, thiserror vs anyhow, error hierarchies
- [Chapter 5 - Automated Testing](best-practices/chapter_05.md): Test naming, one assertion per test, snapshot testing
- [Chapter 6 - Generics and Dispatch](best-practices/chapter_06.md): Static vs dynamic dispatch, trait objects
- [Chapter 7 - Type State Pattern](best-practices/chapter_07.md): Compile-time state safety, when to use it
- [Chapter 8 - Comments vs Documentation](best-practices/chapter_08.md): When to comment, doc comments, rustdoc
- [Chapter 9 - Understanding Pointers](best-practices/chapter_09.md): Thread safety, Send/Sync, pointer types

## Context

If a PR number or URL was provided as an argument, use it. Otherwise, default to the current branch's PR.

Fetch PR information:
```bash
# If argument provided (e.g., "577" or full URL):
gh pr view <pr_number_or_url> --json number,title,body,author,baseRefName,headRefName

# If no argument, use current branch:
gh pr view --json number,title,body,author,baseRefName,headRefName
```

Get the PR diff:
```bash
gh pr diff <pr_number_or_url>
```

## Review Process

1. **Understand the change**
   - Run `gh pr diff` or `git diff` to see all changes
   - Read the PR description and linked issues
   - Identify the scope and intent

2. **Manual review** using the criteria below, referencing the best practices documents

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

### When running in GitHub Actions (CI environment)

Post review comments directly on the PR using inline comments on specific lines of code:

```bash
# For line-specific comments:
gh api repos/{owner}/{repo}/pulls/{pr_number}/comments \
  -f body="**[Blocking]** Issue description here. See Chapter X for details." \
  -f commit_id="<commit_sha>" \
  -f path="src/foo.rs" \
  -f line=42

# For general PR comments (summary, test coverage assessment):
gh pr comment {pr_number} --body "## Review Summary\n\n..."
```

Each inline comment should:
- Start with severity: `**[Blocking]**`, `**[Should Fix]**`, or `**[Consider]**`
- Explain the problem clearly
- Reference the relevant best practice chapter when applicable
- Suggest a fix

After posting inline comments, add a summary comment with:
- Overall assessment
- Test coverage assessment
- Final recommendation (Approve / Approve with suggestions / Request changes)

### When running locally

Structure your review as follows:

#### Summary
Brief description of what this PR does and its overall quality.

#### Blocking Issues
Issues that must be fixed before merge. Each item must include:
- File and line reference: `src/foo.rs:42`
- Clear explanation of the problem
- Reference to relevant best practice chapter when applicable
- Suggested fix

#### Should Fix
Strong recommendations that significantly improve the code. Same format as blocking.

#### Consider
Optional improvements. Keep this section minimal to reduce noise.

#### Test Coverage Assessment
- What's tested
- What's missing
- Suggested test cases

#### Recommendation
One of:
- **Approve**: No blocking issues, tests adequate
- **Approve with suggestions**: No blocking issues, but should-fix items exist
- **Request changes**: Blocking issues present

## Guidelines for High-Signal Reviews

- **Be specific**: Always reference exact file and line numbers
- **Explain why**: Don't just say "this is wrong" - explain the consequence
- **Reference best practices**: Link to the relevant chapter when suggesting changes
- **Suggest fixes**: Provide concrete code examples when helpful
- **Skip the obvious**: Don't mention things clippy would catch (run it instead)
- **Focus on this PR**: Don't request unrelated refactoring
- **Acknowledge good work**: Note well-designed solutions briefly
