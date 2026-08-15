# Draft language identity

This document records the current direction for Nymph's type system and language semantics. It is
design documentation, not the implemented language reference. Where it conflicts with older draft
designs, this document takes precedence.

## Identity

Nymph is a language of **functional immutable values with checked effects**. It does not adopt
Rust-style ownership for ordinary data. Its center is approachable syntax, persistent values, strong
static checking, explicit expected errors, checked effects, structured concurrency, and narrowly
managed resources.

The safety boundary is deliberate:

- Ordinary Nymph values are immutable and structurally comparable where lawful.
- Shared mutation, ownership, and borrowing do not exist for ordinary values.
- Host resources and JavaScript objects are opaque external references. Their changing state is
  represented through effects and runtime behavior rather than a general borrow checker.
- FFI declarations are trusted ABI promises.

## Immutability and bindings

All ordinary values are immutable. Bindings cannot be reassigned; state evolution uses shadowing:

```nymph
let users = #[alice, bob]
let users = users.appended(charlie)

let account = Account(balance = 10)
let account = Account(...account, balance = 20)
```

Old values remain valid and unchanged. Runtime implementations may use mutation or structural sharing
internally, but this cannot be observed through Nymph semantics.

Closures capture the specific immutable binding visible where they are created. A later shadowing
declaration creates a distinct binding.

There is no `mut T`, `let mut`, or `mut func` for ordinary Nymph data.

## Structural equality and hashing

Structs and collections use structural equality:

- Structs are equal when all fields are equal.
- Lists are ordered.
- Maps are unordered and compare entries structurally.
- Hidden fields participate in equality and hashing.
- Functions do not support equality.
- Opaque resources and external types support equality only through an explicit implementation.

Equality capabilities are static. Types containing non-equalable fields do not automatically support
equality.

IEEE floating-point equality is partial: NaN is not reflexive. Partial equality must remain distinct
from lawful reflexive equality and hashability. Floats are not lawful hash keys by default. Hashing
must always agree with equality.

## Field visibility, construction, cloning, and matching

Constructor visibility is derived entirely from field visibility; there is no separate constructor
modifier.

```nymph
public struct Foo(
  internal a: int,
  private b: int,
)
```

Outside the package:

- `Foo` can be named, stored, returned, compared, and cloned opaquely.
- `Foo(...foo)` is valid.
- `Foo(...)` is a valid shape-only pattern.
- Neither field can be accessed, replaced, or bound.
- Fresh construction is impossible because not all required fields are available.

Inside the package but outside the declaring module:

- `foo.a` is valid; `foo.b` is not.
- `Foo(...foo, a = value)` is valid.
- `Foo(...foo, b = value)` is not.
- `Foo(a = ..., b = ...)` remains unavailable because `b` is private.
- Pattern matching may bind `a` but not `b`.

Inside the declaring module, all fields are available.

General rules:

1. Fresh construction requires every required field to be available.
2. Whole-spread cloning requires only visibility of the nominal type.
3. Spread update requires availability of each explicitly replaced field.
4. Field access and pattern binding require availability of that field.
5. Hidden fields may be copied opaquely through `...value`.
6. Hidden defaults do not bypass construction restrictions.

Struct updates use one source spread, with explicit replacements winning:

```nymph
let user = User(
  ...user,
  name = "Mira",
  active = true,
)
```

At most one source spread is accepted in a struct construction.

## Privacy and debugging

Equality and hashing always use complete structure and are context-independent, even when this
reveals whether hidden states are equal. Pattern matching and field access remain
visibility-sensitive.

Compiler debugging is also visibility-sensitive:

```nymph
echo credential
```

Only fields visible at the source location are rendered. An explicit `Debug` implementation controls
its own public representation.

Serialization is never derived merely from field visibility. It requires an explicit conversion or
interface.

## `echo`: non-semantic debugging

`echo` is a compiler expression:

