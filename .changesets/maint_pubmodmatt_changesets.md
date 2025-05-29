### Automate changesets and changelog - @pubmodmatt PR #107

Contributors can now generate a changeset file automatically with:
```console
cargo xtask changeset create
```
This will generate a file in the `.changesets` directory, which can be added to the pull request.
