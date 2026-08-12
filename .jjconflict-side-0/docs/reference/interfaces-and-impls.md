# Interfaces and impls

An `interface` declares a set of methods (and `let`s) a type can promise to implement; an `impl`
is where a type actually satisfies one, or gains inherent methods with no interface involved at
all.

## Declaring an interface

```nym
interface Area {
  func area(): int
}
```

An interface method can carry a **default body**, in terms of the interface's other methods —
implementors get it for free unless they override it:

```nym
interface Describable {
  func label(): int
  func doubled_label(): int = this.label() * 2
}
```

Overriding a default wins over it — an implementor's own body is always what actually runs, even
when the interface method it's overriding is only ever *called* through the default of another
method:

```nym
interface MyComparable<Other> {
  func compare_to(other: Other): int
  func less_than(other: Other): boolean = this.compare_to(other) < 0
}

struct Weird(v: int)
impl MyComparable<Other = Weird> for Weird {
  func compare_to(other: Weird): int = this.v - other.v
  func less_than(other: Weird): boolean = true
}
```

### Super-interfaces

`interface B: A { … }` declares `B` as requiring `A` too. A type satisfying both implements each
one in its **own** `impl` block — the super-interface relationship affects what a bound accepts,
not how the methods are grouped at the impl site:

```nym
interface Named { func name(): string }
interface Greeter: Named { func greet(): string }

struct Robot(id: int)
impl Named for Robot {
  func name(): string = "R-${this.id}"
}
impl Greeter for Robot {
  func greet(): string = "Hello, ${this.name()}!"
}
```

### Generic interfaces

An interface can itself be generic, most commonly to parameterize the type on the other side of a
method (an `Other` operand, an `Output`, an `Item`):

```nymph
interface Plus<Other, Output> {
  func plus(other: Other): Output
}
```

This is exactly the shape the stdlib's ambient [operator interfaces](./operators) use (in fact it's
almost verbatim `Plus` itself, already declared for you — see that page for the full list, and why
this sample can't redeclare it here).

## `impl`

### Inherent impls

`impl Type { … }` (or the equivalent nested form inside the `struct`/`enum` body) adds methods with
no interface attached:

```nym
struct Square(side: int)
impl Square {
  func doubled_side(): int = this.side * 2
}
```

### Interface impls

`impl Interface for Type { … }` satisfies `Interface` for `Type`. Every non-defaulted method (and
any defaults you want to override) goes in the body:

```nym
interface Area { func area(): int }
struct Square(side: int)
impl Area for Square {
  func area(): int = this.side * this.side
}
```

The same thing can be written nested inside the struct/enum body instead of as a separate top-level
`impl`:

```nym
interface Area { func area(): int }
struct Circle(radius: int) {
  impl Area {
    func area(): int = 3 * this.radius * this.radius
  }
}
```

### Generic impls

`impl<T> …` introduces a type parameter for the impl itself — usable both for "for every `T`"
impls (a **blanket impl**) and for implementing an interface for one specific instantiation of a
generic type:

```nym
struct Slot<T>(value: T, occupied: boolean)
impl<T> Slot<T> {
  func get(): T = this.value
  func is_free(): boolean = !this.occupied
}
```

```nym
interface Describe { func describe(): string }
impl<T> Describe for T {
  func describe(): string = "a value"
}

func demo(): string = 5.describe()
```

When both a blanket implementation and an implementation for a specific concrete type apply, the
concrete implementation takes precedence. A blanket method has one shared generic body regardless
of how many concrete receiver types use it; this does not change source-level receiver behavior or
argument evaluation order.

## Bounds

`<T: Interface>` on a function, struct, enum, or method restricts `T` to types that implement
`Interface`; `<T: A + B>` (an [intersection type](./types#compound-types)) requires more than one
at once. Inside the bounded scope, `T`'s interface methods are callable exactly as if `T` were a
concrete type:

```nym
interface Area { func area(): int }
interface Named { func name(): string }

struct Square(side: int)
impl Area for Square { func area(): int = this.side * this.side }
impl Named for Square { func name(): string = "square" }

func describe<T: Area + Named>(shape: T): string = "${shape.name()}: ${shape.area()}"
```

## `impl Trait` as a parameter shorthand

Writing an interface name directly as a parameter's type (instead of naming a bound type
parameter) is sugar for "accepts any type implementing this interface" — the parameter is used
exactly like a bounded generic inside the body, and a concrete implementing type can be passed
straight in from the call site:

```nym
interface Area { func area(): int }
struct Square(side: int)
impl Area for Square {
  func area(): int = this.side * this.side
}

func measure(shape: Area): int = shape.area()
func total(s: Square): int = measure(s)
```

## Shared method names

The same method name can be defined independently on unrelated types — inherently on one, through
different interfaces on others — and each call resolves against its own receiver's type with no
ambiguity between them:

```nym
interface Scored { func score(): int }

struct Player(points: int)
impl Scored for Player { func score(): int = this.points }

struct Judge(bias: int) {
  func score(): int = this.bias
}

func tally(p: Player, j: Judge): int = p.score() + j.score()
```
