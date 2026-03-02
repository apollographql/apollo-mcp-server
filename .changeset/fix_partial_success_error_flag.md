---
default: patch
---

# Fix isError for partial-success GraphQL responses

When a GraphQL resolver fails at runtime, servers often return a response where `errors` is populated alongside a non-null `data` object (e.g. `{"errors": [...], "data": {"createUsers": null}}`). The `execute` tool was incorrectly treating these responses as successes because its `isError` logic required `data` to be absent or null. The check now only requires the presence of a non-null `errors` array, which is the correct signal per the [GraphQL spec §7.1.6](https://spec.graphql.org/draft/#sec-Errors).