```nymph
echo value
```

It prints a source-aware debug representation and returns its operand unchanged, so it is available
in pipelines:

```nymph
input
  |> parse
  |> echo
  |> normalize
  |> echo
```

Conceptually:

```text
type(echo expression) = type(expression)
effects(echo expression) = effects(expression)
```

`echo` itself does not add `!Io`. Its output is outside observable program semantics:

- Programs cannot consume it.
- Concurrent order is unspecified, though lines remain atomic.
- Release builds omit the observation.
- Release builds emit a configurable warning when `echo` remains.
- The operand still evaluates exactly once, and its own effects remain.
- Intentional output uses `println` or telemetry and carries real effects.

## Effect syntax and inference

Effects are nominal, statically tracked labels:

```nymph
effect Database
effect Network
effect Io
```

Syntax:

```text
!()                         pure effect
!Database                   one effect
!Database + !Network        effect composition
!E                          generic effect parameter
!_                          infer effects
```

Every callable result conceptually has a value component and an effect component:

```text
T               == T + !()
!Database       == void + !Database
!()             == void + !()
```

Effects compose as an idempotent, commutative set.

```nymph
func parse(text: string): Config
func query(sql: string): Result<Rows, DbError> + !Database
func log(message: string): !Telemetry
```

Omitted effects on an explicitly written return type mean purity. Effect inference is requested
explicitly:

```nymph
func load(path: Path): Result<string, IoError> + !_ = todo
```

A fully omitted return type infers both its value and effects:

```nymph
func load(path: Path) = File.read_text(path)
```

Effect parameters use generic syntax:

```nymph
func apply<T, U, !E>(
  value: T,
  operation: (T) -> U + !E,
): U + !E = operation(value)
```

Known effects may be combined with an inferred remainder:

```nymph
func synchronize(): Result<void, Error> + !Database + !_ = todo
```

`!_` is valid only where a body, initializer, or context can infer a concrete row. It is invalid in
bodyless interface contracts and unresolved exported aliases.

Effects enter the call graph through explicit function signatures and trusted external or intrinsic
declarations. They are static labels, not runtime algebraic operations or handlers. A closed
annotation is an upper bound on body effects; unlisted effects are rejected. Explicit effects may
conservatively over-approximate the current body.

Effect changes are API changes. Effects support purity enforcement, deterministic callbacks,
architectural boundaries, task auditing, and restricted execution environments. They are not a
security boundary against dishonest FFI declarations.

## Effects in interfaces

Interface methods declare effect upper bounds. Implementations may be narrower but not broader:

```nymph
interface Store<Item, !LoadEffects> {
  func load(id: Id): Result<Item, StoreError> + !LoadEffects
}
```

Concrete calls use the concrete implementation's narrower effects. Calls through generic or
interface bounds use the interface effect contract.

Interfaces that permit arbitrary effects expose an effect generic. Protocols such as lawful equality,
hashing, and ordinary debug formatting remain pure by omission.

## Explicit generic arguments

Generic arguments are available at calls. All, some, or none may be supplied.

By position:

```nymph
convert<int, string, Strict>(value)
convert<_, string, _>(value)
```

By name:

```nymph
convert<Target = string>(value)
convert<Mode = Strict, Target = string>(value)
```

A positional prefix may be followed by named arguments. Positional arguments cannot follow named
ones. Parameters cannot be supplied twice. `_` requests inference. Effect parameters may also be
named explicitly.

## `Into` and `.to<T>()`

One generic conversion surface replaces collection-specific conversion methods:

```nymph
iterator.to<#[_]>()
pairs.to<#{_: _}>()
items.to<Set<_>>()
```

Expected-type inference permits:

```nymph
let values: #[int] = iterator.to()
```

`Into` conversions may carry effects when conversion drives an effectful computation, such as
collecting an effectful iterator. Fallibility remains explicit in the value type with `Result`.

