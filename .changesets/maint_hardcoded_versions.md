### Hardcoded version strings in tests - @DaleSeo PR #305

The GraphQL tests have hardcoded version strings that we need to update manually each time we release a new version. Since this isn't included in the release checklist, it's easy to miss it and only notice the test failures later.