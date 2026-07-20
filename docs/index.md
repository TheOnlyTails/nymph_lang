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
    details: "if, match, blocks, and loops all produce values — write the answer, not a pile of statements."
  - title: Structs, enums, and matching
    details: Model your domain with product and sum types, then take them apart with exhaustive pattern matching.
  - title: No null, no exceptions
    details: Absence is Option, failure is Result — both ambient, both checked, so callers can't forget the edge cases.
---

::: code-group

```nymph [hello_world.nym]
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

```nymph [lists.nym]
import std/io

let nums = #[1, 2, 3]
nums
  .filter($ % 2 == 1)
  .map($ ** 2)
  .fold(0, $0 + $1)
  |> io.println // 10
```

:::
