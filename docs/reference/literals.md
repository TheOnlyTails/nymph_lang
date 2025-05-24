# Literals

Literals are language constructs that are "built-in", with special syntax to create and manipulate them.
They should be either very _basic_ primitives or very _useful_ structures.

## Numbers

Number literals come in 2 kinds: integers and floats. Integers are whole, signed 64-bit numbers,
and floats are IEEE 754 double-precision floating point numbers.

Integers are sequences of digits, optionally separated by underscores,
in either binary, octal, decimal, or hexadecimal.

```nym
12345 // decimal
1_000_000_000 // digit separators
0b10101101 // binary
0o7654321 // octal
0xDEADF00D // hexadecimal
```

Floats are only decimal, and they may include underscore digit separators, scientific-notation exponents.
Decimal integer literals suffixed with `f` are treated as floats of the same value.

```nym
1.0 // regular float
0.24e10 // exponent
9e-1 // dotless float with exponent
102.000_000_0001 // digit separators
1f // integer float
```

Note that digit separators for both integers and floats may _only_ appear between digits,
not other parts such as the `f` float prefix, `0b` radix specifier, etc:

```nym
0b_0110 // ❌
10_f // ❌
1._2 // ❌
```

## Booleans

Boolean literals are `true` and `false`, which are the only two values of the `boolean` type,
represented by their respective case-sensitive keywords.

```nym
true
false
```

## Characters

Character literals are single characters, enclosed in single quotes.
They are a single UTF-8 codepoint, and may be entered either directly or using escape sequences.

The only available escape sequences are newline (`\n`), tab (`\t`), carriage return (`\r`),
apostrophe (`\'`), backslash (`\\`), and the unicode escape (`\uXXXX`).
Unicode escape sequences always use 4-6 hexadecimal digits, and must represent a valid unicode codepoint.

```nym
'a' // regular character
'\n' // newline
'\t' // tab
'\r' // carriage return
'\'' // single quote
'\\' // backslash
'\u1234' // unicode escape (4-6 hex digits)
```

## Strings

String literals are sequences of characters, enclosed in double quotes.
They are UTF-8 encoded, and may include unescaped newlines.

> [!NOTE] Escape Sequences
> While they may appear similar, string escape sequences are different from character escape sequences.
> Character literals may include escape apostrophes `'\''`, while string literals may not.
> On the flip side, string literals must escape double quotes `"\""`, and may include escaped
> interpolated expressions `\${`.

```nym
"Hello, world!" // regular string
"Hello, \"world!\"" // escaped double quotes
"Hello, 
world!" // newline
"Hello, \nworld!" // escaped newline
"Hello, ${name}!" // interpolated expression
"Hello, \${name}!" // escaped interpolated expression
```

## Identifiers

Identifiers are names for variables, functions, types, and other constructs.
They are case-sensitive, and may include letters (including some unicode characters), digits, and underscores.
They must start with a letter or underscore, and may not be a reserved keyword.

The single underscore `_` is a special identifier,
which is used to indicate that a variable is intentionally unused and to suppress compiler warnings.
Any declaration or assignment to `_` effectively discards the value.

> [!NOTE] Casing and Conventions
> Nymph does not enforce any particular casing or naming convention for identifiers.
> However, it is recommended to use `snake_case` for variables and functions,
> and `PascalCase` for types, structs, enums, etc.

```nym
myVariable
_myVariable
MY_VARIABLE
my_variable
myVariable123
שלום_עולם
αβγδε
ابتثج
```

## Lists

Lists are ordered collections of values, enclosed in square brackets, preceded with the collection sigil `#`.
All items in a list must be of the same type, and may be any valid expression.

They may include any number of items, including zero, as well as an optional trailing comma,
and span multiple lines.

Other [Iterators](./stdlib/iter#Iterator) may be spread into a list using the `...` operator,
so long as the `Item` type of the iterator matches the list type.

```nym
#["apple", "banana", "cherry"]
#[a, b, ...c]
#[]
#[
  1,
  2,
  3,
  4,
  5,
]
```

## Tuples

Similar to lists, tuples are ordered collections of values, enclosed in parentheses, preceded with the collection sigil `#`.
Unlike lists, tuples may contain values of different types, and are fixed in size.

They may include any number of items, including zero, as well as an optional trailing comma,
and span multiple lines.

Other tuples may be spread into a tuple using the `...` operator,
so long as their structure matches the structure of the parent tuple.

```nym
#(1, true, 'a')
#(
  1,
  2,
  3,
)
#(1, ...a, 'c') // a : #(int, boolean) -> #(int, int, boolean, char)
```

## Maps

Maps are unordered collections of key-value pairs, enclosed in curly braces, preceded with the collection sigil `#`.
All keys in a map must be of the same type, and all values must be of the same type.

Keys and values may be any valid expression, and the key-value pairs are separated by commas.
They may include any number of items, including zero, as well as an optional trailing comma,
and span multiple lines.

Other [Iterators](./stdlib/iter#Iterator) may be spread into a map using the `...` operator,
so long as their `Item` type is a tuple containing the key and value types of the map.

```nym
#{"apple": 1, "banana": 2, "cherry": 3}
#{}
#{
  "apple": 1,
  "banana": 2,
  "cherry": 3,
  ...a,
} // a : #[#(string, int)]
```

## Ranges

Ranges are not a literal type, but rather a special syntax for creating iterators.
A range can be either exclusive (`..`) or inclusive (`..=`).
Inclusive ranges may omit either bound, but exclusive ranges must include an upper bound.

Ranges can be created for any type that implements the [`Range`](./stdlib/cmp-comparison#Comparable) interface, which provides a way to order values.

Ranges which do not contain a lower bound are "max-only" ranges, and they cannot be iterated upon,
only used to check for inclusion (using the [`in`](./expressions#Inclusion) operator).
Ranges which do not contain an upper bound are "min-only" ranges, and they can be iterated upon,
but are infinite in size and will never terminate.

```nym
1..10 // exclusive range
1..=10 // inclusive range
1.. // min-only exclusive range
..10 // max-only exclusive range
..=10 // max-only inclusive range
```

## This

The `this` keyword is a special identifier that refers to the current instance of an interface, struct, or enum.
It is used to access instance variables and methods within the context of the surrounding scope.

It may not be used outside of an instance context, such as in namespaces or in the global scope.
