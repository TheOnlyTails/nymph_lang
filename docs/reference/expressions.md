# Expressions

Nymph is expression-oriented: `if`, `match`, `while`, `for`, and blocks all *produce* a value,
not just literals and operator chains. The only things that are *not* expressions are a bare `let`
binding and the handful of top-level [declarations](./declarations).

## Operators and precedence

From lowest to highest binding strength:

| Tier             | Operators                                    |
| ---------------- | --------------------------------------------- |
| Assignment        | `=`, `+=`, `-=`, `*=`, `/=`, `%=`, `**=`, `<<=`, `>>=`, `&=`, `^=`, `\|=`, `~=`, `&&=`, `\|\|=` |
| Pipe               | `\|>`                                         |
| Logical or         | `\|\|`                                        |
| Logical and        | `&&`                                          |
| Equality           | `==`, `!=`                                    |
| Comparison         | `<`, `<=`, `>`, `>=`                          |
| Inclusion          | `in`, `!in`                                   |
| Unwrap             | `??`                                          |
| Bitwise or         | `\|`                                          |
| Bitwise xor        | `^`                                            |
| Bitwise and        | `&`                                            |
| Shift              | `<<`, `>>`                                    |
| Range              | `..`, `..=`                                   |
| Additive           | `+`, `-`                                      |
| Multiplicative     | `*`, `/`, `%`                                 |
| Power              | `**`                                          |
| Pattern test       | `is`, `!is`                                   |
| Cast               | `as`                                           |
| Unary              | `!`, `-`, `~`                                 |
| Indexing           | `x[i]`                                        |
| Member access      | `x.field`                                     |
| Call               | `f(x)`                                        |

Most binary and unary operators are backed by an interface from the stdlib's ambient operator
prelude, so user types can overload them by implementing the matching interface — see
[Operators](./operators) for the full list and how dispatch works. `==`/`!=` are the one exception:
they always compare by native identity/structural equality and never dispatch anywhere, even for a
type that implements `Equals`.

```nym
func polynomial(x: int): int = x ** 3 + 2 * x ** 2 - x + 7
```

## Function calls

A call is a callee expression followed by parenthesized, comma-separated arguments. Arguments to a
**plain function** are matched to parameters by position — argument names are not accepted there
(named arguments are for [struct and enum construction](./structs-and-enums) instead, which is
parsed the same way but resolved by field name).

```nym
func add(a: int, b: int): int = a + b
func demo(): int = add(1, 2)
```