Numeric conversions are distinct:

```text
.to<T>()          infallible semantic conversion
.checked_to<T>()  Option-based checked conversion
.try_to<T>()      Result-based diagnosed conversion
as T              explicit checked cast that may panic
.wrapping_to<T>() explicit modular conversion
```

## Error handling and `?`

Expected failure remains `Option` or `Result`; Nymph has no exceptions.

`?` behaves as follows:

1. Exact error types propagate directly.
2. Otherwise the compiler searches for one unique pure, infallible `Into<ExpectedError>` conversion.
3. Missing or ambiguous conversions are compile errors.
4. `Option` and `Result` families never convert implicitly between one another.
5. Error conversion cannot silently add effects.

This permits ergonomic application-error wrapping while keeping signatures explicit. Panics are
defects, not `Result` errors and not declared effects.

## Enum variant embedding and spreading

Enums may embed all variants of another enum or selected qualified variants:

```nymph
enum Bor {
  E,
  F,

  func calc() = todo
}

enum Foo {
  A,
  B,
}

enum Bar {
  ...Foo,
  Bor.F,
  C,
  D,

  func calc() = todo
}
```

Construction preserves the nominal wrapper:

```nymph
let a = Bar(Foo.A)
let f = Bar(Bor.F)
```

The outer value dispatches outer methods:

```nymph
a.calc() // Bar.calc
```

Matching extracts the source value, which dispatches source methods:

```nymph
match (f) {
  value = Bor.F -> value.calc(),
  _ -> {},
}
```

A source-enum spread pattern uses `...Foo`, not `Foo(...)`:

```nymph
match (bar) {
  foo = ...Foo -> handle(foo),
  f = Bor.F -> handle_f(f),
  Bar.C -> handle_c(),
  Bar.D -> handle_d(),
}
```

Selected variants retain exactly the fields from their source declaration and cannot add or
redeclare fields. They must be source-qualified; no ambiguous bare variant is created. Whole spread
accepts any source-enum value. A selected variant requires static refinement to that variant.

Embedding preserves immediate nominal layers for method dispatch and matching; it does not flatten
representation:

```text
Baz(Bar(Foo.A))
```

Equality and hashing are embedding-transparent:

```nymph
Foo.A == Bar(Foo.A) == Baz(Bar(Foo.A))
```

Hashing normalizes identically. Native unrelated variants remain nominally distinct.

The embedding graph is a DAG. Cycles are rejected. A destination may have only one normalized path to
each source variant; overlapping direct or transitive embeddings are compile errors. This provides one
canonical representation per normalized variant in each destination.

Generated pure `Into` conversions traverse arbitrary unique embedding depth and preserve every
nominal wrapper. A whole-enum conversion is synthesized whenever every source variant has exactly one
path into the destination, including coverage assembled from selected variants through intermediate
enums. This supports nested application-error enums and deep `?` propagation.

## Iteration without mutation

Pure iterators are persistent values whose step returns successor state:

```nymph
interface Iterator<Item + !E> {
  func next(): Option<#(Item, self)> + !E
}
```

Conceptually:

```nymph
let Some(#(first, iterator)) = iterator.next()
let Some(#(second, iterator)) = iterator.next()
```

Compiler-generated loops may mutate private runtime state as an optimization, but observable iterator
values remain immutable.

Iterator methods are directly chainable from iterable values:

```nymph
items
  .map(transform)
  .filter(predicate)
  .take(5)
  .to<#[_]>()
```

`items.map(...)` is conceptually `items.iter().map(...)`.

Effects are allowed in lazy `map` and `filter`. Their effects become latent iterator effects and occur
only when consumed:

```nymph
let traced: Iterator<int + !Io> =
  items.map((item) -> {
    println("${item}")
    item * 2
  })
```

Repeated consumption repeats declared effects. Pure iterators are deterministically replayable;
impure iterators make no same-input/same-output promise. Evaluation remains sequential in source
traversal order unless an API explicitly promises concurrency. Laziness determines how many callback
invocations occur.

