# Chapter 8 - Comments vs Documentation

> Clear code beats clear comments. However, when the why isn't obvious, comment it plainly.

## 8.1 Comments vs Documentation: Know the Difference

| Purpose      | Use `// comment`                          | Use `/// doc` or `//! crate doc`                        |
|--------------|-------------------------------------------|---------------------------------------------------------|
| Describe Why | Yes - explains tricky reasoning           | Not for documentation                                   |
| Describe API | Not useful                                | Yes - public interfaces, usage, details, errors, panics |
| Maintainable | Often becomes obsolete                    | Tied to code, appears in generated docs                 |
| Visibility   | Local development only                    | Exported to users and tools like `cargo doc`            |

## 8.2 When to use comments

Use `//` comments when something can't be expressed clearly in code:
* **Safety Guarantees**
* Workarounds or **Optimizations**
* Legacy or **platform-specific** behaviors
* Links to **Design Docs** or **ADRs**
* Assumptions or **gotchas** that aren't obvious

> Name your comments! For example: `// SAFETY: ...`

### Good comment:
```rust
// SAFETY: `ptr` is guaranteed to be non-null and aligned by caller
unsafe { std::ptr::copy_nonoverlapping(src, dst, len); }
```

## 8.3 When comments get in the way

Avoid comments that:
* Restate obvious things
* Can grow stale over time
* `TODO`s without actions
* Could be replaced by better naming or smaller functions

## 8.4 Don't Write Living Documentation

Comments are **not free**:
* They **rot** - nobody compiles comments.
* They **mislead** - readers usually assume they are true.
* They **go stale** - unless maintained with the code.
* They are **noisy**.

If something deserves to live beyond a PR, put it in:
* An **ADR** (Architectural Design Record).
* A Design Document.
* Document it **in code** using types, doc comments, examples.
* Add tests to cover and explain the change.

## 8.5 Replace Comments with Code

Instead of long commented blocks, break logic into named helper functions.

## 8.6 `TODO` should become issues

Don't leave `// TODO:` scattered around. Instead:
1. File Github Issue or Jira Ticket.
2. Reference the issue in the code:

```rust
// TODO(issue #42): Remove workaround after bugfix
```

## 8.7 When to use doc comments

Use `///` doc comments to document:
* All **public functions, structs, traits, enums**.
* Their purpose, their usage and their behaviors.
* Add context that relates to `Errors` and `Panics`.
* Plenty of examples.

### Good doc comment:

```rust
/// Loads [`User`] profile from disk
///
/// # Error
/// - Returns [`MyError`] if the file is missing.
/// - Returns [`MyError`] if the content is invalid Json.
fn load_user(path: &Path) -> Result<User, MyError> {...}
```

## 8.8 Documentation in Rust

### Difference between `///` and `//!`

| Style  | Used for                 | Scope                                    |
|--------|--------------------------|------------------------------------------|
| `///`  | Line doc comment         | Public items like struct, fn, enum       |
| `//!`  | Module level doc comment | Modules or entire crates                 |

### Doc specific lints

| Lint                    | Description                                                  |
|-------------------------|--------------------------------------------------------------|
| `missing_docs`          | Warns that a public item has missing documentation           |
| `broken_intra_doc_links`| Detects if an internal documentation link is broken          |
| `empty_docs`            | Disallow empty docs                                          |
| `missing_panics_doc`    | Warns that documentation should have a `# Panics` section    |
| `missing_errors_doc`    | Warns that documentation should have a `# Errors` section    |
| `missing_safety_doc`    | Warns that documentation should have a `# Safety` section    |

## 8.9 Checklist for Documentation coverage

- [ ] Crate-Level: `//!` doc explains what the crate does
- [ ] Modules: `//!` doc explains what this module is for
- [ ] Structs, Enums, Traits: `///` doc explains the role
- [ ] Functions: `///` doc covers what it does, parameters, return value, edge cases
- [ ] Traits: Explain the purpose, doc for each method
- [ ] Public Constants: Document what they configure

### Best Practices
* Use examples generously
* Prefer clarity over formality
* Use `cargo doc --open` to check your output
* Add `#![deny(missing_docs)]` for full doc coverage
