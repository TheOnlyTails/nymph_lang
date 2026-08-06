# `@/math`: numeric functions and constants

The math module is ambient: its methods and constants are available without an
`import`. Floating-point operations use IEEE-754 double-precision semantics,
including JavaScript's `NaN` and positive or negative infinity results for
out-of-domain and overflow cases.

## Float methods

`float` provides the trigonometric methods `sin`, `cos`, `tan`, `asin`, `acos`,
and `atan`; the hyperbolic methods `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, and
`atanh`; and `floor`, `ceil`, and `round`. `exp()` computes \(e^x\), while
`ln()` computes the natural logarithm. `log(base)` is defined in Nymph as:

```nymph
func log(base: float): float = this.ln() / base.ln()
```

`atan2(y, x)` is a top-level function whose operands are interpreted in that
order. Integer trigonometric and hyperbolic convenience methods convert their
receiver to `float` and return a `float`.

The host implementations receive canonical boxed Nymph values and return
canonical `float` or `int` boxes. Raw JavaScript numbers exist only while a
host math primitive is being called; source operands are evaluated once in
source order.

## Constants

- `pi`, `tau`, `e`, and `phi` are the usual mathematical constants.
- `max_float` is the largest finite positive float (`Number.MAX_VALUE`).
- `min_float` is the most negative finite float (`-Number.MAX_VALUE`).
- `min_positive_float` is the smallest positive representable float
  (`Number.MIN_VALUE`, including subnormal values).
- `max_int` and `min_int` are the language's nominal signed integer bounds.

The external float constants are immutable boxed values initialized once per
generated program and shared by every reference.
