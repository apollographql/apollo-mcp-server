# Repository Guidelines

## Project Structure & Module Organization
Workspace root stores Cargo/Nix config plus sample `apollo.config.json`. Core crates live in `crates/`: `apollo-mcp-server` (binary + transports), `apollo-mcp-registry` (registry/auth helpers), and `apollo-schema-index` (schema cache utilities). Docs live in `docs/source`, sample graphs + configs in `graphql/TheSpaceDevs` and `graphql/weather`, and `e2e/mcp-server-tester` houses black-box scenarios. Developer tooling sits in `xtask/` (changelog automation) and `scripts/` (CI helpers); stage release notes in `CHANGELOG_SECTION.md` before promoting them to `CHANGELOG.md`.

## Build, Test, and Development Commands
- `cargo build --workspace` compiles every crate; add `--release` before running e2e harnesses.
- `cargo run -p apollo-mcp-server -- --config graphql/TheSpaceDevs/apollo.config.json` boots the server against the sample graph.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features -D warnings` mirror the formatting + lint gates enforced in CI.
- `cargo llvm-cov --workspace --html` (via `cargo install cargo-llvm-cov`) generates the coverage artifact that Codecov ingests.
- `cargo xtask changeset create` writes a changeset entry; `cargo xtask changeset changelog` rolls staged entries into `CHANGELOG.md`.

## Coding Style & Naming Conventions
Rust 2024 + rustfmt 4-space indentation is canonical—never hand-align. Use kebab-case crate names, snake_case modules, UpperCamelCase exported types, and keep feature flags consistent with `Cargo.toml`. Clippy denies `unwrap`, `expect`, unchecked indexing, and panics (see `clippy.toml`), so lean on `?` and typed errors. Instrument async flows with `tracing` macros and document public APIs with `///`. Snapshot assets emitted by `insta` should remain readable YAML/JSON inside the corresponding `snapshots/` directories.

## Testing Guidelines
Run `cargo test --workspace` before committing; append `-- --ignored` for slow or networked suites. Mirror module layouts (`src/foo.rs` → `src/foo/tests.rs`) and use `rstest` for table-driven cases. Update snapshots with `cargo insta review` so reviewers only see intentional diffs. End-to-end validation lives in `e2e/mcp-server-tester`; after building with `cargo build --release`, run `./e2e/mcp-server-tester/run_tests.sh local-operations` (configs + secrets provided via env). Codecov enforces ≥80% patch coverage per `codecov.yml`, so add targeted tests whenever behavior shifts.

## Commit & Pull Request Guidelines
Branch from `develop` and keep commits small, imperative (`Improve operation update concurrency`). Every functional change needs a changeset (`cargo xtask changeset create`), relevant docs, and config migrations. Pull requests should describe intent, link GitHub issues or discussions, attach screenshots/logs for UX or protocol changes, and confirm CI (`cargo fmt`, `cargo clippy`, `cargo test`, `cargo llvm-cov`) passed. Avoid unrelated refactors; reviewers expect risk callouts and manual test notes in the PR body.

## Security & Configuration Tips
Store Apollo keys, JWKS URLs, and other secrets in your shell or a local `.envrc`; never bake them into `graphql/*` fixtures or committed configs. Sample configs in `graphql/TheSpaceDevs` and `e2e/mcp-server-tester/server-config.template.json` illustrate the minimum fields—copy then tweak rather than editing in-place. When touching auth, telemetry, or transport code, document new ports, scopes, or TLS requirements in your PR and ping the security contact listed in `SECURITY.md`.
