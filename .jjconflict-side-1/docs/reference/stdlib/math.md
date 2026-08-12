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

## Complex numbers and powers

`import std/math/complex with (Complex)` provides the canonical
`Complex.new(real: float, imaginary: float)` constructs a complex number. It supports addition,
subtraction, multiplication, division, negation, conjugation, magnitude, and
the exact scalar exponentiation matrix documented under
[Operators](../operators#exponentiation). The same import supplies the real
base/`float` exponent rows because those rows return `Complex`.

Integer and integral-valued float powers use exponentiation by squaring.
Non-integral powers use a positive-real fast path where valid and otherwise
the principal branch

\[
z^x = \exp\!\left(x\operatorname{Log}(z)\right),
\]

where `Log` uses `ln(abs(z))` and `atan2(imaginary, real)`. Thus, for example,
`(-4) ** 0.5` has a positive imaginary component on the principal branch.
`0 ** 0` is one, zero to a positive exponent is zero, and zero to a negative
exponent raises a runtime domain error.
