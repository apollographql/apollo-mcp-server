![ci workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/ci.yml)
![release binaries workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-bins.yml?label=release%20binaries)
![release container workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-container.yml?label=release%20container)
![version](https://img.shields.io/github/v/release/apollographql/apollo-mcp-server)
![license](https://img.shields.io/github/license/apollographql/apollo-mcp-server)

## How to contribute to Apollo MCP Server

Excited about Apollo MCP Server and want to make it better? We're excited too! We welcome anyone who wants to contribute or provide constructive feedback, no matter their level of experience.

> [!IMPORTANT]
> **Please do not open a pull request before the team has had a chance to discuss the issue.** We appreciate the enthusiasm, but unsolicited PRs (especially for features or non-trivial changes) will be closed if there hasn't been prior agreement on the approach. The workflow is: **open an issue first, wait for a maintainer to respond, and only start coding once we've agreed on a path forward.** This saves everyone's time and keeps the project moving in the right direction.

### PR quality expectations

Every pull request should reflect a genuine understanding of the problem and the codebase. We will close PRs that:

* Were submitted without a prior issue discussion or maintainer sign-off (exception: small bug fixes under ~20 lines with tests, as described below).
* Contain low-quality or AI-generated code that hasn't been reviewed, tested, or adapted to the project's conventions by the author.
* Make broad, speculative changes unrelated to a specific agreed-upon issue.

We're not against using AI tools to assist your work, but you are responsible for every line of code you submit. If you can't explain your changes, debug failures, or respond to review feedback, the PR isn't ready. **Authorship means accountability.**

### Bug Reporting

> [!WARNING]  
> **Do not open up a GitHub issue if the bug is a security vulnerability**, and instead refer to our [security policy](https://github.com/apollographql/.github/blob/main/SECURITY.md).
* **Ensure the bug was not already reported** by searching on GitHub under [Issues](https://github.com/apollographql/apollo-mcp-server/issues) as well as the [Apollo Community forums](https://community.apollographql.com/latest).
* If you're unable to find an open issue addressing the problem, [open a new one](https://github.com/apollographql/apollo-mcp-server/issues/new). Be sure to include a **title and clear description**, as much relevant information as possible, and a **code sample** or an **executable test case** demonstrating the expected behavior that is not occurring.
* If appropriate, add the most relevant label but leave empty if unsure.

### Did you write a patch that fixes a bug?

Small, focused bug fixes (under ~20 lines of code) with accompanying tests can be submitted directly as a pull request. For anything larger:

1. **Open an issue first** (or find an existing one) and describe the bug and your proposed fix.
2. **Wait for a maintainer to respond.** We may already be working on it, or we may want to discuss the approach before code is written.
3. Once there's agreement, branch off `main` per our [branching guide](#branching-strategy) and submit your PR.

* Ensure the PR description clearly describes the problem and solution. Include the relevant issue number.
* Before submitting, please read the [branching strategy](#branching-strategy) and [code review guidelines](#code-review-guidelines) to learn more about our coding conventions and review process.

### Do you intend to add a new feature or change an existing one?

New features require consensus before implementation. Please follow this workflow:

1. **Open an issue** describing the feature using the `enhancement` label, or start a discussion on the [Apollo Community forums](https://community.apollographql.com/latest).
2. **Wait for the team to weigh in.** We discuss feature proposals internally and need time to evaluate how they fit into the project's roadmap. A maintainer will respond on the issue when we're ready.
3. **Get agreement on the approach** before writing code. This includes the scope of the change, the implementation strategy, and any architectural considerations.
4. **Submit your PR** with a clear description of the feature and links back to the discussion.

> [!WARNING]
> **Do not open a PR for a feature that hasn't been discussed and approved in a GitHub issue or Community forum thread.** Even if the implementation looks correct, PRs without prior agreement will be closed. We know it can be tempting to jump straight to code, but aligning on the "what" and "why" first leads to better outcomes for everyone.

* Before submitting, please read the [branching strategy](#branching-strategy) and [code review guidelines](#code-review-guidelines).

### Do you have questions about the code or about Apollo MCP Server itself?

* Ask any question about Apollo MCP Server using either the [issues](https://github.com/apollographql/apollo-mcp-server/issues) page or the [Apollo Community forums](https://community.apollographql.com/latest). 
* If using the issues page, please use the `question` label.

Thanks!

Apollo MCP Server team

---

### Code of Conduct

Please refer to our [code of conduct policy](https://github.com/apollographql/router/blob/dev/CONTRIBUTING.md#code-of-conduct).

---

### Branching strategy
The Apollo MCP Server project follows a trunk-based branch strategy.

1. All feature/bug fix/patch work should branch off the `main` branch.

### Code review guidelines
It’s important that every piece of code in Apollo packages is reviewed by at least one core contributor familiar with that codebase. Here are some things we look for:

1. Required CI checks pass. This is a prerequisite for the review, and it is the PR author's responsibility. As long as the tests don’t pass, the PR won't get reviewed.
2. Simplicity. Is this the simplest way to achieve the intended goal? If there are too many files, redundant functions, or complex lines of code, suggest a simpler way to do the same thing. In particular, avoid implementing an overly general solution when a simple, small, and pragmatic fix will do.
3. Testing. Please make sure that the tests ensure that the code won’t break when other stuff change around it. The error messages in the test should help identify what is broken exactly and how. The tests should test every edge case if possible. Please make sure you get as much coverage as possible.
4. No unnecessary or unrelated changes. PRs shouldn’t come with random formatting changes, especially in unrelated parts of the code. If there is some refactoring that needs to be done, it should be in a separate PR from a bug fix or feature, if possible.
5. Please run `cargo test`, `cargo clippy`, and `cargo fmt` prior to creating a PR.

### Code Coverage

Apollo MCP Server uses comprehensive code coverage reporting to ensure code quality and test effectiveness. 
The project uses [cargo-llvm-cov](https://crates.io/crates/cargo-llvm-cov) for generating code coverage reports and [Codecov](https://www.codecov.io/) for coverage analysis and reporting. Coverage is automatically generated and reported on every pull request through GitHub Actions.

#### Coverage Targets

The project maintains the following coverage targets, configured in `codecov.yml`:

- **Project Coverage**: Automatically maintained - should increase overall coverage on each PR
- **Patch Coverage**: 80% - requires 80% coverage on all new/modified code

These targets help ensure that:

- The overall codebase coverage doesn't decrease over time
- New code is well-tested before being merged
