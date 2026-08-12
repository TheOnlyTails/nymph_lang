# shapes

Model a few geometric figures and compute their areas — the classic tour of Nymph's
type system, using nothing but the language itself.

What it shows:

- **`enum` with per-variant fields** — `Circle(radius)`, `Rectangle(width, height)`,
  `Triangle(base, height)` are all one `Figure` type.
- **`interface` + `impl … for …`** — `Shape` is the contract; `Figure` fulfills it.
- **Exhaustive `match`** — every variant must be handled, checked at compile time.
  Add a new figure and the compiler points you at the `match` that needs updating.
- **Generics with bounds** — `describe<T: Shape>` works for any `Shape`, not just
  `Figure`.
- **`...` field elision** — `Circle(...)` matches the variant without binding fields.

**Status:** ✅ Runs today — pure language, no aspirational imports.