External file and network streams are managed resource types, not persistent pure iterators.
Accumulation uses `fold` and shadowing rather than mutable locals. `for` remains useful for traversal,
effects, and early exits. General mutation-oriented `while` loops are removed.

## Proper tail calls

Nymph guarantees proper tail calls as language semantics, including:

- Direct self-recursion
- Mutual recursion
- Generic calls
- Higher-order and dynamic calls
- Tail calls in branch and match tails

The JavaScript backend uses loops or trampolines as needed. A call is in tail position only when no
work remains afterward; pending resource cleanup may require a separate cleanup continuation or make a
call non-tail.

## Numeric safety

`int` and `uint` are exact fixed-width 64-bit values. Default integer arithmetic is checked in all
builds.

Compiler policy:

1. Proven overflow, division by zero, or invalid shifts are compile errors.
2. Proven-safe operations omit runtime checks.
3. Uncertain operations retain runtime checks and panic on failure.

Range analysis should include literals and constants, branch comparisons, min/max constraints, known
collection lengths, range-loop bounds, and checked-operation refinements.

Checked, saturating, and wrapping arithmetic families are explicit.

The current unconditional `uint -> int` widening is removed because not every unsigned 64-bit value
fits in signed 64 bits. Integer conversion is implicit only when range analysis proves it safe.
Otherwise, checked, trapping, or wrapping conversion is explicit. The same applies to `int -> uint`.

Arithmetic panics are defects, not effects.

## Indexing and ranges

Single indexing returns `T` and panics when uncertain runtime bounds fail. `.get(index)` returns
`Option<T>`. Obvious invalid indices are compile errors; proven-valid indices omit checks. Negative
integer indices count from the end.

Range indexing is supported for homogeneous collections and strings:

```nymph
items[1..3]
items[1..=3]
items[..2]
items[2..]
items[-3..-1]
items.get(1..3)
```

Rules:

- An exclusive end may equal the collection length.
- An inclusive end must name an existing element.
- Reversed in-bounds ranges produce an empty result, matching Nymph range semantics.
- Out-of-bounds ranges panic through `[]` and return `None` through `.get`.
- List slices are immutable values and may structurally share storage.
- String indices use Unicode code-point offsets.

Tuple slicing is unsupported, including constant ranges. Tuple single indices remain statically known
because result types are heterogeneous. Variadic generics do not make dynamic tuple slicing typeable.

## Resource management

Managed immutable bindings use `let use`:

```nymph
let use file = File.open(path)?
```

Resources implement one synchronous cleanup interface:

```nymph
interface Close {
  func close(): void
}
```

Semantics:

- Register synchronous cleanup at lexical scope exit.
- Close on normal completion, `?`, return, panic, and cancellation.
- Close in reverse declaration order.
- `close(): void` is synchronous, non-fallible, and idempotent.
- Fallible or suspending finalization is explicit through operations such as
  `finish(): Result<...>`.

Creating a resource without `let use` remains legal. Conservative warnings identify obvious unmanaged
cases and suggest `let use`; they do not claim ownership tracking. Manual cleanup and escaping
resources remain allowed.

A managed handle may escape as an alias, but scope exit still closes the underlying resource. Later
operations fail safely with a closed-resource error. The resource itself is not leaked.

If a spawned child captures a `let use` resource whose lexical scope ends before the child's joining
context, emit a static warning with declaration, capture, close, and join spans. Suggest moving `let
use` to the enclosing async context or introducing an awaited nested async context. This is
warning-oriented lifetime analysis, not ownership enforcement.

Cleanup defects follow these rules:

- Always attempt every close in reverse order.
- A body panic remains primary; cleanup panics are suppressed and attached.
- Normal completion plus a cleanup panic defects the task.
- Cancellation plus a cleanup panic becomes a defect with cancellation context.
- Multiple cleanup defects are retained in close order.

