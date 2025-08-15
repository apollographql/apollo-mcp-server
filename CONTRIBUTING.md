![ci workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/ci.yml)
![release binaries workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-bins.yml?label=release%20binaries)
![release container workflow status](https://img.shields.io/github/actions/workflow/status/apollographql/apollo-mcp-server/release-container.yml?label=release%20container)
![version](https://img.shields.io/github/v/release/apollographql/apollo-mcp-server)
![license](https://img.shields.io/github/license/apollographql/apollo-mcp-server)

## How to contribute to Apollo MCP Server

#### Bug Reporting

* **Ensure the bug was not already reported** by searching on GitHub under [Issues](https://github.com/apollographql/apollo-mcp-server/issues) as well as the [Apollo Community forums](https://community.apollographql.com/latest).
* If you're unable to find an open issue addressing the problem, [open a new one](https://github.com/apollographql/apollo-mcp-server/issues/new). Be sure to include a **title and clear description**, as much relevant information as possible, and a **code sample** or an **executable test case** demonstrating the expected behavior that is not occurring.
* If appropriate add the most relevant label but leave empty if unsure.

#### **Did you write a patch that fixes a bug?**

* Refer to the simple branching guide for the project.
* Open a new GitHub pull request with the patch.
* Ensure the PR description clearly describes the problem and solution. Include the relevant issue number if applicable.
* Before submitting, please read the [Contributing to Apollo MCP Server](#contributing-to-apollo-mcp-server) guide to know more about coding conventions, branching strategies, etc.

#### **Do you intend to add a new feature or change an existing one?**

* Suggest your change as a new [issues](https://github.com/apollographql/apollo-mcp-server/issues) using the `enhancement` label and start writing code.
* You can also suggest changes and features using the [Apollo Community forums](https://community.apollographql.com/latest).
* Before submitting, please read the [Contributing to Apollo MCP Server](#contributing-to-apollo-mcp-server) guide to know more about coding conventions, branching strategies, etc.

#### **Do you have questions about the code or about Apollo MCP Server itself?**

* Ask any question about Apollo MCP Server using either the [issues](https://github.com/apollographql/apollo-mcp-server/issues) page or the [Apollo Community forums](https://community.apollographql.com/latest). 
* If using the issues page, please use the `question` label.

Thanks!

Apollo MCP Server team

---

### Contributing to Apollo MCP Server

#### Branching strategy
The Apollo MCP Server project follows a more pseudo GitFlow branch strategy.

1. All feature work should branch off the `develop` branch.
2. Hotfix/patches are branched off main but changes must be cherry-picked back into `develop`.

#### Code conventions and testing
1. Run `cargo test`, `cargo clippy`, and `cargo fmt` prior to creating a PR.
2. Add unit tests for any changed or added functionality.