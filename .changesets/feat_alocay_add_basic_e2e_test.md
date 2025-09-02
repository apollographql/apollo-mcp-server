### test: adding a basic manual e2e test for mcp server - @alocay PR #320

Adding some basic e2e tests using [mcp-server-tester](https://github.com/steviec/mcp-server-tester). Currently, the tool does not always exit (ctrl+c is sometimes needed) so this should be run manually.

### How to run tests?
Added a script `run_tests.sh` (may need to run `chmod +x` to run it) to run tests. Basic usage found via `./run_tests.sh -h`. The script does the following:

1. Builds test/config yaml paths and verifies the files exist.
2. Checks if release `apollo-mcp-server` binary exists. If not, it builds the binary via `cargo build --release`.
3. Reads in the template file (used by `mcp-server-tester`) and replaces all `<test-dir>` placeholders with the test directory value. Generates this test server config file and places it in a temp location.
4. Invokes the `mcp-server-tester` via `npx`.
5. On script exit the generated config is cleaned up.

### Example run: 
To run the tests for `local-operations` simply run `./run_tests.sh local-operations`
