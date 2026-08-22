# Pattern matching

`match (value) { pattern -> body, … }` compares `value` against each arm's pattern in order and
runs the first one that matches. Arms are separated by commas; the whole `match` is an
[expression](./expressions#if-and-match-as-expressions), so every arm's body must agree on a
common type when the result is used as a value.

## Literal patterns

Int, uint, float, char, string, and boolean literals match themselves:

```nym
func vowel_index(c: char): int = match (c) {
  'a' -> 1,
  'e' -> 2,
  _ -> 0,
}

func classify(s: string): int = match (s) {
  "yes" -> 1,
  "no" -> 0,
  _ -> -1,
}
```

## Bindings and the wildcard

A bare identifier binds the whole matched value under that name; `_` matches anything and binds
nothing. `name = pattern` also binds the whole value to `name`, while requiring the nested pattern
to match. Binding subpatterns can appear anywhere a pattern can, and can be nested to retain both a
structured value and its parts.

```nym
func first_or(xs: #[int], fallback: int): int = match (xs) {
  #[] -> fallback,
  n -> n[0],
}
```

```nym
func endpoints(pair: #(int, int)): int = match (pair) {
  whole = #(first, last) -> whole[0] + first + last,
}
```

Each name may be bound only once in a pattern. Both alternatives of a union must bind exactly the
same names with the same inferred types. Names are compared by identity, not by the
order in which they appear; put a binding around a grouped union when both alternatives should
capture the whole value:

```nym
func one_or_two(n: int): int = match (n) {
  matched = (1 | 2) -> matched,
  _ -> 0,
}
```

## Ranges

Range patterns use the same five valid forms as [range expressions](./literals#ranges): `a..`,
`a..b`, `a..=b`, `..b`, and `..=b`. The spelling `a..=` is invalid because an inclusive range
requires an upper bound. A range pattern matches anything the corresponding range would contain.

```nym
func http_class(code: int): int = match (code) {
  200 -> 1,
  400..500 -> 3,
  500..=599 -> 4,
  _ -> 0,
}
```

## Unions

`A | B` matches if either sub-pattern matches — handy for grouping several literals onto one arm:

```nym
enum Color { Red, Green, Blue }
func is_warm(c: Color): boolean = match (c) {
  Red | Green -> true,
  Blue -> false,
}
```

Alternatives are tested from left to right and stop at the first match. Only that alternative's
bindings are extracted. The value being matched is evaluated once, including when a union is nested
inside another destructuring pattern. Unions whose alternatives bind no names remain valid.

Wrap a union in parentheses when it needs to read as a single pattern in context:

```nym
func small(n: int): boolean = match (n) {
  (1 | 2) -> true,
  _ -> false,
}
```

## Guards

`pattern if (condition) -> body` only matches when the pattern matches **and** the condition
holds; the condition can read anything the pattern bound. A guard's condition must be parenthesized
— without the parens, a guard ending in a bare identifier would otherwise parse as a
[closure](./expressions#closures) (`ident -> body`).

```nym
func route(n: int, limit: int): int = match (n) {
  x if (x > limit) -> -1,
  x -> x,
}
```

## Struct and variant patterns

`Name(field = pattern, …)` matches a struct value or an enum variant. Struct fields always use the
explicit `field = pattern` form. In variant patterns, a bare field name is shorthand for binding the
field under that name. `...` matches the rest of the fields without binding them.

Inside a struct or variant's field list, the name to the left of the first `=` selects the field;
it is not a whole-value binding. A nested `name = pattern` on the right retains its general binding
meaning: `Point(x = captured = 0, y = _)` selects `x`, binds that field value as `captured`, and
requires it to equal `0`.

```nym
struct Point(x: int, y: int)
func on_axis(p: Point): boolean = match (p) {
  Point(x = 0, y = _) -> true,
  Point(x = _, y = 0) -> true,
  _ -> false,
}
```

```nym
struct Point(x: int, y: int)
func x_only(p: Point): int = match (p) {
  Point(x = x, ...) -> x,
}
```

```nym
func flatten(oo: Option<Option<int>>): int = match (oo) {
  Some(value = Some(value)) -> value,
  Some(value = None) -> -1,
  None -> -2,
}
```

A nullary variant (no fields) is matched bare, by name — same as constructing it:

```nym
enum Light { Red, Yellow, Green }
func next(l: Light): Light = match (l) {
  Red -> Green,
  Green -> Yellow,
  Yellow -> Red,
}
```

## List patterns

`#[]` matches an empty list; `#[a, b]` matches an exact length; `#[first, ...rest]` (or
`...rest, last`, or both) splits off a prefix/suffix and binds the remainder as a list. A bare
`...` (no name) matches the remaining elements without binding them.

```nym
func describe(xs: #[int]): int = match (xs) {
  #[] -> 0,
  #[only] -> only,
  #[first, ...mid, last] -> first + last,
  _ -> -1,
}
```

## Tuple patterns

`#(a, b, …)` matches a tuple positionally, one sub-pattern per slot:

```nym
func quadrantish(p: #(int, int)): int = match (p) {
  #(0, 0) -> 0,
  #(x, y) if (x > 0 && y > 0) -> 1,
  #(x, _) if (x < 0) -> 2,
  _ -> 3,
}
```

## Map patterns

`#{ key: pattern, … }` matches specific keys' values; a trailing `...` allows other keys to be
present without matching every one of them.

```nym
func has_one(m: #{int: int}): boolean = match (m) {
  #{ 1: _, ... } -> true,
  _ -> false,
}
```

## `is` / `!is`: a single pattern outside `match`

For a yes/no test against one pattern, `value is Pattern` / `value !is Pattern` avoids a whole
`match` — see [Expressions](./expressions#as-and-is). It accepts the same pattern grammar as a
match arm, just without a guard (guards are match-arm syntax, not part of the pattern itself).

```nym
enum Shape { Circle(radius: int), Square(side: int) }
func is_big_circle(s: Shape): boolean = s is Circle(radius = 20)
```
