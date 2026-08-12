# Functions

## Declaring a function

`func name(params): ReturnType = body`. The body is a single expression — almost always a block —
whose value is the function's result; there's no separate `return` needed for the common case of
"the last thing computed is the answer."

```nym
func add(a: int, b: int): int = a + b

func classify(n: int): string = {
  if (n < 0) { return "negative" }
  if (n == 0) { return "zero" }
  "positive"
}
```

The return type can be omitted and inferred from the body:

```nym
func doubled(x: int) = x * 2
```

## Callable labels

A named function's name is its callable label, so `return@name value` explicitly returns from it.
Closures may write a label as `label@(params) -> body` or `(params) -> label@{ body }`. If both
positions are used, their labels must match. Labels are lexical and cannot escape across a nested
function or closure boundary.

## Parameters

Each parameter is `name: Type`. Parameter bindings and values are immutable. Functions return a
replacement value when they need to represent an update.

```nym
func inc_twice(x: int): int = x + 2
```

```nym
struct Counter(n: int)
func bump(c: Counter): Counter = Counter(n = c.n + 1)
```

### Spread parameters

A `...`-prefixed parameter still declares a single list-typed parameter (`...xs: #[int]` is a
parameter of type `#[int]`) — it's a marker for "this is meant to be spread into," not a variadic
parameter list. Calling it takes a spread list argument:

```nym
func sum(...xs: #[int]): int = {
  xs.iter().fold(0, (total, x) -> total + x)
}

func demo(): int = sum(...#[1, 2, 3])
```

## Generics

`<T>` after the name declares a type parameter; `<T: Interface>` constrains it to types
implementing `Interface` (see [Interfaces and impls](./interfaces-and-impls#bounds)), and `<T:
A + B>` requires more than one. Generic arguments are always **inferred** from the call's
arguments — there is no explicit `f<int>(x)` syntax to pin them.

```nym
func id<T>(x: T): T = x

interface Area { func area(): int }

func biggest<T: Area>(a: T, b: T): int = {
  let first = a.area()
  let second = b.area()
  if (first > second) { first } else { second }
}
```

Calls through generic bounds use the implementation selected for the concrete argument type. This
includes blanket implementations; an implementation written for that concrete type still wins when
both apply. Receivers and explicit arguments are evaluated once, from left to right, before any
compiler-managed generic information is used.

## Higher-order functions

A function value's type is written `(Params) -> Return` — see [Types](./types#compound-types).
A plain function name used as a value (not called) has this type, and so does a
[closure](./expressions#closures):

```nym
func apply_twice(f: (int) -> int, x: int): int = f(f(x))

func double(x: int): int = x * 2
func demo(): int = apply_twice(double, 3)
func demo2(): int = apply_twice((x: int) -> x * 2, 5)
```

## `namespace func`

Inside a `struct`, `enum`, or `interface` body — never at the top level of a module — a static
function can use this declaration form:

- `namespace func` — a static, invoked on the type itself (`Type.name(...)`), not an instance.

```nym
struct Counter(n: int) {
  func bumped(): Counter = Counter(n = this.n + 1)
  namespace func zero(): Counter = Counter(n = 0)
}

func demo(): int = {
  let before = Counter.zero()
  let after = before.bumped()
  after.n
}
```

See [Declarations](./declarations#func) for where each function kind is and isn't allowed to
appear, and [Structs and enums](./structs-and-enums) for methods in their full context.
