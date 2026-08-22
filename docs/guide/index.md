# Getting started

Nymph is a small, expression-oriented language that compiles to JavaScript. This page is a guided
tour: each section adds one more piece, building toward a small program that ties several features
together. For the full grammar and semantics behind any of it, the [Reference](../reference/) is
the place to go next — this page links to it throughout.

## Your first function

A Nymph program is a flat list of top-level declarations. The simplest is a function: a name, its
parameters, a return type, and a body that's a single expression.

```nym
func greet(name: string): string = "Hello, ${name}!"
```

`${…}` inside a string is interpolation — exactly one complete expression goes between the braces.
A body with more than one step is a block, whose last expression is the value the function returns:

```nym
func average_scaled(a: int, b: int, scale: int): float = {
  let sum = a + b
  let scaled = sum * scale
  scaled / 2
}
```

`let` introduces a local binding. See [Functions](../reference/functions) for parameters,
generics, and higher-order functions, and [Expressions](../reference/expressions) for everything
that can appear in a body — `if`/`match` as values, closures, and the full operator set.

## Immutable values

Values and bindings in Nymph are immutable. Operations return replacements rather than changing
existing values:

```nym
struct Counter(value: int)
func increment(counter: Counter): Counter = Counter(value = counter.value + 1)
func demo(): #(int, int) = {
  let before = Counter(value = 0)
  let after = increment(before)
  #(before.value, after.value)
}
```

Repeated state uses an immutable state loop. Each `continue` installs fresh loop-carried values
simultaneously:

```nym
func sum_to(limit: int): int = loop (let next = 1, let total = 0) {
  if (next > limit) { break total }
  continue(next = next + 1, total = total + next)
}
```

See [Immutability and migration](../reference/mutability) for translating legacy mutable code.

## Structs

A `struct` groups fields under a name; construct one by calling it with named arguments. See
[Structs and enums](../reference/structs-and-enums) for field defaults, generics, and methods in
depth.

```nym
struct Point(x: int, y: int)

func manhattan(a: Point, b: Point): int = {
  let dx = a.x - b.x
  let dy = a.y - b.y
  dx.abs() + dy.abs()
}
```

## Enums and pattern matching

An `enum` is a fixed set of named variants, each optionally carrying its own fields. `match`
destructures one back apart — see [Pattern matching](../reference/pattern-matching) for the full
grammar (ranges, structs, lists, tuples, guards, and more).

```nym
enum Shape {
  Circle(radius: int),
  Square(side: int),
  Dot,
}

func area(s: Shape): int = match (s) {
  Circle(radius) -> 3 * radius * radius,
  Square(side) -> side * side,
  Dot -> 0,
}
```

## Operators are interfaces

Arithmetic, comparison, and a handful of other operators are backed by interfaces the standard
library ships as an always-available prelude — implement one for your own type and the matching
operator syntax starts working for it. See [Operators](../reference/operators) for the complete
list.

```nym
struct Vec2(x: int, y: int)

impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
  func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
}

func combine(a: Vec2, b: Vec2): Vec2 = a + b
```

## Closures and pipes

A closure is a small anonymous function, `params -> body`; `|>` calls a function with the value on
its left as the sole argument, and chains left to right. Both are covered in
[Expressions](../reference/expressions#closures).

```nym
func double(x: int): int = x * 2
func inc(x: int): int = x + 1
func demo(): int = 10 |> double |> inc
```

```nym
func apply_twice(f: (int) -> int, x: int): int = f(f(x))
func demo2(): int = apply_twice((x: int) -> x * 2, 3)
```

## Looping over things

The `for (pat in src) { .. }` loop walks over a range, a list, or anything implementing one of the
standard library's iteration interfaces:

```nym
func sum(): int = {
  (1..=4).iter().fold(0, $0 + $1)
}
```

```nym
func sum_list(): int = {
  #[1, 2, 3, 4].iter().fold(0, $0 + $1)
}
```

See [Iteration](../reference/iteration) for looping over your own types.

## Putting it together

A slightly bigger example, combining a struct with a method, an enum matched with a guard, and a
generic bounded function — the same shape a real program's core logic tends to take:

```nym
interface Area { func area(): int }

enum Shape { Circle(radius: int), Rectangle(w: int, h: int) }

struct Sprite(pos: #(int, int), shape: Shape)
impl Area for Sprite {
  func area(): int = match (this.shape) {
    Circle(radius) -> 3 * radius * radius,
    Rectangle(w, h) if (w == h) -> w * w,
    Rectangle(w, h) -> w * h,
  }
}

func biggest<T: Area>(a: T, b: T): int = {
  let first = a.area()
  let second = b.area()
  if (first > second) { first } else { second }
}

func scene(): int = {
  let a = Sprite(pos = #(0, 0), shape = Circle(radius = 2))
  let b = Sprite(pos = #(3, 4), shape = Rectangle(w = 5, h = 5))
  biggest(a, b)
}
```

## Next steps

From here, the [Reference](../reference/) covers each piece in full:

- [Literals](../reference/literals) and [Types](../reference/types) — the built-in values and how
  they're typed.
- [Declarations](../reference/declarations) — everything a module can be made of.
- [Functions](../reference/functions), [Structs and enums](../reference/structs-and-enums), and
  [Interfaces and impls](../reference/interfaces-and-impls) — the shapes user code takes.
- [Pattern matching](../reference/pattern-matching) and [Operators](../reference/operators) — the
  two topics this tour only sampled.
- [Immutability and migration](../reference/mutability) and [Iteration](../reference/iteration) — the two rules
  that shape how state and loops behave.
