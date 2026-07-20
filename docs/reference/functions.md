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

## Parameters

Each parameter is `name: Type`. Two modifiers can prefix a parameter, and they mean different
things:

- A `mut` **before the name** (`mut x: int`) makes the parameter binding itself reassignable inside
  the function body — purely local; the caller's own value is never affected.
- A `mut` **inside the type** (`x: mut Counter`) is a [mut type](./mutability#mut-types): it grants
  permission to mutate *through* `x` — reassign one of its fields, or call a `mut func` method on
  it.

```nym
func inc_twice(mut x: int): int = {
  x = x + 1
  x = x + 1
  x
}
```

```nym
struct Counter(n: int)
func bump(c: mut Counter): int = {
  c.n = c.n + 1
  c.n
}
```

### Spread parameters

A `...`-prefixed parameter still declares a single list-typed parameter (`...xs: #[int]` is a
parameter of type `#[int]`) — it's a marker for "this is meant to be spread into," not a variadic
parameter list. Calling it takes a spread list argument:

```nym
func sum(...xs: #[int]): int = {
  let mut total = 0
  for (x in xs) {
    total = total + x
  }
  total
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

## `mut func` and `namespace func`

Inside a `struct`, `enum`, or `interface` body — never at the top level of a module — two more
function kinds exist:

- `mut func` — an instance method allowed to mutate its receiver; see
  [Mut func](./mutability#mut-func).
- `namespace func` — a static, invoked on the type itself (`Type.name(...)`), not an instance.

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

See [Declarations](./declarations#func) for where each function kind is and isn't allowed to
appear, and [Structs and enums](./structs-and-enums) for methods in their full context.
