---
# https://vitepress.dev/reference/default-theme-home-page
layout: home

hero:
  name: "Nymph"
  text: "Programming Language"
  tagline: "A simple language that gets out of your way."
  actions:
    - theme: brand
      text: Markdown Examples
      link: /markdown-examples
    - theme: alt
      text: API Examples
      link: /api-examples

features:
  - title: Feature A
    details: Lorem ipsum dolor sit amet, consectetur adipiscing elit
  - title: Feature B
    details: Lorem ipsum dolor sit amet, consectetur adipiscing elit
  - title: Feature C
    details: Lorem ipsum dolor sit amet, consectetur adipiscing elit
---

::: code-group

```nym [hello_world.nym]
func main() -> {
  println("Hello world!")
}
```

```nym [functions.nym]
func factorial(n: int) -> match (n) {
  ..=1 -> 1
  _ -> n * factorial(n - 1)
}
```

```nym [types.nym]
enum BinaryTree<T> {
  Leaf { value: T },
  Node { left: BinaryTree<T>, right: BinaryTree<T> },
}
```

```nym [lists.nym]
let nums = #[1, 2, 3]

nums
  .filter((x) -> x % 2 == 1)
  .map((x) -> x ** 2)
  .fold(0, (x, y) -> x + y)
  |> println(_) // 10
```

:::