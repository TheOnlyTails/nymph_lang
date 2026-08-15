# Operators

Most of Nymph's operators are backed by an interface — implement the interface for your type and
the operator syntax starts working for it. These interfaces are part of Nymph's **ambient core**,
alongside APIs such as `Option`, `Result`, and iteration interfaces. Every one of them is available
with no `import`, in every module; this is separate from opt-in `std/...` modules.

> [!NOTE] `==` and `!=` are the exception
> Equality and inequality never dispatch anywhere — they always compare by native identity
> (structs and enums) or value (primitives), even for a type that implements `Equals`. `Equals` and
> its blanket impl exist for explicit `.equals()`/`.not_equals()` calls, not for `==`/`!=` syntax.

## Arithmetic

An unsuffixed, non-negative integer literal inherits a known `uint` or `float` type from the other
operand, so `index - 1` works when `index` is a `uint`. This applies only to literals: `-1` and values
stored in `int` variables remain signed and use the corresponding mixed-type overload. See
[Number literals](./literals#numbers) for the complete inference rule.

| Operator | Interface                  | Method      |
| -------- | -------------------------- | ----------- |
| `a + b`  | `Plus<Other, Output>`      | `plus`      |
| `a - b`  | `Minus<Other, Output>`     | `minus`     |
| `a * b`  | `Times<Other, Output>`     | `times`     |
| `a / b`  | `Divide<Other, Output>`    | `divide`    |
| `a % b`  | `Remainder<Other, Output>` | `remainder` |
| `a ** b` | `Power<Other, Output>`     | `power`     |
| `-a`     | `Negate<Output>`           | `negate`    |

```nym
struct Vec2(x: int, y: int)

impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
  func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
}
impl Minus<Other = Vec2, Output = Vec2> for Vec2 {
  func minus(other: Vec2): Vec2 = Vec2(x = this.x - other.x, y = this.y - other.y)
}
impl Times<Other = int, Output = Vec2> for Vec2 {
  func times(scale: int): Vec2 = Vec2(x = this.x * scale, y = this.y * scale)
}
impl Negate<Output = Vec2> for Vec2 {
  func negate(): Vec2 = Vec2(x = -this.x, y = -this.y)
}

func lerpish(a: Vec2, b: Vec2): Vec2 = a + (b - a) * 2
func flip(v: Vec2): Vec2 = -v
```

`Other` and `Output` don't have to be the receiver's own type — `Times<Other = int, Output = Vec2>`
above scales a `Vec2` by a plain `int` and hands back a `Vec2`, so the two operands of `*` need not
match.

### Exponentiation

The standard numeric `Power` implementations form an exact matrix:

| Base                      | Exponent                  | Result    |
| ------------------------- | ------------------------- | --------- |
| `int`                     | `uint`                    | `int`     |
| `uint`                    | `uint`                    | `uint`    |
| `float`                   | `uint`                    | `float`   |
| `int`, `uint`, or `float` | `int`                     | `float`   |
| `int`, `uint`, or `float` | `float`                   | `Complex` |
| `Complex`                 | `uint`, `int`, or `float` | `Complex` |

The `Complex`-producing rows become available with `import std/math/complex with (Complex)`.
Combinations outside the table are rejected unless a visible user implementation supplies that
combination.

Integer exponents use exponentiation by squaring. An integral-valued finite `float` exponent takes
the same algebraic fast path. Other float exponents use a real fast path for a positive real base;
negative real and Complex bases use the principal branch \(\exp(x\operatorname{Log}(z))\), with the
principal argument supplied by `atan2`.

For every accepted row, zero to the zeroth power is one and zero to a positive power is zero. Zero
to a negative power raises a runtime `RangeError`. IEEE-754 behavior is otherwise preserved,
including signed zero, `NaN`, and infinities.

## Bitwise and boolean

| Operator | Interface                   | Method    |
| -------- | --------------------------- | --------- |
| `a & b`  | `BitAnd<Other, Output>`     | `bit_and` |
| `a \| b` | `BitOr<Other, Output>`      | `bit_or`  |
| `a ^ b`  | `BitXor<Other, Output>`     | `bit_xor` |
| `~a`     | `BitNot<Output>`            | `bit_not` |
| `a << b` | `LeftShift<Other, Output>`  | `shl`     |
| `a >> b` | `RightShift<Other, Output>` | `shr`     |
| `!a`     | `Not<Output>`               | `not`     |

`int` and `boolean` already implement the bitwise set out of the box (`&`/`|`/`^`/`~` work on both
natively), so overloading them yourself is for a type of your own:

```nym
struct Mask(bits: int)
impl BitAnd<Other = Mask, Output = Mask> for Mask {
  func bit_and(other: Mask): Mask = Mask(bits = this.bits & other.bits)
}
impl BitNot<Output = Mask> for Mask {
  func bit_not(): Mask = Mask(bits = ~this.bits)
}

func combine(a: Mask, b: Mask): Mask = a & ~b
```

> [!NOTE] `&&` and `||` are not overloadable
> Logical and/or always operate on `boolean`, and always short-circuit — there is no interface
> behind them, unlike every other binary operator on this page.

## Comparison

| Operator                             | Interface           | Method(s)                                                                                                   |
| ------------------------------------ | ------------------- | ----------------------------------------------------------------------------------------------------------- |
| `a < b`, `a <= b`, `a > b`, `a >= b` | `Comparable<Other>` | `compare_to` (required); `less_than`, `less_than_eq`, `greater_than`, `greater_than_eq` (defaulted from it) |

`Comparable<Other>` needs just one method, `compare_to`, returning an `Order` (`LessThan`, `Equal`,
`GreaterThan`); the four comparison operators are default methods built on top of it, so
implementing `compare_to` alone lights up all four:

```nym
struct Version(major: int, minor: int)
impl Comparable<Other = Version> for Version {
  func compare_to(other: Version): Order =
    if (this.major != other.major) {
      if (this.major < other.major) { Order.LessThan } else { Order.GreaterThan }
    } else if (this.minor < other.minor) { Order.LessThan }
    else if (this.minor > other.minor) { Order.GreaterThan }
    else { Order.Equal }
}

func outdated(a: Version, b: Version): boolean = a < b
```

## `Equals`

`.equals(other)` / `.not_equals(other)` are explicit methods, backed by `Equals<Other>` — every
type gets them for free through a blanket impl, so they're always callable, on anything:

```nym
func same(a: int, b: int): boolean = a.equals(b)
func different(a: int, b: int): boolean = a.not_equals(b)
```

Remember: this is unrelated to `==`/`!=`, which never dispatch to `Equals` no matter what a type
implements.

## `in` / `!in`

`item in collection` and `item !in collection` dispatch to `Contains<Item>` — note the **receiver
is the collection**, the right-hand operand, with `item` passed as the argument:

```nym
struct Bag(n: int)
impl Contains<Item = int> for Bag {
  func contains(item: int): boolean = item == this.n
}

func has(b: Bag, x: int): boolean = x in b
func lacks(b: Bag, x: int): boolean = x !in b
```

`not_contains` (and so `!in`) comes for free once `contains` is implemented, the same way
`Comparable`'s extra methods do.

## `??` (Unwrap)

`a ?? fallback` dispatches to `Unwrap<Output>`'s `unwrap` method, called eagerly as
`a.unwrap(fallback)` — see [Expressions](./expressions#unwrap) for why this is unconditional rather
than short-circuiting.

```nym
struct MaybeInt(present: boolean, value: int)
impl Unwrap<Output = int> for MaybeInt {
  func unwrap(default: int): int = if (this.present) { this.value } else { default }
}

func get(m: MaybeInt, d: int): int = m ?? d
```

## `as`

For a user type, `value as Target` dispatches to `Into<Target>`'s `into` method — see
[Expressions](./expressions#as-and-is) for the built-in scalar conversions `as` runs between
`int`/`uint`/`float`/`char` without any interface involved.

```nym
struct Meters(value: int)
impl Into<string> for Meters {
  func into(): string = "${this.value}m"
}

func describe(m: Meters): string = m as string
```
