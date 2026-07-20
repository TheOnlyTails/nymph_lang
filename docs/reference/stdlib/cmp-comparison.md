# `@/ops`: comparison

`Comparable` is one of the interfaces in the stdlib's **operator prelude** — unlike the rest of the
standard library, it (along with the rest of `@/ops`) is ambient: available in every module with no
`import`, which is why every sample below implements it directly. See [Operators](../operators)
for the full prelude.

## `Order`

```nymph
enum Order {
  LessThan,
  Equal,
  GreaterThan,
}
```

The three-way result of a comparison — what `compare_to` below returns.

## `Comparable`

```nymph
interface Comparable<Other> {
  func compare_to(other: Other): Order

  func less_than(other: Other): boolean = this.compare_to(other) == Order.LessThan
  func less_than_eq(other: Other): boolean = this.compare_to(other) != Order.GreaterThan
  func greater_than(other: Other): boolean = this.compare_to(other) == Order.GreaterThan
  func greater_than_eq(other: Other): boolean = this.compare_to(other) != Order.LessThan
}
```

Implementing `compare_to` alone is enough: `<`, `<=`, `>`, and `>=` are default methods built on
top of it, so all four operators start working the moment `compare_to` exists.

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

`int`, `float`, `char`, `string`, and `boolean` all implement `Comparable` against themselves out of
the box, so `<`/`<=`/`>`/`>=` already work on every built-in scalar without writing anything.

`Comparable` is also what backs a [range](../literals#ranges)'s bound type — a range's index type
`Idx` must implement `Comparable<Idx>` for the range to be constructible at all.

## `Equals`

A related, separate interface — see [Operators](../operators#equals) — provides `.equals()`/
`.not_equals()` through a blanket impl on every type. It's unrelated to `==`/`!=`, which never
dispatch to it.
