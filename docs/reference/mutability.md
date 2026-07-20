# Mutability

By default, everything in Nymph is immutable: a binding cannot be reassigned, a struct's fields
cannot be reassigned through it, and a method that would need to change its receiver cannot be
called on it. Mutability is opt-in, and always visible at the point where it is granted — either
on a `let` binding, on a function parameter, or on a field's own declared type.

## Bindings

A plain `let` binding may never be reassigned. Adding `mut` makes the binding reassignable, and
also gives the bound value's type a `mut` marker (see [Mut types](#mut-types) below) — this is
what lets `mut`-gated operations (mutating methods, field assignment) be performed through it
later on.

```nym
func demo(): int = {
  let x = 5
  let mut y = 10
  y = y + 1 // fine: y is `mut`
  x + y
}
```

```nym
func demo(): int = {
  let x = 5
  x = 6 // [!code error]
  x
}
```

## Mut types

`mut` is not only a binding-time keyword — it is also a type constructor. `mut T` describes a
value of type `T` that additionally carries permission to mutate through it: to reassign one of
its fields, or to call a [mut func](#mut-func) method on it. It can appear anywhere a type can: a
function parameter, a return type, a field declaration, or a generic argument.

```nym
struct Counter(n: int)

func bump(c: mut Counter): int = {
  c.n = c.n + 1
  c.n
}

func demo(): int = {
  let mut c = Counter(n = 0)
  bump(c)
}
```

`mut T <: T` is a **one-way** coercion: a `mut T` value may always be used where a plain `T` is
expected (mutability is simply not needed there), but a plain `T` is never usable where `mut T`
is required — nothing implicitly "promotes" a value to `mut`. In particular, neither an `as` cast
nor an `Into` conversion ever mints a `mut` value; the result of a conversion is always plain.

```nym
struct Counter(n: int)
func read(c: Counter): int = c.n

func demo(): int = {
  let mut c = Counter(n = 5)
  read(c) // fine: `mut Counter` is usable where `Counter` is wanted
}
```

```nym
struct Counter(n: int)
func bump(c: mut Counter): void = { c.n = c.n + 1 }

func demo(): void = {
  let c = Counter(n = 5)
  bump(c) // [!code error]
}
```

## Field assignment

Reassigning a field slot (`p.field = v`) type-checks only when `p`'s type is `mut` — the
immutable case is a compile error, not a silent no-op.

```nym
struct Counter(n: int)
func bump(c: Counter): void = {
  c.n = c.n + 1 // [!code error]
}
```

A field's own declared type is the sole authority on whether *it* can be mutated, independent
of the mutability of the value it lives on. A field declared `mut U` stays mutable through it
even when reached via an otherwise-immutable receiver, because reading the field itself
produces a `mut U`:

```nym
struct Counter(n: int)
struct Wrapper(inner: mut Counter)

func bump(w: Wrapper): int = {
  // `w` itself is a plain `Wrapper`, but `inner`'s declared type is `mut Counter`.
  w.inner.n = w.inner.n + 1
  w.inner.n
}

func demo(): int = {
  let mut c = Counter(n = 0)
  let w = Wrapper(inner = c)
  bump(w)
}
```

## Mut func

A method declared `mut func` (instead of plain `func`) is one that mutates its receiver — inside
its body, `this` behaves like a `mut Self`, so it may reassign its own fields.

```nym
struct Counter(n: int) {
  mut func bump(): void = { this.n = this.n + 1 }
  func peek(): int = this.n
}

func demo(): int = {
  let mut c = Counter(n = 0)
  c.bump()
  c.bump()
  c.peek()
}
```

Declaring a method `mut func` on an interface makes that receiver requirement part of the
interface's own contract: calling it — whether on a concrete type that implements the interface,
or through a generic type parameter bound to it — requires a `mut` receiver.

```nym
interface Stack<E> {
  mut func push(x: E): void
  func peek(): E
}

struct Buf(n: int) {}
impl Stack<E = int> for Buf {
  mut func push(x: int): void = { this.n = x }
  func peek(): int = this.n
}

func demo(): void = {
  let b = Buf(n = 0)
  b.push(1) // [!code error]
}
```

## Bound satisfaction

An interface can be implemented specifically for the `mut` version of a type — `impl A for mut B`
— rather than for `B` itself. This means only a `mut B` value satisfies a `T: A` bound; a plain
`B`, even though it is the "same" underlying type, does not.

```nym
interface Greet { func hello(): string }
struct Robot(name: string) {}
impl Greet for mut Robot { func hello(): string = this.name }

func greet<T: Greet>(x: T): string = x.hello()

func demo(): string = {
  let mut r = Robot(name = "Robby")
  greet(r) // fine: `mut Robot` implements `Greet`
}
```

> [!NOTE] `impl A for mut B` targeting a struct doesn't lower yet
> The sample above type-checks cleanly — that's the semantic point of this section — but
> `impl A for mut B` where `B` is a struct type doesn't have a lowering path yet, so the compiler
> panics if you try to actually compile and run it. That's a compiler gap, not a rule of the
> language — the docs only type-check their samples, so this one is covered like any other.

```nym
interface Greet { func hello(): string }
struct Robot(name: string) {}
impl Greet for mut Robot { func hello(): string = this.name }

func greet<T: Greet>(x: T): string = x.hello()

func demo(): string = {
  let r = Robot(name = "Robby")
  greet(r) // [!code error]
}
```

The diagnostic deliberately names the `mut` type that *would* satisfy the bound — the fix is
almost always to bind the argument with `let mut` instead.

> [!NOTE] Casts and `Into` never mint `mut`
> Because `mut T <: T` is one-way, and no conversion ever produces a `mut` value, the only way
> to obtain a `mut T` is to bind (or receive as a parameter) a value at that type directly —
> `let mut x = ...`, or a parameter declared `mut T`.
