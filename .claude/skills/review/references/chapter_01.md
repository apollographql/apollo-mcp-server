# Chapter 1 - Coding Styles and Idioms

## 1.1 Borrowing Over Cloning

Rust's ownership system encourages **borrow** (`&T`) instead of **cloning** (`T.clone()`).
> Performance recommendation

### When to `Clone`:

* You need to change the object AND preserve the original object (immutable snapshots).
* When you have `Arc` or `Rc` pointers.
* When data is shared across threads, usually `Arc`.
* Avoid massive refactoring of non performance critical code.
* When caching results (dummy example below):
```rust
fn get_config(&self) -> Config {
  self.cached_config.clone()
}
```
* When the underlying API expects Owned Data.

### `Clone` traps to avoid:

* Auto-cloning inside loops `.map(|x| x.clone)`, prefer to call `.cloned()` or `.copied()` at the end of the iterator.
* Cloning large data structures like `Vec<T>` or `HashMap<K, V>`.
* Clone because of bad API design instead of adjusting lifetimes.
* Prefer `&[T]` instead of `Vec<T>` or `&Vec<T>`.
* Prefer `&str` or `&String` instead of `String`.
* Prefer `&T` instead of `T`.
* Clone a reference argument, if you need ownership, make it explicit in the arguments for the caller. Example:
```rust
fn take_a_borrow(thing: &Thing) {
  let thing_cloned = thing.clone(); // the caller should have passed ownership instead
}
```

### Prefer borrowing:
```rust
fn process(name: &str) {
  println!("Hello {name}");
}

let user = String::from("foo");
process(&user);
```

### Avoid redundant cloning:
```rust
fn process_string(name: String) {
  println!("Hello {name}");
}

let user = String::from("foo");
process(user.clone()); // Unnecessary clone
```

## 1.2 When to pass by value? (Copy trait)

Not all types should be passed by reference (`&T`). If a type is **small** and it is **cheap to copy**, it is often better to **pass it by value**. Rust makes it explicit via the `Copy` trait.

### When to pass by value, `Copy`:
* The type **implements** `Copy` (`u32`, `bool`, `f32`, small structs).
* The cost of moving the value is negligible.

```rust
fn increment(x: u32) -> u32 {
    x + 1
}

let num = 1;
let new_num = increment(num); // `num` still usable after this point
```

### Which structs should be `Copy`?
* When to consider declaring `Copy` on your own types:
* All fields are `Copy` themselves.
* The struct is `small`, up to 2 (maybe 3) words of memory or 24 bytes (each word is 64 bits/8bytes).
* The struct **represents a "plain data object"**, without resourcing to ownership (no heap allocations. Example: `Vec` and `Strings`).

**Rust Arrays are stack allocated.** Which means they can be copied if their underlying type is `Copy`, but this will be allocated in the program stack which can easily become a stack overflow.

### Good struct to derive `Copy`:
```rust
#[derive(Debug, Copy, Clone)]
struct Point {
  x: f32,
  y: f32,
  z: f32
}
```

### Bad struct to derive `Copy`:
```rust
#[derive(Debug, Clone)]
struct BadIdea {
  age: i32,
  name: String, // String is not `Copy`
}
```

### Which Enums should be `Copy`?
* If your enum acts like tags and atoms.
* The enum payloads are all `Copy`.
* **Enums size are based on their largest element.**

### Good Enum to derive
```rust
#[derive(Debug, Copy, Clone)]
enum Direction {
  North,
  South,
  East,
  West,
}
```

## 1.3 Handling `Option<T>` and `Result<T, E>`
Rust 1.65 introduced a better way to safely unpack Option and Result types with the `let Some(x) = … else { … }` or `let Ok(x) = … else { … }` when you have a default `return` value, `continue` or `break` default else case. It allows early returns when the missing case is **expected and normal**, not exceptional.

### Cases to use each pattern matching for Option and Return
* Use `match` when you want to pattern match against the inner types `T` and `E`
* Use `match` when your type is transformed into something more complex Like `Result<T, E>` becoming `Result<Option<U>, E>`.
* Use `let PATTERN = EXPRESSION else {  DIVERGING_CODE; }` when the divergent code doesn't need to know about the failed pattern matches or doesn't need extra computation.
* Use `let PATTERN = EXPRESSION else {  DIVERGING_CODE; }` when you want to break or continue a pattern match.
* Use `if let PATTERN = EXPRESSION else {  DIVERGING_CODE; }` when `DIVERGING_CODE` needs extra computation.

**If you don't care about the value of the `Err` case, please use `?` to propagate the `Err` to the caller.**

### Bad Option/Return pattern matching:

* Conversion between Result and Option (prefer `.ok()`,`.ok_or()`, and `ok_or_else()`)
* `if let PATTERN = EXPRESSION else {  DIVERGING_CODE; }` when divergent code is a default or pre-computed value
* Using `unwrap` or `expect` outside tests

## 1.4 Prevent Early Allocation

When dealing with functions like `or`, `map_or`, `unwrap_or`, `ok_or`, consider that they have special cases for when memory allocation is required. They can be replaced with their `_else` counter-part to defer allocation.

### Mapping Err

When dealing with Result::Err, sometimes is necessary to log and transform the Err into a more abstract or more detailed error, this can be done with `inspect_err` and `map_err`:

```rust
let x = Err(ParseError::InvalidContent(...));

x
.inspect_err(|err| tracing::error!("function_name: {err}"))
.map_err(|err| GeneralError::from(("function_name", err)))?;
```

## 1.5 Iterator, `.iter` vs `for`

### When to prefer `for` loops
* When you need **early exits** (`break`, `continue`, `return`).
* **Simple iteration** with side-effects (e.g., logging, IO)
* When readability matters more than simplicity or chaining.

### When to prefer `iterators` loops (`.iter()` and `.into_iter()`)
* When you are `transforming collections` or `Option/Results`.
* You can **compose multiple steps** elegantly.
* No need for early exits.
* You need support for indexed values with `.enumerate`.
* You need to use collections functions like `.windows` or `chunks`.
* You need to combine data from multiple sources and don't want to allocate multiple collections.

> **REMEMBER: Iterators are Lazy** - `.iter`, `.map`, `.filter` don't do anything until you call its consumer, e.g. `.collect`, `.sum`, `.for_each`.

### Anti-patterns to AVOID

* Don't chain without formatting. Prefer each chained function on its own line.
* Don't chain if it makes the code unreadable.
* Avoid needlessly collect/allocate of a collection when not needed.
* Prefer `iter` over `into_iter` unless you don't need the ownership of the collection.
* Prefer `iter` over `into_iter` for collections that inner type implements `Copy`.
* For summing numbers prefer `.sum` over `.fold`.

## 1.6 Comments: Context, not Clutter

> "Context are for why, not what or how"

Well-written Rust code, with expressive types and good naming, often speaks for itself.

### Good comments

* Safety concerns
* Performance quirks
* Links to ADRs or design docs

### Bad comments

* Wall-of-text explanations
* Comments that could be better represented as functions or are plain obvious

### TODOs are not comments - track them properly

Avoid leaving lingering `// TODO: Lorem Ipsum` comments in the code. Instead:
* Turn them into Jira or Github Issues.
* Reference the issue in the code.

## 1.7 Use Declarations - "imports"

The standard way of sorting imports:

- `std` (`core`, `alloc` would also fit here).
- External crates (what is in your Cargo.toml `[dependencies]`).
- Workspace crates (workspace member crates).
- This module `super::`.
- This module `crate::`.
