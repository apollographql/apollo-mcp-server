# Chapter 2 - Clippy and Linting Discipline

## 2.1 Why care about linting?

Rust compiler is a powerful tool that catches many mistakes. However, some more in-depth analysis require extra tools, that is where `cargo clippy` comes into play. Clippy checks for:
* Performance pitfalls.
* Style issues.
* Redundant code.
* Potential bugs.
* Non-idiomatic Rust.

## 2.2 Always run `cargo clippy`

Add the following to your daily workflow:

```shell
$ cargo clippy --all-targets --all-features --locked -- -D warnings
```

* `--all-targets`: checks library, tests, benches and examples.
* `--all-features`: checks code for all features enabled.
* `--locked`: Requires `Cargo.lock` to be up-to-date.
* `-D warnings`: treats warnings as errors

## 2.3 Important Clippy Lints to Respect

| Lint Name | Why |
| --------- | ----|
| `redundant_clone` | Detects unnecessary `clones`, has performance impact |
| `needless_borrow` group | Removes redundant `&` borrowing |
| `map_unwrap_or` / `map_or` | Simplifies nested `Option/Result` handling |
| `manual_ok_or` | Suggest using `.ok_or_else` instead of `match` |
| `large_enum_variant` | Warns if an enum has very large variant. Suggests `Boxing` it |
| `unnecessary_wraps` | If your function always returns `Some` or `Ok`, you don't need `Option`/`Result` |
| `clone_on_copy` | Catches accidental `.clone()` on `Copy` types like `u32` and `bool` |
| `needless_collect` | Prevents collecting and allocating an iterator when allocation is not needed |

## 2.4 Fix warnings, don't silence them!

**NEVER** just `#[allow(clippy::lint_something)]` unless:

* You **truly understand** why the warning happens and you have a reason why it is better that way.
* You **document** why it is being ignored.
* Don't use `allow`, but `expect`, it will give a warning in case the lint is not true anymore.

### Example:

```rust
// Faster matching is preferred over size efficiency
#[expect(clippy::large_enum_variant)]
enum Message {
    Code(u8),
    Content([u8; 1024]),
}
```

### Handling false positives

Sometimes Clippy complains even when your code is correct:
1. Try to refactor the code, so it improves the warning.
2. **Locally** override the lint with `#[expect(clippy::lint_name)]` and a comment with the reason.
3. Avoid global overrides.

## 2.5 Configure workspace/package lints

In your `Cargo.toml` file it is possible to determine which lints and their priorities:

```toml
[lints.rust]
future-incompatible = "warn"
nonstandard_style = "deny"

[lints.clippy]
all = { level = "deny", priority = 10 }
redundant_clone = { level = "deny", priority = 9 }
manual_while_let_some = { level = "deny", priority = 4 }
pedantic = { level = "warn", priority = 3 }
```
