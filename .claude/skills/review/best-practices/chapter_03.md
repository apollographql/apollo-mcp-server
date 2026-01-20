# Chapter 3 - Performance Mindset

The **golden rule** of performance work:

> Don't guess, measure.

Rust code is often already pretty fast - don't "optimize" without evidence. Optimize only after finding bottlenecks.

### A good first steps
* Use `--release` flag on your builds.
* `$ cargo clippy -- -D clippy::perf` gives you important tips on best practices for performance.
* `cargo bench` is a cargo tool to create micro-benchmarks.
* `cargo flamegraph` a powerful profiler for Rust code.

## 3.1 Flamegraph

Flamegraph helps you visualize how much time CPU spent on each task.

```shell
cargo install flamegraph
cargo flamegraph
```

> Always run your profiles with `--release` enabled.

* The `y-axis` shows the **stack depth number**.
* The `width of each box` shows the **total time that function** is on the CPU.

### Remember
* Thick stacks: heavy CPU usage
* Thin stacks: low intensity (cheap)

## 3.2 Avoid Redundant Cloning

> Cloning is cheap... **until it isn't**

* If you really need to clone, leave it to the last moment.

### When to pass ownership?

* Only `.clone()` if you truly need a new owned copy.
* You have reference counted pointers (`Arc, Rc`).
* You have small structs that are too big to `Copy` but as costly as `std::collections`.

### When **NOT** to pass ownership?

* Prefer API designs that take reference (`fn process(values: &[T])`).
* If you only need read access to elements, prefer `.iter` or slices.
* You need to mutate data that is owned by another thread, use `&mut MyStruct`.

### Use `Cow` for `Maybe Owned` data

Sometimes you don't actually need owned data:

```rust
use std::borrow::Cow;

fn hello_greet(name: Cow<'_, str>) {
    println!("Hello {name}");
}

hello_greet(Cow::Borrowed("Julia"));
hello_greet(Cow::Owned("Naomi".to_string()));
```

## 3.3 Stack vs Heap: Be size-smart!

### Good Practices

* Keep small types (`impl Copy`, `usize`, `bool`, etc) **on the stack**.
* Avoid passing huge types (`> 512 bytes`) by value. Prefer pass by reference.
* Heap allocate recursive data structures.
* Return small types by value.

### Be Mindful

* Only use `#[inline]` when benchmark proves beneficial.
* Avoid massive stack allocations, box them.
* For large `const` arrays, consider using `smallvec`.

## 3.4 Iterators and Zero-Cost Abstractions

Rust iterators are lazy, but eventually compiled away into very efficient tight loops.

* Prefer `iterators` over manual `for` loops when working with collections.
* Calling `.iter()` only creates a **reference** to the original collection.

#### Avoid creating intermediate collections unless really needed:

* **BAD** - useless intermediate collection:
```rust
let doubled: Vec<_> = items.iter().map(|x| x * 2).collect();
process(doubled);
```
* **GOOD** - pass the iterator:
```rust
let doubled_iter = items.iter().map(|x| x * 2);
process(doubled_iter);
```
