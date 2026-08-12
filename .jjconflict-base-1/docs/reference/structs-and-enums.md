# Structs and enums

## Structs

A `struct` is a fixed set of named, typed fields — a product type.

```nym
struct Point(x: int, y: int)
```

### Construction

Construct a struct by calling its name with **named** arguments, one per field — order doesn't
matter when every argument is named:

```nym
struct Point(x: int, y: int)
func origin(): Point = Point(x = 0, y = 0)
func demo(): Point = Point(y = 1, x = 2)
```

### Field defaults

A field can declare a default value with `= expr`; a construction call may then omit that
argument entirely, and override just the ones it needs to:

```nym
struct Config(retries: int = 3, verbose: boolean = false)

func demo(): int = Config().retries
func verbose_demo(): boolean = Config(verbose = true).verbose
```

### Field access

`.field` reads a field. Fields are immutable; construct a replacement value to represent an update.

```nym
struct Segment(from: Point, to: Point)
struct Point(x: int, y: int)

func length_sq(s: Segment): int = {
  let dx = s.to.x - s.from.x
  let dy = s.to.y - s.from.y
  dx * dx + dy * dy
}
```

### Methods

A method body goes either inside the struct itself, or in a separate `impl` block — the two forms
are equivalent, and a struct can use both at once. Inside a method, `this` refers to the receiver.

```nym
struct Account(balance: int, overdraft: int) {
  func available(): int = this.balance + this.overdraft
}

impl Account {
  func can_spend(amount: int): boolean = amount <= this.available()
}
```

### Generic structs

`<T>` after the name declares a type parameter, usable in the field list and any method:

```nym
struct Slot<T>(value: T, occupied: boolean)

impl<T> Slot<T> {
  func get(): T = this.value
  func is_free(): boolean = !this.occupied
}

func read_int(s: Slot<int>): int = if (s.is_free()) { 0 } else { s.get() }
```

A struct's own generic parameter can carry a bound, checked at every construction site:

```nym
interface Area { func area(): int }
struct Square(side: int)
impl Area for Square { func area(): int = this.side * this.side }

struct Container<T: Area>(value: T)

func make(s: Square): Container<Square> = Container(value = s)
```

## Enums

An enum is a nominal static view over a canonical, deduplicated set of single-variant types. A
variant can carry its own fields (exactly like a struct's) or none at all. Every qualified variant
is also a source-nameable type.

```nym
enum Shape {
  Circle(radius: int),
  Square(side: int),
  Dot,
}
```

### Construction

A variant with fields is constructed the same way a struct is — named arguments, by variant name.
A variant name that's unambiguous across every enum in scope can be used bare; otherwise (or just
for clarity) qualify it with the enum's name:

```nym
enum Shape { Circle(radius: int), Square(side: int), Dot }

func a_dot(): Shape = Dot
func a_circle(r: int): Shape = Circle(radius = r)
func qualified(): Shape = Shape.Dot
```

### Matching

`match` is how you get a variant's fields back out — see [Pattern matching](./pattern-matching)
for the full pattern grammar:

```nym
enum Shape { Circle(radius: int), Square(side: int), Dot }

func area(s: Shape): int = match (s) {
  Circle(radius) -> 3 * radius * radius,
  Square(side) -> side * side,
  Dot -> 0,
}
```

### Methods

Exactly like a struct, an enum can declare methods inline or via `impl`. The one difference: `this`
on an enum receiver has no fields of its own to read directly (a variant's fields are only reachable
by first matching `this` against a variant pattern):

```nym
enum Shape { Circle(radius: int), Square(side: int) }
impl Shape {
  func area(): int = match (this) {
    Circle(radius) -> 3 * radius * radius,
    Square(side) -> side * side,
  }
}

func total(a: Shape, b: Shape): int = a.area() + b.area()
```

### Embedding and static views

An enum may embed every variant accepted by another enum with `...Source`, or one qualified variant
with `Source.Variant`:

```nym
enum InputError { Missing, Invalid(message: string) }
enum NetworkError { Offline }
enum AppError { ...InputError, NetworkError.Offline, Cancelled }

func widen(error: InputError): AppError = error
func selected(error: NetworkError.Offline): AppError = error as AppError
```

Embedding is set inclusion, not wrapper construction. Assignment, arguments, returns, and `as` may
change the nominal static view only when the source's known variant set is a subset of the
destination set. The runtime value retains its original variant identity and fields.

Accepted sets are least fixed points. Self-embedding, mutual cycles, diamonds, and repeated paths are
legal and deduplicate the same single-variant type. Selected variants stay qualified and cannot add
fields. Exhaustiveness uses the destination's final set, while a successful qualified pattern
rebinds the source view. Methods dispatch through the current static view.

Embedding never synthesizes `Into`. An explicitly declared `Into` implementation remains legal and
is the only conversion used by `.to()`; direct set assignability takes precedence during `?`, with a
unique pure, infallible explicit `Into` as the fallback.

### `namespace func` and `self`

A `namespace func` on an enum is a static constructor or helper — `self` inside its signature
refers to the enum itself:

```nym
enum Color { Red, Green }
impl Color {
  namespace func default(): self = Red
}

func demo(): Color = Color.default()
```

### Implementing interfaces

Both structs and enums implement interfaces the same way — see
[Interfaces and impls](./interfaces-and-impls) — including the stdlib's ambient
[operator interfaces](./operators) for overloading `+`, `==`-adjacent methods, `<`, and friends.

```nym
interface Area { func area(): int }

enum Shape { Circle(radius: int), Square(side: int) }
impl Area for Shape {
  func area(): int = match (this) {
    Circle(radius) -> 3 * radius * radius,
    Square(side) -> side * side,
  }
}
```
