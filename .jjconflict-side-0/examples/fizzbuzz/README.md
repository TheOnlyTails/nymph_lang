# fizzbuzz

Print 1–100, replacing multiples of 3 with `Fizz`, of 5 with `Buzz`, and of both
with `FizzBuzz`.

The interesting part is the `match`: rather than a chain of `if`/`else if`, it
matches on a **tuple of both remainders** at once, so every rule is one arm and the
"both" case naturally comes first.

```nym
func fizzbuzz(n: int): string = match (#(n % 3, n % 5)) {
  #(0, 0) -> "FizzBuzz",
  #(0, _) -> "Fizz",
  #(_, 0) -> "Buzz",
  _ -> "${n}",
}
```

Also on display: fully-bounded ranges as a `for` source (`1..=100`) and string
interpolation (`"${n}"`).

**Status:** ✅ Runs today — no imports beyond `std/io`.
