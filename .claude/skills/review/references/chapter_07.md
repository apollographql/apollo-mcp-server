# Chapter 7 - Type State Pattern

Models state at compile time, preventing bugs by making illegal states unrepresentable.

## 7.1 What is Type State Pattern?

**Type State Pattern** is a design pattern where you encode different **states** of the system as **types**, not as runtime flags or enums. This allows the compiler to enforce state transitions and prevent illegal actions at compile time.

> Invalid states become compile errors instead of runtime bugs.

## 7.2 Why use it?

* Avoids runtime checks for state validity.
* Models state transitions as type transitions.
* Prevents data misuse.
* Improves API safety and correctness.
* The phantom data field is removed after compilation so no extra memory is allocated.

## 7.3 Simple Example: File State

```rust
struct FileNotOpened;
struct FileOpened;

struct File<State> {
    path: PathBuf,
    handle: Option<std::fs::File>,
    _state: std::marker::PhantomData<State>
}

impl File<FileNotOpened> {
    fn open(path: &Path) -> io::Result<File<FileOpened>> {
        let file = std::fs::File::open(path)?;
        Ok(File {
            path: path.to_path_buf(),
            handle: Some(file),
            _state: std::marker::PhantomData::<FileOpened>
        })
    }
}

impl File<FileOpened> {
    fn read(&mut self) -> io::Result<String> {
        // read can only be called by state File<FileOpened>
        ...
    }
}
```

## 7.4 Real-World Examples

### Builder Pattern with Compile-Time Guarantees

> Forces the user to **set required fields** before calling `.build()`.

A type-state pattern can have more than one associated states. This guarantees that all necessary fields are present.

### Network Protocol State Machine

Illegal transitions like sending a message before connecting **simply don't compile**.

## 7.5 Pros and Cons

### Use Type-State Pattern When:
* You want **compile-time state safety**.
* You need to enforce **API constraints**.
* You are writing a library/crate that is heavy dependent on variants.
* You want to replace runtime booleans or enums with **type-safe code paths**.

### Avoid it when:
* Writing trivial states like enums.
* Don't need type-safety.
* When it leads to overcomplicated generics.
* When runtime flexibility is required.

### Downsides and Cautions
* Can lead to more **verbose solutions**.
* Can lead to **complex type signatures**.
* May require **unsafe** to return **variant outputs** based on different states.
* May require duplication.
* PhantomData is not intuitive for beginners.

> Use this pattern when it **saves bugs, increases safety or simplifies logic**, not just for cleverness.
