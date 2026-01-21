# Chapter 4 - Errors Handling

Rust enforces a strict error handling approach, but *how* you handle them defines whether your code feels ergonomic, consistent and safe.

> Even if you decide to crash your application with `unwrap` or `expect`, Rust forces you to declare that intentionally.

## 4.1 Prefer `Result`, avoid panic

If your function can fail, prefer to return a `Result`:
```rust
fn divide(x: f64, y: f64) -> Result<f64, DivisionError> {
    if y == 0.0 {
        Err(DivisionError::DividedByZero)
    } else {
        Ok(x / y)
    }
}
```

* Use `panic!` only in unrecoverable conditions - typically tests, assertions, bugs.
* Consider `todo!`, `unreachable!`, `unimplemented!` for appropriate conditions.

## 4.2 Avoid `unwrap`/`expect` in Production

Although `expect` is preferred to `unwrap`, they should be avoided in production code. Use them in:
- Tests, assertions or test helper functions.
- When failure is impossible.
- When the smarter options can't handle the specific case.

### Alternative ways of handling `unwrap`/`expect`:

* Use `let Ok(..) = else { return ... }` pattern for early returns.
* Use `if let Ok(..) else { ... }` pattern for error recovery.
* Use `unwrap_or`, `unwrap_or_else` or `unwrap_or_default`.

## 4.3 `thiserror` for Crate level errors

Use `thiserror` to create error types that implement `From` trait and easy error messages:

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyError {
    #[error("Network Timeout")]
    Timeout,
    #[error("Invalid data: {0}")]
    InvalidData(String),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}
```

### Error Hierarchies and Wrapping

For layered systems use nested `enum/struct` errors with `#[from]`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("Database handler error: {0}")]
    Db(#[from] DbError),
    #[error("External services error: {0}")]
    ExternalServices(#[from] ExternalHttpError)
}
```

## 4.4 Reserve `anyhow` for Binaries

`anyhow` is recommended only for **binaries**, not libraries:

```rust
use anyhow::{Context, Result, anyhow};

fn main() -> Result<Config> {
    let content = std::fs::read_to_string("config.json")
        .context("Failed to read config file")?;
    Config::from_str(&content)
        .map_err(|err| anyhow!("Config parsing error: {err}"))
}
```

### `Anyhow` Gotchas

* Keeping the `context` and `anyhow` strings up-to-date is harder than `thiserror`.
* `anyhow::Result` erases context that a caller might need, so avoid in libraries.

## 4.5 Use `?` to Bubble Errors

Prefer using `?` over verbose alternatives like `match` chains:
```rust
fn handle_request(req: &Request) -> Result<ValidatedRequest, RequestValidationError> {
    validate_headers(req)?;
    validate_body_format(req)?;
    let body = Body::try_from(req)?;
    Ok(ValidatedRequest::try_from((req, body))?)
}
```

## 4.6 Unit Test should exercise errors

Test error messages with `format!` or `to_string()`:

```rust
#[test]
fn error_does_not_implement_partial_eq() {
    let err = divide(10., 0.0).unwrap_err();
    assert_eq!(err.to_string(), "division by zero");
}
```

## 4.7 Important Topics

### Custom Error Structs

Sometimes you don't need an enum:

```rust
#[derive(Debug, thiserror::Error, PartialEq)]
#[error("Request failed with code `{code}`: {message}")]
struct HttpError {
    code: u16,
    message: String
}
```

### Async Errors

Make sure errors implement `Send + Sync + 'static` where needed:

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Ok(())
}
```

> Avoid `Box<dyn std::error::Error>` in libraries unless really needed.
