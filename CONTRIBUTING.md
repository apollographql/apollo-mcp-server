![ci workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/ci.yml)
![release binaries workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-bins.yml?label=release%20binaries)
![release container workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-container.yml?label=release%20container)
![version](https://img.shields.io/github/v/release/apollographql/apollo-mcp-server)
![license](https://img.shields.io/github/license/apollographql/apollo-mcp-server)

## How to contribute to Apollo MCP Server

### Bug Reporting

> [!WARNING]  
> **Do not open up a GitHub issue if the bug is a security vulnerability**, and instead refer to our [security policy](https://github.com/apollographql/.github/blob/main/SECURITY.md).
* **Ensure the bug was not already reported** by searching on GitHub under [Issues](https://github.com/apollographql/apollo-mcp-server/issues) as well as the [Apollo Community forums](https://community.apollographql.com/latest).
* If you're unable to find an open issue addressing the problem, [open a new one](https://github.com/apollographql/apollo-mcp-server/issues/new). Be sure to include a **title and clear description**, as much relevant information as possible, and a **code sample** or an **executable test case** demonstrating the expected behavior that is not occurring.
* If appropriate, add the most relevant label but leave empty if unsure.

### Did you write a patch that fixes a bug?

* Refer to the simple [branching guide](#branching-strategy) for the project.
* Open a new GitHub pull request with the patch.
* Ensure the PR description clearly describes the problem and solution. Include the relevant issue number if applicable.
* Before submitting, please read the [branching strategy](#branching-strategy) and [code review guidelines](#code-review-guidelines) to learn more about our coding conventions, branching strategies, code reviews guidelines, etc.

### Do you intend to add a new feature or change an existing one?

* Suggest your change as a new [issue](https://github.com/apollographql/apollo-mcp-server/issues) using the `enhancement` label.
* You can also suggest changes and features using the [Apollo Community forums](https://community.apollographql.com/latest).
* Once the feature is coded and complete, open a GitHub pull request providing clear description of the feature/change and include any relevant links to discussions.
* Before submitting, please read the [branching strategy](#branching-strategy) and [code review guidelines](#code-review-guidelines) to learn more about our coding conventions, branching strategies, code reviews guidelines, etc.

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
The Apollo MCP Server project follows a pseudo [GitFlow](https://docs.aws.amazon.com/prescriptive-guidance/latest/choosing-git-branch-approach/gitflow-branching-strategy.html) branch strategy.

1. All feature/bug fix/patch work should branch off the `develop` branch.

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