A `...` prefix on a call argument spreads an iterable into the call. It's how you pass a list to a
[spread parameter](./functions#spread-parameters):

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

> [!NOTE] No explicit generic arguments at a call site
> A generic function's type parameters are always inferred from the argument types — there is no
> `f<int>(x)` turbofish-style syntax for pinning them explicitly at the call.

## Method calls

`receiver.method(args…)` resolves `method` against the receiver's type: an inherent method, an
interface method available through a bound, or one materialized through an `impl … for` block.

```nym
struct Counter(n: int) {
  func peek(): int = this.n
}

func demo(c: Counter): int = c.peek()
```

## Indexing

`x[i]` reads an element out of a [list](./literals#lists), [tuple](./literals#tuples), or
[map](./literals#maps). A tuple index must be a literal integer (its element types can differ per
position, so the checker needs a constant to know which one you get back).

```nym
func first_of(xs: #[int]): int = xs[0]
func swap_sum(t: #(int, int)): int = t[1] + t[0]
func lookup(scores: #{int: int}, level: int): int = scores[level]
```

Indexing is also a valid assignment target — see [Field assignment](./mutability#field-assignment)
for the general mutability rule it falls under:

```nym
func set(xs: #[int], i: int, v: int): #[int] = {
  xs[i] = v
  xs
}
```

## Ranges

`a..`, `a..b`, `a..=b`, `..b`, and `..=b` build range values. The spelling `a..=` is invalid:
inclusive ranges require an upper bound. See [Ranges](./literals#ranges) for the full syntax and
[Iteration](./iteration#ranges) for using one as a `for` source.

```nym
func sum_inclusive(n: int): int = {
  let mut total = 0
  for (i in 1..=n) {
    total = total + i
  }
  total
}
```

## `if` and `match` as expressions

Both branches of an `if`/`else` — and every arm of a `match` — must agree on their type when the
result is used as a value, since the whole construct itself has a type. See
[Pattern matching](./pattern-matching) for everything `match` can destructure.

```nym
func clamp(x: int, lo: int, hi: int): int =
  if (x < lo) { lo }
  else if (x > hi) { hi }
  else { x }

func vowel_index(c: char): int = match (c) {
  'a' -> 1,
  'e' -> 2,
  'i' -> 3,
  'o' -> 4,
  'u' -> 5,
  _ -> 0,
}
```

An `if` with no `else`, used where no value is needed, is fine in statement position:

```nym
func demo(n: int): int = {
  let mut x = 0
  if (n > 0) { x = n }
  x
}
```

## Blocks

`{ stmt; stmt; expr }` runs each statement in order and evaluates to its last expression (or `void`
if the block ends in a statement, not an expression). Blocks are how `func` bodies, `if`/`while`/
`for` bodies, and match arm bodies all get more than one step.

```nym
func average_scaled(a: int, b: int, scale: int): float = {
  let sum = a + b
  let scaled = sum * scale
  scaled / 2
}
```

## Closures

A closure is an anonymous function value: `params -> body`. A single untyped parameter can skip
the parentheses; anything else — zero params, multiple params, or a typed param — needs them.

```nym
func doubled(n: int): int = (10 |> x -> x * n)
```

```nym
func apply_twice(f: (int) -> int, x: int): int = f(f(x))
func demo(): int = apply_twice((x: int) -> x * 2, 3)
```

A closure captures its enclosing scope the way a JS arrow function does: reading and — if the
outer binding is `let mut` — reassigning an outer variable both work from inside the closure body,
and a closure built inside a method body keeps reading the original receiver's fields even after
the method returns.

```nym
func demo(): int = {
  let mut x = 1
  let bump = () -> { x = x + 1 }
  bump()
  x
}
```

`return` exits the nearest enclosing callable. Inside an explicit closure it therefore exits that
closure, and its value must match the closure's return type; it does not return from the function
that created the closure. It can occur in any grammar-valid expression position. Compiler-generated
helpers used to evaluate expression-valued control flow are transparent: they neither capture nor
retarget a source `return`.

## Anonymous closure parameters

A closure short enough that naming its parameters is just noise can skip the header
entirely and refer to its arguments positionally: `$0` is the first, `$1` the second,
and so on, with a bare `$` as a shorthand for `$0`. An expression that mentions any
`$N` *implicitly becomes a closure* — no `->` needed.

```nym
func apply(f: (int) -> int, x: int): int = f(x)
func demo(): int = apply($ + 1, 5)
```

```nym
func combine(f: (int, int) -> int): int = f(7, 2)
func demo(): int = combine($0 - $1)
```

They read especially well as the argument to a transforming method — `o.map($ + 1)`
is exactly `o.map((x: int) -> x + 1)`:

```nym
func inc(o: Option<int>): Option<int> = o.map($ + 1)
func evens(o: Option<int>): Option<int> = o.filter($ % 2 == 0)
```

Which enclosing expression becomes the closure's body — the **boundary** — is chosen
by the *types*, not by punctuation: it's the smallest enclosing expression for which
the resulting closure type-checks in its position. That's why `$ % 2 == 0` above
becomes the whole predicate `(x) -> x % 2 == 0` rather than `((x) -> x % 2) == 0`:
only the wider reading is a `(int) -> boolean`, which is what `filter` wants. The
search runs at each spot a closure is expected — a call argument, a `let`
initializer, a `return` operand, a constructor field — and can't cross out past that
spot, so a `$` always resolves to the nearest such boundary.

## Pipe

`a |> f` calls `f` with `a` as its sole argument — `f` can be any single-argument callable: a named
function or a closure. Chained pipes are left-associative, so `x |> f |> g` is `g(f(x))`.

```nym
func double(x: int): int = x * 2
func inc(x: int): int = x + 1
func demo(): int = 10 |> double |> inc
```

The right-hand side can also be a closure — including a parenthesized
[anonymous-parameter](#anonymous-closure-parameters) one, handy for a one-off step
that doesn't deserve a name:

```nym
func demo(): int = 10 |> ($ * 2) |> ($ + 1)
```

## `as` and `is`

`value as Type` casts `value` to `Type`. Between the built-in scalar types (`int`, `uint`, `float`,
`char`) it runs Nymph's own defined conversion (see the [Cast semantics](./types) built into each
pair) and always produces the canonical boxed representation of the destination type, including
identity and widening casts. Numeric-to-`char` casts truncate floats toward zero, then require a
Unicode scalar value (`0..=0x10FFFF`, excluding `0xD800..=0xDFFF`). Invalid literal casts are
compile-time errors; invalid dynamic values fail deterministically at runtime. A cast evaluates its
source exactly once. For a user type, it dispatches to an implementation of the ambient
`Into<Other>` interface.
`value is Pattern` / `value !is Pattern` tests `value` against a single [pattern](./pattern-matching)
without a full `match` — it accepts the same pattern shapes a match arm does, just without a guard
(guards are match-arm syntax, not part of a pattern).

```nym
func f(n: int): boolean = n as float > 0.0
```

```nym
enum Shape { Circle(radius: int), Square(side: int) }
func is_big_circle(s: Shape): boolean = s is Circle(radius = 20)
```

## `in` and `!in`

`item in collection` / `item !in collection` dispatch to the ambient `Contains<Item>` interface's
`contains`/`not_contains` methods — note the receiver is the *collection*, the right-hand operand,
not the left-hand `item`.

```nym
struct Bag(n: int)
impl Contains<Item = int> for Bag {
  func contains(item: int): boolean = item == this.n
}

func has(b: Bag, x: int): boolean = x in b
func lacks(b: Bag, x: int): boolean = x !in b
```

## `??` (Unwrap)

`a ?? fallback` dispatches to the ambient `Unwrap<Output>` interface's `unwrap` method, called
eagerly as `a.unwrap(fallback)`. Nymph has no null/undefined-style optional representation, so
unlike a nullish-coalescing operator in other languages, this is always a plain, unconditional call
— nothing here short-circuits at the language level; whatever short-circuiting behavior exists is
up to `unwrap`'s own body.

```nym
struct MaybeInt(present: boolean, value: int)
impl Unwrap<Output = int> for MaybeInt {
  func unwrap(default: int): int = if (this.present) { this.value } else { default }
}

func get(m: MaybeInt, d: int): int = m ?? d
```

## `return`, `break`, `continue`

All three are expressions typed [`never`](./types#basic-types) — the type of an expression that
never produces a value because control leaves right there — so they can appear anywhere a value of
any type is expected, including as an operand.

```nym
func classify(n: int): string = {
  if (n < 0) { return "negative" }
  if (n == 0) { return "zero" }
  "positive"
}
```

```nym
func first_positive(xs: #[int]): int = {
  let mut i = 0
  while (i < 10) {
    if (xs[i] > 0) { break }
    i += 1
  }
  i
}
```

`break` and `continue` target the innermost lexically enclosing `while` or `for` loop. Label a loop
as `while@outer (...)` or `for@outer (...)`, then target it as `break@outer value` or
`continue@outer`. A block is labeled `outer@{ ... }`; `return@outer value` completes that block,
and all such returns unify with its direct tail value. A callable body is a boundary: control can
never target a construct outside the current callable. Unlabeled return still targets the nearest
callable.

A loop with no targeting `break` has type `void`. If it contains bare `break`, its result is
`Option<#()>` (`Some(#())` on the early exit and `None` on natural exhaustion). If every targeting
break supplies a value of type `T`, the result is `Option<T>` instead. Bare and valued breaks may
not be mixed in one loop, and all valued breaks must agree on `T`. This is determined by a lexical
scan of the whole loop body, including unreachable branches; breaks in nested loops or callable
bodies are deliberately excluded.
