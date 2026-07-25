# Declarations

A Nymph module is a flat list of declarations: bindings, functions, types, and the constructs that
attach behavior to them. This page is the map of what's declarable at the top level and how each
form is written; see [Functions](./functions), [Structs and enums](./structs-and-enums), and
[Interfaces and impls](./interfaces-and-impls) for the deeper treatment of each.

## `let` / `let mut`

A top-level `let` declares a module-level binding, evaluated once. Add `mut` to make it
reassignable — see [Mutability](./mutability) for what that actually grants.

```nym
let limit = 100
func under_limit(n: int): boolean = n < limit
```

### External lets

An intrinsic may expose an immutable host value with `external let`. The
optional marker names the linkage-registry entry; without one it defaults to
the Nymph binding name.

```nymph
public external let max_float: float
public external(min_float) let minimum: float
```

External lets differ from external functions: the generated module imports
the host export once, marshals its raw value into the declared canonical boxed
Nymph representation once, and stores that snapshot in one `const` binding.
Every reference shares that binding and identity; the host export is not read
or boxed again. `external let mut` is rejected. Ambient external lets are
emitted only when demanded, preserving single canonical type emission and
avoiding duplicate imports or initializers.

The same `let [mut] name [: Type] = value` form is also how a local binding is introduced inside a
block — see [Blocks](./expressions#blocks).

## `func`

`func name(params): ReturnType = body` declares a function. The return type can be omitted and
inferred from the body; the body is any [expression](./expressions), most often a block.

```nym
func add(a: int, b: int): int = a + b
```

Two more keywords change what a function member *is*, but both are only meaningful **inside** a
`struct`, `enum`, or `interface` body — declaring either at the top level of a module is rejected:

- `mut func` — an instance method that may mutate its receiver (`this` behaves like a `mut Self`
  inside the body). See [Mut func](./mutability#mut-func).
- `namespace func` — a static, called on the type itself (`Type.name(...)`) rather than on an
  instance.

```nym
struct Counter(n: int) {
  mut func bump(): void = { this.n = this.n + 1 }
  namespace func zero(): Counter = Counter(n = 0)
}

func demo(): int = {
  let mut c = Counter.zero()
  c.bump()
  c.n
}
```

See [Functions](./functions) for parameters, generics, and closures in depth.

## `struct`

A product type: a fixed set of named, typed fields, plus an optional body of methods and interface
impls.

```nym
struct Point(x: int, y: int)

struct Account(balance: int, overdraft: int) {
  func available(): int = this.balance + this.overdraft
}
```

See [Structs and enums](./structs-and-enums) for fields, defaults, construction, and methods.

## `enum`

A sum type: a fixed set of named variants, each optionally carrying its own fields, plus the same
kind of method/impl body a struct can have.

```nym
enum Shape {
  Circle(radius: int),
  Square(side: int),
  Point,
}

func area(s: Shape): int = match (s) {
  Circle(radius) -> 3 * radius * radius,
  Square(side) -> side * side,
  Point -> 0,
}
```

See [Structs and enums](./structs-and-enums) and [Pattern matching](./pattern-matching).

## `interface`

A named set of method (and `let`) signatures a type can promise to implement, optionally with
default bodies and super-interfaces.

```nym
interface Area {
  func area(): int
}

interface Named {
  func name(): string
}

interface LabeledArea: Area, Named {
  func label(): string
}
```

See [Interfaces and impls](./interfaces-and-impls) for default methods, bounds, and the operator
interfaces the stdlib ships as an always-available prelude — see [Operators](./operators).

## `impl`

Attaches methods to a type, either on its own (an **inherent** impl) or as the implementation of a
specific interface (an **interface impl**, `impl … for …`).

```nym
interface Area { func area(): int }
struct Square(side: int)

// Inherent: adds a method with no interface involved.
impl Square {
  func doubled_side(): int = this.side * 2
}

// Interface: satisfies `Area` for `Square`.
impl Area for Square {
  func area(): int = this.side * this.side
}
```

An inherent or interface impl can also be written **nested inside** the `struct`/`enum` body
itself, which is equivalent to a separate top-level `impl` block:

```nym
interface Area { func area(): int }
struct Circle(radius: int) {
  impl Area {
    func area(): int = 3 * this.radius * this.radius
  }
}
```

See [Interfaces and impls](./interfaces-and-impls) for generic impls, bounds, and operator
overloading.

## `namespace`

A top-level `namespace Name { … }` block groups plain `func`s and `let`s under a shared name. Since
there's no receiver to mutate and nesting another namespace would be pointless, only ordinary
`func`s and `let`s are accepted inside — a `mut func`, `namespace func`, or `namespace let` here is
rejected the same way it would be at the bare top level.

```nym
namespace MathUtils {
  func double(x: int): int = x * 2
}
```

> [!NOTE] `Namespace.member` access isn't wired up yet
> The declaration above type-checks cleanly, but calling `MathUtils.double(21)` from elsewhere does
> not — resolving a member *through* a top-level namespace name isn't implemented yet (unlike a
> struct/enum's own `namespace func`, which *is* callable as `Type.member`; see [`func`](#func)
> above). Declare the namespace, but don't rely on reaching into it yet.

## Visibility

`public`, `internal`, or `private` may prefix most top-level declarations, and a struct field
individually:

```nym
public struct Point(public x: int, public y: int)

internal func helper(): int = 1

private let secret = 42

func origin(): Point = Point(x = 0, y = 0)
```

## Imports

```nymph
import @/math               // project-rooted
import ./sibling             // relative to the current file
import ../parent             // relative to the parent directory
import pkg/name as aliased   // future external-package syntax, aliased
import @/math with (sin as sine, cos)
```

> [!NOTE] Imports don't link yet
> `import` parses and type-checks today, but nothing is actually resolved or linked in from another
> module — every sample on this page (and everywhere else in this reference) relies only on the
> stdlib's ambient **operator prelude** (see [Operators](./operators)), which is available without
> any `import` at all. Treat the syntax above as forward-looking, not yet load-bearing.
