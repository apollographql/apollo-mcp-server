# Chapter 6 - Generics, Dynamic Dispatch and Static Dispatch

> Static where you can, dynamic where you must

Rust allows you to handle polymorphic code in two ways:
* **Generics / Static Dispatch**: compile-time, monomorphized per use.
* **Trait Objects / Dynamic Dispatch**: runtime vtable, single implementation.

## 6.1 Generics

We use generics to create definitions for items like function signatures or structs, which we can then use with many different concrete data types.

### Generics Performance

Using generic types won't make your program run any slower than it would with concrete types. Rust accomplishes this by performing monomorphization at compile time.

## 6.2 Static Dispatch: `impl Trait` or `<T: Trait>`

### Best when:
* You want **zero runtime cost**.
* You need **tight loops or performance**.
* Your types are **known at compile time**.
* You are working with **single-use implementations**.

### Example:
```rust
fn specialized_sum<U: Sum + RandomMapping>(iter: impl Iterator<Item = U>) -> U {
    iter.map(|x| x.random_mapping()).sum()
}
```

## 6.3 Dynamic Dispatch: `dyn Trait`

Usually used with some kind of pointer like `Box<dyn Trait>`, `Arc<dyn Trait>` or `&dyn Trait`.

### Best when:
* You need runtime polymorphism.
* You need to **store different implementations** in one collection.
* You want to **abstract internals behind a stable interface**.
* You are writing a **plugin-style architecture**.

### Example: Heterogeneous collection

```rust
trait Animal {
    fn greet(&self) -> String;
}

fn all_animals_greeting(animals: Vec<Box<dyn Animal>>) {
    for animal in animals {
        println!("{}", animal.greet())
    }
}
```

## 6.4 Trade-off summary

|                   | Static Dispatch (impl Trait) | Dynamic Dispatch (dyn Trait) |
|-------------------|------------------------------|------------------------------|
| Performance       | Faster, inlined              | Slower: vtable indirection   |
| Compile time      | Slower: monomorphization     | Faster: shared code          |
| Binary size       | Larger: per-type codegen     | Smaller                      |
| Flexibility       | Rigid, one type at a time    | Can mix types in collections |

* Prefer generics/static dispatch when you control the call site and want performance.
* Use dynamic dispatch when you need abstraction, plugins or mixed types.

## 6.5 Best Practices for Dynamic Dispatch

### Use Dynamic Dispatch When:

* You need heterogeneous types in a collection.
* You want runtime plugins or hot-swappable components.
* You want to abstract internals from the caller.

### Avoid Dynamic Dispatch When:

* You control the concrete types.
* You are writing code in performance critical paths.
* You can express the same logic with generics.

## 6.6 Trait Objects Ergonomics

* Prefer `&dyn Trait` over `Box<dyn Trait>` when you don't need ownership.
* Use `Arc<dyn Trait + Send + Sync>` for shared access across threads.
* Don't use `dyn Trait` if the trait has methods that return `Self`.
* **Avoid boxing too early**. Don't box inside structs unless required.
* **Object Safety**: You can only create `dyn Traits` from object-safe traits:
    * No generic methods.
    * Doesn't require `Self: Sized`.
    * All method signatures use `&self`, `&mut self` or `self`.
