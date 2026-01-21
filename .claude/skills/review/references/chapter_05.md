# Chapter 5 - Automated Testing

> Tests are not just for correctness. They are the first place people look to understand how your code works.

## 5.1 Tests as Living Documentation

### Use descriptive names

> In the unit test name we should see:
> * `unit_of_work`: which *function* we are calling.
> * `expected_behavior`: the set of **assertions** that we need to verify.
> * `state_that_the_test_will_check`: the general **arrangement** of the specific test case.

#### Don't use a generic name for a test
```rust
#[test]
fn test_add_happy_path() {
    assert_eq!(add(2, 2), 4);
}
```

#### Use a name which reads like a sentence
```rust
#[test]
fn process_should_return_blob_when_larger_than_b() {
    let a = setup_a_to_be_xyz();
    let b = Some(2);
    let expected = MyExpectedStruct { ... };

    let result = process(a, b).unwrap();

    assert_eq!(result, expected);
}
```

### Use modules for organization

```rust
#[cfg(test)]
mod test {
  mod process {
    #[test]
    fn returns_error_xyz_when_b_is_negative() { ... }

    #[test]
    fn returns_invalid_input_error_when_a_and_b_not_present() { ... }
  }
}
```

### Only test one behavior per function

#### Don't test multiple things in the same test
```rust
fn test_thing_parser(...) {
  assert!(Thing::parse("abcd").is_ok());
  assert!(Thing::parse("ABCD").is_err());
}
```

#### Test one thing per test
```rust
#[test]
fn lowercase_letters_are_valid() {
  assert!(Thing::parse("abcd").is_ok());
}

#[test]
fn capital_letters_are_invalid() {
  assert!(Thing::parse("ABCD").is_err());
}
```

### Use very few, ideally one, assertion per test

When there are multiple assertions per test, it's harder to understand the intended behavior.

## 5.2 Add Test Examples to your Docs

Rustdoc can turn examples into executable tests using `///`:

* These tests run with `cargo test`.
* They serve both as documentation and correctness checks.
* No extra testing boilerplate.

## 5.3 Unit Test vs Integration Tests vs Doc tests

### Unit Test

Tests that go in the **same module** as the **tested unit**. They can be more focused on **implementation and edge-cases checks**.

* They should be as simple as possible. KISS.
* They should test for errors and edge cases.
* Try to keep external states/side effects to minimum.

### Integration Tests

Tests that go under the `tests/` directory. They can **only test** functions on your **public API**.

* Test for happy paths and common use cases.
* Allow external states and side effects.

### Doc Testing

Doc tests should have happy paths, general public API usage.

### Attributes:

* `ignore`: tells rust to ignore the code.
* `should_panic`: tells the rust compiler that this example block will panic.
* `no_run`: compiles but doesn't execute the code.
* `compile_fail`: Test rustdoc that this block should cause a compilation fail.

## 5.4 How to `assert!`

* `assert!` for asserting boolean values.
* `assert_eq!` for checking equality between two different values.

### `assert!` reminders
* Rust asserts support formatted strings.
* Use `matches!` combined with `assert!` for pattern matching.
* Use `#[should_panic]` wisely.

## 5.5 Snapshot Testing with `cargo insta`

> When correctness is visual or structural, snapshots tell the story better than asserts.

### What is snapshot testing?

Snapshot testing compares your output against a saved "golden" version. Perfect for:
* Generated code.
* Serializing complex data.
* Rendered HTML.
* CLI output.

#### What not to test with snapshot
* Very stable, numeric-only data (prefer `assert_eq!`).
* Critical path logic (prefer precise unit tests).
* Flaky tests, randomly generated output.

## 5.6 Snapshot Best Practices

* Use named snapshots for meaningful file names.
* Keep snapshots small and clear.
* Avoid snapshotting huge objects.
* Avoid snapshotting simple types.
* Use redactions for unstable fields.
* Commit your snapshots into git.
* Review changes carefully before accepting.
