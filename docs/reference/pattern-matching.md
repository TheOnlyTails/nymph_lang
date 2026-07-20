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
nothing.

```nym
func first_or(xs: #[int], fallback: int): int = match (xs) {
  #[] -> fallback,
  n -> n[0],
}
```

## Ranges

`a..b` (exclusive) and `a..=b` (inclusive) match anything the range would contain — same bounds
rules as a [range expression](./literals#ranges).

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

`Name(field, …)` matches a struct value or an enum variant. A bare field name (`field`) is
shorthand for binding it under its own name; `field = pattern` matches that field against a nested
pattern (and is also how you bind it under a *different* name than its own). `...` matches the rest
of the fields without binding them.

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
  Point(x, ...) -> x,
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
