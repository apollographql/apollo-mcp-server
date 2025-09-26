### Fix version on mcp server tester - @alocay PR #374

Add a specific version when calling the mcp-server-tester for e2e tests. The current latest (1.4.1) as an issue so to avoid problems now and in the future updating the test script to invoke the testing tool via specific version.
