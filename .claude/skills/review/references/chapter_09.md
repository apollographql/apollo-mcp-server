# Chapter 9 - Understanding Pointers

Many higher level languages hide memory management. Rust makes memory management explicit and safe.

## 9.1 Thread Safety

Rust tracks pointers using `Send` and `Sync` traits:
- `Send` means data can move across threads.
- `Sync` means data can be referenced from multiple threads.

> A pointer is thread-safe only if the data behind it is.

| Pointer Type   | Short Description                               | Send + Sync?                   | Main Use                           |
|----------------|-------------------------------------------------|--------------------------------|------------------------------------|
| `&T`           | Shared reference                                | Yes                            | Shared access                      |
| `&mut T`       | Exclusive mutable reference                     | No, not Send                   | Exclusive mutation                 |
| `Box<T>`       | Heap-allocated owning pointer                   | Yes, if T: Send + Sync         | Heap allocation                    |
| `Rc<T>`        | Single-threaded ref counted pointer             | No, neither                    | Multiple owners (single-thread)    |
| `Arc<T>`       | Atomic ref counter pointer                      | Yes                            | Multiple owners (multi-thread)     |
| `Cell<T>`      | Interior mutability for copy types              | No, not Sync                   | Shared mutable, non-threaded       |
| `RefCell<T>`   | Interior mutability (dynamic borrow checker)    | No, not Sync                   | Shared mutable, non-threaded       |
| `Mutex<T>`     | Thread-safe interior mutability                 | Yes                            | Shared mutable, threaded           |
| `RwLock<T>`    | Thread-safe shared read OR exclusive write      | Yes                            | Shared mutable, threaded           |
| `OnceCell<T>`  | Single-thread one-time initialization           | No, not Sync                   | Simple lazy value initialization   |
| `OnceLock<T>`  | Thread-safe version of `OnceCell<T>`            | Yes                            | Multi-thread single init           |
| `LazyLock<T>`  | Thread-safe lazy initialization                 | Yes                            | Multi-thread complex init          |
| `*const T`     | Raw Pointers                                    | No, user must ensure safety    | Raw memory / FFI                   |

## 9.2 When to use pointers:

### `&T` - Shared Borrow:

**Safe, with no mutation** and allows **multiple readers**.

### `&mut T` - Exclusive Borrow:

**Safe, but only allows one mutable borrow at a time**.

### `Box<T>` - Heap Allocated

Single-owner heap-allocated data, great for recursive types and large structs.

### `Rc<T>` - Reference Counter (single-thread)

You need multiple references to data in a single thread.

### `Arc<T>` - Atomic Reference Counter (multi-thread)

You need multiple references to data in multiple threads.

### `RefCell<T>` - Runtime checked interior mutability

Used when you need shared access and the ability to mutate data. Borrow rules are enforced at runtime. **It may panic!**.

### `Cell<T>` - Copy-only interior mutability

Fast and safe version of `RefCell`, limited to types that implement `Copy`.

### `Mutex<T>` - Thread-safe mutability

Exclusive access pointer for read/write. Usually wrapped in `Arc`.

### `RwLock<T>` - Thread-safe mutability

Allows multiple threads to read OR a single thread to write. Usually wrapped in `Arc`.

### `*const T/*mut T` - Raw pointers

Inherently **unsafe** and necessary for FFI.

### `OnceLock<T>` - thread-safe single initialization

Useful when you need a `static` value.

### `LazyLock<T>` - thread-safe lazy initialization

For static values that are complex to initialize.

## References
- Mara Bos - Rust Atomics and Locks
