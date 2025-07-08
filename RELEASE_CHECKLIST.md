# Release Checklist

This document outlines the steps required to prepare and execute a new release of the Apollo MCP Server.

## Release Process

- [ ] Create a new branch "#.#.#" where "#.#.#" is this release's version
- [ ] Update the change log by running `cargo xtask changeset changelog #.#.#` where `#.#.#` is the release version, and verify the results in [CHANGELOG.md](./CHANGELOG.md)
- [ ] Ensure that any new command line arguments have been added to [the command reference doc page](./docs/source/command-reference.mdx)
- [ ] Ensure any new command line arguments have an equivalent in `rover dev`, or there is an open task to add them if appropriate
- [ ] Update the version number in [Cargo.toml](./Cargo.toml)
- [ ] Update the version number in [the \*nix install script](./scripts/nix/install.sh)
- [ ] Update the version number in [the Windows install script](./scripts/windows/install.ps1)
- [ ] Update the version number in [the command reference](./docs/source/command-reference.mdx)
- [ ] Create a PR with these changes
- [ ] Copy and paste the section of the change log for this release into the PR comment
- [ ] Get the PR approved and merged
- [ ] Check out `main` and `git pull` to pick up your merged changes
- [ ] Sync your tags with the repo: `git tag -d $(git tag) && git fetch --tags`
- [ ] Create a new tag for the release: `git tag -a v#.#.# -m "#.#.#"`
- [ ] Push the release tag - this will kick off a release build in GitHub: `git push origin tag v#.#.#`
- [ ] Wait for CI to pass and the release to appear on the [Releases page](https://github.com/apollographql/apollo-mcp-server/releases)
- [ ] Edit the release and paste the changelog entry into the description
- [ ] Check the box to mark the release as the latest, and click the button to Update Release
- [ ] Test the install with `curl -sSL https://mcp.apollo.dev/download/nix/latest | sh`
- [ ] Run `./apollo-mcp-server` and make sure the version number is the new release