## JavaScript interop

External declarations are trusted ABI promises. Nymph performs the ABI conversion implied by the
signature and calls the host function. It does not automatically:

- Catch JavaScript exceptions
- Convert `null` or `undefined` to `Option`
- Convert rejected promises to `Result`
- Validate returned shapes
- Detect undeclared effects or mutation
- Repair incorrect prototypes or generic arguments

The FFI author is responsible for correctness. A declared `Result` must be returned in Nymph ABI form;
exceptions are not converted automatically.

Opaque external types preserve live JavaScript identity and mutability:

```nymph
external type JsArray<T> {
  external func length(): uint + !Js
  external func get(index: uint): Option<T> + !Js
  external func push(value: T): !Js
}
```

Methods must be declared inside the external type or an impl block. Reads of externally mutable state
are effects too. External types receive no automatic structural equality, hashing, or serialization.
Explicit conversion can snapshot them into ordinary immutable Nymph collections.

## Async task model

The complete task design is maintained in [`async-model.md`](./async-model.md). Its defining choices
are:

- `async func ...: T + !E` returns a cold `Task<T + !E>`.
- `async {}` creates a nested structured task context.
- A task stores a reusable computation recipe and one memoized default execution.
- Direct `.await` drives or observes the default execution.
- Each explicit `.spawn()` creates a fresh independent execution handle.
- Application effects occur when a task is driven or spawned, not when a running handle is observed.
- Handle completion distinguishes a produced value from cancellation or a defect.
- Task contexts join all children before exposing their result.
- Cancellation is cooperative and cannot be suppressed inside the affected execution.

## Concurrency utilities

Low-level selection observes running handles without owning them:

```nymph
struct Selection<T>(
  index: uint,
  result: Result<T, HandleError>,
)

Handle.select(handles): Task<Selection<T>>
```

It observes the first terminal handle, preserves its input index, and neither cancels nor owns other
handles. If several inputs are already settled, it chooses the lowest input index deterministically.

`Task.race` owns its executions:

```nymph
Task.race(tasks): Task<Result<T, HandleError> + !E>
```

It spawns fresh independent executions, observes the first settlement, requests cancellation of
losers, joins them after cleanup, and returns the winner result. First settlement may be success,
cancellation, or defect.

`select` observes; `race` owns and cancels. Separate `first_ok` and `try_all` families interpret
application-level `Result` values rather than teaching generic task primitives that `Err` means
cancellation.

## Diagnostics standard

Type, effect, and lifetime diagnostics use Rust-style source diagrams:

- Primary span at the operation causing the problem.
- Secondary spans for declarations, inferred constraints, cleanup points, and async join points.
- Plain-language causal notes.
- Multiple alternative fixes where valid.
- Machine-applicable suggestions when unambiguous.

Resource warnings show declaration, child capture, lexical close, and actual task join boundaries
rather than only saying “resource may escape.”

## Safety and complexity tradeoff

Nymph spends compiler and runtime complexity on:

- Persistent immutable values
- Structural equality and hashing
- Strong inference
- Checked effects
- Proper tail calls
- Numeric and range analysis
- Structured task contexts
- Deterministic resource cleanup
- High-quality causal diagnostics

It avoids exposing:

- Borrow syntax
- Lifetimes for ordinary values
- Ownership and move tracking
- Mutable aliases for ordinary data
- Async-close protocols
- Exceptions
- Implicit effect permission

It accepts runtime responsibility for:

- Closed escaped resource handles
- External JavaScript mutation and identity
- Dishonest FFI declarations
- Cooperative cancellation responsiveness
- Host-resource concurrency semantics

This is a coherent identity: functional and immutable at the value layer, explicit and checked at the
effect and error layer, structured at the concurrency and resource layer, and pragmatic at the
JavaScript boundary.
