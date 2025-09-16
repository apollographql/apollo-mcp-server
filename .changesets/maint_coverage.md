### Implement Test Coverage Measurement and Reporting - @DaleSeo PR #335

This PR adds the bare minimum for code coverage reporting using [cargo-llvm-cov](https://crates.io/crates/cargo-llvm-cov) and integrates with [Codecov](https://www.codecov.io/). It adds a new `coverage` job to the CI workflow that generates and uploads coverage reporting in parallel with existing tests. The setup mirrors that of Router, except it uses `nextest` instead of the built-in test runner and CircleCI instead of GitHub Actions.
