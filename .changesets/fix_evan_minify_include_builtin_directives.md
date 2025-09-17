### Minify: Add support for deprecated directive - @esilverm PR #367

Includes any existing `@deprecated` directives in the schema in the minified output of builtin tools. Now operations generated via these tools should take into account deprecated fields when being generated.