---
# https://vitepress.dev/reference/default-theme-home-page
layout: home

hero:
  name: "Nymph"
  text: "Programming Language"
  tagline: "A simple language that gets out of your way."
  actions:
    - theme: brand
      text: Get Started
      link: /guide/

features:
  - title: Expression-oriented
    details: "Immutable values, matches, folds, and state loops make data flow explicit."
  - title: Structs, enums, and matching
    details: Model your domain with product and sum types, then take them apart with exhaustive pattern matching.
  - title: No null, no exceptions
    details: Absence is Option, failure is Result — both ambient, both checked, so callers can't forget the edge cases.
---

Core APIs such as `Option`, `Result`, operators, and iteration are ambient. Other standard-library
modules are opt-in through `std/...`; project modules use source-rooted `@/...` or relative
`./...` and `../...` imports.

::: code-group

```nym [hello_world.nym]
import std/io

func main() = {
  io.println("Hello world!")
}
```

```nym [functions.nym]
func factorial(n: int): int = match (n) {
  ..=1 -> 1,
  _ -> n * factorial(n - 1),
}
```

```nym [types.nym]
enum BinaryTree<T> {
  Leaf(value: T),
  Node(left: BinaryTree<T>, right: BinaryTree<T>),
}
```

```nym [lists.nym]
func odd_squares(nums: #[int]): #[int] = nums
  .iter()
  .filter((value: int) -> value % 2 == 1)
  .map((value: int) -> value ** 2u)
  .to_list()
```

:::
