---
default: patch
---

# Express nullability and default values in generated input schemas

The shape of generated tool input schemas changes: every nullable variable, input object field, and list item is now wrapped as `{"anyOf": [<type>, {"type": "null"}]}`. Output schemas already spell out nullability the same way with `oneOf`; input schemas use `anyOf` because the strict-mode schema subsets of OpenAI and Anthropic accept `anyOf` and reject `oneOf`. Any call that was valid before stays valid, since omitting a property is still allowed and `required` only shrinks. Hosts that inspect `properties.<name>.type` directly will see `anyOf` instead of a bare type, and tests that snapshot tool schemas will need updating.

Previously, a variable was marked nullable only by leaving it out of `required`. Clients that rewrite schemas for strict function calling move every property into `required`, which removed the only way to leave a value unset, so models sent placeholder values such as `""`, `[]`, or an arbitrary boolean for filters they meant to skip. With an explicit `null` alternative, `null` stays valid after such a rewrite.

GraphQL default values on variables and input fields are emitted as JSON Schema `default`. A non-null variable with a default value, such as `$limit: Int! = 10`, is no longer listed as `required`, matching GraphQL semantics and how input fields were already handled. Models may now omit it and let the default apply where they previously had to supply a value. Descriptions on list-typed variables now sit on the property itself rather than on its items, so they stay visible above the new wrapper.
