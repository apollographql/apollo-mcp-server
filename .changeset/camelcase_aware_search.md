---
default: minor
---

# Add camelCase-aware search tokenization to schema index

Split camelCase and PascalCase identifiers into individual words before indexing and querying, so searching for "post" now matches types like `PostAnalytics`, `CreatePostInput`, and `createPost`. Uses `heck::ToSnakeCase` to split identifiers at word boundaries, matching Rover's existing behavior.
