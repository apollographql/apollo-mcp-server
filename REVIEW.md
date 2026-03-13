# Code Review Guidelines

## Best Practices Handbook

Please use the `rust-best-practices` skill and review all of the best practices contained in that skill prior to beginning your review.

## General Guidelines

- Review the current PR with a focus on **high-signal, actionable feedback**. Avoid nitpicks and style preferences unless they impact correctness or maintainability.
- All automated checks (tests, linting, clippy, formatting) are handled by GitHub Actions. Focus exclusively on reviewing the code.
- For duplicate findings in multiple locations within a file (E.g. multiple tests have the same issue), leave a single online comment referencing the various places in the file it needs to be fixed instead of leaving several inline comments about the same issue.

## Guidelines for High-Signal Reviews

- **Be specific**: Always reference exact file and line numbers
- **Explain why**: Don't just say "this is wrong" - explain the consequence
- **Reference best practices**: Link to the relevant chapter when suggesting changes
- **Suggest fixes**: Provide concrete code examples when helpful
- **Skip the obvious**: Don't mention things clippy would catch (run it instead)
- **Focus on this PR**: Don't request unrelated refactoring
- **Acknowledge good work**: Note well-designed solutions briefly
- **Don't Overwhelm**: Stick to a maximum of the top 5 most important findings.
