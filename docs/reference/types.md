# Types

Types in Nymph are a way to describe the structure of a piece of data, allowing the programmer to
write code fast and correctly, passing work to the compiler.

In general, types built into Nymph always begin with either a symbol or a lowercase letter,
while user-defined types may be any valid identifier.
However, as mentioned in the [Identifiers](./literals#Identifiers) section, types created by users
should generally use `PascalCase`.

## Basic types

Basic types represent a very simple construct, which can be created using a [literal](./literals):

- `int`: a 64-bit signed integer.
- `float`: a double-precision floating point number.
- `boolean`: a value of either `true` or `false`.
- `char`: a single Unicode codepoint.
- `string`: a UTF-8 encoded list of Unicode codepoints.
- `void`: a type representing the absence of data. Equivalent to `#()`.
- `never`: a type representing the result of an operation that causes the program to panic.
- `self`: a reference to the current type being implemented or defined. Invalid outside declarations.
- Infer `_`: asks the compiler to attempt to fill in the missing type on its own.

## Container types

Container types represent data structures containing references to other types, and may also be created
using a [literal](./literals):

- List `#[T]`: an ordered list of items of type `T`.
- Tuple `#(A, B, C)`: an immutable list where the type of each item is defined in order inside the type.
- Map `#{K: V}`: an unordered hash-map where each key of type `K` is associated with an item of type `V`.

## Compound types

Compound types are reference to other types, usually imposing a kind of constraint on them:

- Reference `A<B>`: a reference to a user-defined type named `A`, with an optional list of generic
  type arguments (that may be labelled).
- Function `(A) -> B`: a list of ordered parameters surrounded by parentheses and a return type,
  representing any function matching the signature. Functions containing spread (`...`) parameters
  should use the list (`#[T]`) type for them.
- Intersection `A + B`: given two interfaces `A` and `B`, the intersection between them represents
  any type that implements both of them.
- Pattern `A is B`: given a type `A` and a pattern `B`, the pattern type represents only the values
  of `A` that also match `B`. The `!is` operator is used to represent only values that _don't_ match `B`.
