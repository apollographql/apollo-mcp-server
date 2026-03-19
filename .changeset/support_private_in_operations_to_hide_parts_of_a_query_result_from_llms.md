---
default: minor
---

# Support @private in operations to hide parts of a query result from LLMs

In MCP Apps, you may have data that you want made available to your app but hidden from the LLM.

```
query ProductsQuery {
  topProducts {
    sku
    title
    meta @private {
      createdAt
      barcode
    }
  }
}
```

This will result in the non-private portions of the result being passed back in `structuredContent` as usual, and the entire result, including private portions, in `_meta.structuredContent`.
