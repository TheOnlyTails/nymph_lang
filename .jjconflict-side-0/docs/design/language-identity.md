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
  public p: int,
  internal a: int,
  private b: int,
)
```

Outside the package:

- `Foo` can be named, stored, returned, compared, and cloned opaquely.
- `Foo(...foo)` is valid.
- `Foo(...)` is a valid shape-only pattern.
- `p` can be accessed, replaced, and bound; neither hidden field can be.
- Fresh construction is impossible because not all required fields are available.

Inside the package but outside the declaring module:

- `foo.a` is valid; `foo.b` is not.
- `Foo(...foo, a = value)` is valid.
- `Foo(...foo, b = value)` is not.
- `Foo(a = ..., b = ...)` remains unavailable because `b` is private.
- Pattern matching may bind `a` but not `b`.

Inside the declaring module, all fields are available.

Here, a package is one exact resolved package instance in the compiler's dependency graph. Dependency
aliases that resolve to the same graph node share package membership; separately resolved copies do
not, even when their names and versions match. `internal` availability compares this package identity,
while `private` availability additionally requires the declaring module. Compiler module interfaces
retain complete hidden field structure and ownership metadata; contextual checking, rather than
interface erasure, controls which fields a source location may use.

General rules:

1. Omitted field visibility means `internal`.
2. Struct construction and patterns use named fields only.
3. Fresh construction requires every field to be available and every available non-defaulted field to
   be supplied.
4. An available defaulted field may be omitted, but hidden defaults do not bypass construction
   restrictions.
5. Defaults have declaring-module lexical scope, cannot implicitly refer to sibling fields, and run in
   declaration order after supplied expressions.
6. Whole-spread cloning requires only visibility of the nominal type.
7. Spread update requires availability of each explicitly replaced field.
8. Field access and pattern binding require availability of that field.
9. Hidden fields may be copied opaquely through `...value`.
10. A struct pattern requires a trailing `...` whenever it omits any field from the complete shape.

Struct updates use one exact-type source spread first, with named explicit replacements winning:

```nymph
let user = User(
  ...user,
  name = "Mira",
  active = true,
)
```

At most one source spread is accepted in a struct construction. The source and every supplied or
replacement expression evaluate exactly once from left to right. Clone/update copies complete storage,
does not evaluate defaults, and leaves the source value unchanged.

## Privacy and debugging

Equality and hashing always use complete structure and are context-independent, even when this
reveals whether hidden states are equal. Pattern matching and field access remain
visibility-sensitive.

Compiler observation is deliberately visibility-insensitive:

```nymph
echo credential
```

`echo` recursively renders complete ordinary Nymph structure, including private and internal fields.
Field visibility controls source access, not secrecy from development output. It never dispatches
`Debug`, invokes host hooks or getters, or structurally traverses functions, managed resources, or
opaque external references; those render as inert type-tagged placeholders. An explicit `Debug`
implementation controls ordinary public `.debug()` behavior only.

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
- Development writes atomic stderr lines as `basename:line:column: value`, with OSC 8 source links
  when stderr is a terminal and a source URI is available.
- Release builds emit only the operand and strip the observer, source site, and URI metadata.
- Release builds apply a configurable `echo-in-release` lint with `Allow`, `Warn`, and `Deny` levels.
- The operand still evaluates exactly once, and its own effects remain.
- Intentional output uses `println` or telemetry and carries real effects.

Build profile and lint level are compiler-owned incremental inputs. CLI `--release`, manifest
`[lints]`, and LSP settings select the same compiler policy. Release erasure applies universally, while
warnings apply only to the selected root or workspace package by exact `PackageId`; dependencies own
their warning policy when compiled as roots.

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
2. A single-variant error or enum error propagates directly when its accepted variant set is a subset
   of the expected error enum's set. This changes only the static enum view; it does not convert the
   error value.
3. Otherwise the compiler searches for one unique pure, infallible explicit
   `Into<ExpectedError>` conversion.
4. Missing or ambiguous conversions are compile errors.
5. `Option` and `Result` families never convert implicitly between one another.
6. Error conversion cannot silently add effects.

Embedding does not generate `Into` implementations. An explicit `Into` may coexist with embedding,
but direct assignability takes precedence during `?`; `.to<T>()` continues to use the explicit
implementation. Propagation rebuilds the destination `Result.Error` wrapper around the unchanged
error value when its outer `Result` type differs. Panics are defects, not `Result` errors and not
declared effects.

## Enum variant embedding and static views

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

Every enum denotes a nominal static view and a deduplicated set of accepted single-variant types.
Every variant is itself an ordinary, source-nameable type, including in parameters, results, generic
arguments, and error types:

```nymph
func only_f(value: Bor.F): Result<void, Bor.F> = todo
```

Embedding changes assignability, not the underlying value. A whole-enum embedding accepts every
variant in the source enum's set. A selected embedding accepts only that qualified single-variant
type. Assignment, argument passing, returning, or `as` may change the static view when the source's
set is a subset of the destination's set:

```nymph
let foo = Foo.A
let bar: Bar = foo
consume_bar(foo)
let other = foo as Bar
```

The annotation, parameter, return type, or cast supplies the destination view. No `Bar(Foo.A)`
wrapper is constructed, and that construction form is invalid. A general enum cannot be viewed as a
destination assembled from selected variants unless every possible source variant is accepted. A
value already refined to a selected single-variant type can be viewed as that destination.

Methods dispatch through the static view. Viewing `foo` as `Bar` selects Bar's implementation; a
qualified pattern rebinds the same value through its source enum view and therefore selects source
methods:

```nymph
bar.calc() // Bar.calc

match (bar) {
  foo = Foo.A -> foo.calc(), // Foo.calc
  _ -> bar.calc(),           // Bar.calc
}
```

Patterns always use original qualified variants. Source-enum spread patterns such as `...Foo` are
invalid. Exhaustiveness operates on the destination's final deduplicated set:

```nymph
match (bar) {
  foo_a = Foo.A -> handle_a(foo_a),
  foo_b = Foo.B -> handle_b(foo_b),
  bor_f = Bor.F -> handle_f(bor_f),
  Bar.C -> handle_c(),
  Bar.D -> handle_d(),
}
```

Selected variants retain exactly the fields from their source declaration and cannot add or
redeclare fields. They must be source-qualified; no ambiguous bare variant is created. A successful
qualified pattern proves the single-variant type. A wildcard or otherwise unrefined binder retains
the scrutinee's current enum view, even when the runtime value originated in an embedded source.

An enum's accepted set is the least fixed point of its native variants and embedding declarations.
Self-embedding, mutually recursive embedding, diamonds, and repeated direct or transitive paths are
legal: adding the same single-variant type more than once is a no-op. Cross-module fixed points are
resolved with the language's cyclic-module analysis rather than rejected as enum cycles.

Generic arguments participate only when they affect a variant's fields. For example,
`Option<int>.Some` and `Option<string>.Some` are different single-variant types and runtime identities,
while `Option<int>.None` and `Option<string>.None` are the same. A contextual refinement may retain an
enclosing `Option<int>` view for method inference; without such context, a method that needs an erased
generic argument requires an annotation or explicit view.

Equality is available between equalable enum types whose accepted sets overlap. It compares the
original stable variant identity and fields, not the current static view:

```nymph
let bar: Bar = Foo.A
Foo.A == bar
```

Different original variant identities compare false, including variants accepted by only one side.
Hashing uses the same stable variant identity and fields, so every static view hashes identically.
Runtime identity reifies only generic arguments used by the variant.

## Iteration without mutation

Iterators are persistent values whose step returns a nominal result and successor state. The successor
keeps the receiver's full static iterator capabilities:

```nymph
enum Iteration<Item, Next> {
  Done,
  Yield(item: Item, next: Next),
}

interface Iterator<Item + !E> {
  func next(): Iteration<Item, self> + !E
}

interface ExactSizeIterator<Item + !E>: Iterator<Item + !E> {
  func remaining(): uint
}
```

Conceptually:

```nymph
let Iteration.Yield(item = first, next = iterator) = iterator.next()
let Iteration.Yield(item = second, next = iterator) = iterator.next()
```

`remaining()` is the exact number of future `Yield` steps. Future iterator capability interfaces may
use the same `self`-preserving contract. Adapters retain `ExactSizeIterator` only when the exact
remainder is derivable: `map`, `enumerate`, and `sorted_by` preserve it; `take` and `drop` preserve it
for exact-size sources; `zip` takes the minimum; `chain` uses the checked sum; and `filter` and
`flat_map` lose it.

Compiler-generated loops may mutate private runtime state as an optimization, but observable iterator
values remain immutable. The compiler may also fuse a pipeline ending in a terminal when doing so
preserves effects, order, replay, early exits, and diagnostics. There is no separate public traversal
abstraction.

Iterator methods are directly chainable from iterable values:

```nymph
items
  .map(transform)
  .filter(predicate)
  .take(5)
  .to<#[_]>()
```

`items.map(...)` is conceptually `items.iter().map(...)`.

`Iterable<Item + !E>.iter()` is pure and returns an iterator carrying `!E`. Creating an iterator or
lazy adapter is pure. Predictably ordered callbacks may carry effects, which join the source's latent
row and occur only when consumed:

```nymph
let traced: Iterator<int + !Io> =
  items.map((item) -> {
    println("${item}")
    item * 2
  })
```

`map`, `filter`, `flat_map`, `fold`, `for_each`, and short-circuiting terminals invoke callbacks
sequentially in source order. Laziness and short-circuiting determine how many invocations occur.
Callbacks whose schedule depends on an algorithm, including sorting comparators, must be pure.
`sorted_by` remains lazy: its first `next()` drains and sorts the source before yielding.

Repeated consumption repeats declared effects. Pure iterators are deterministically replayable;
impure iterators make no same-input/same-output promise. Evaluation remains sequential in source
traversal order unless a separate API explicitly promises concurrency.

Generic conversion remains canonical, with clear aliases for standard collections:

```nymph
iterator.to<#[int]>()
pairs.to<#{string: int}>()
items.to<Set<int>>()

iterator.to_list()
pairs.to_map()
items.to_set()
```

Future standard collection types receive corresponding aliases. Duplicate map keys retain the last
value in traversal order. Collection terminals may build through runtime-private transients but freeze
the result before returning it.

`for` is a dedicated backend-neutral HIR operation rather than an early source desugaring. It records
iterator dispatch, latent effects, persistent successor state, pattern and control targets, and source
spans. The iterable expression and its pure `iter()` call each evaluate once. Every iteration calls
`next()` once and saves the successor before entering the body. `continue` resumes from that successor;
`break`, return, `?`, panic, and cancellation abandon it without another step. Iteration-local cleanup
runs on every departure through the shared activation unwind. A tail transfer first cleans every
departing scope. Loops add no implicit cancellation checkpoint.

A valued `break` from `for` produces `Option<T>` because normal exhaustion produces `None`. A bare
break produces `void`, and mixing bare and valued breaks is an error.

External file and network streams are managed resource types, not persistent pure iterators.
Accumulation uses `fold`, `for`, or functional state loops rather than mutable locals.

## Functional state loops

General mutation-oriented `while` loops are removed. A functional state loop declares immutable
loop-carried bindings and advances them through named `continue` values:

```nymph
loop (
  let index: uint = 0
  let use file = File.open(path)?
) {
  if (done(index)) {
    break result
  }

  continue(index = index + 1)
}
```

Header declarations evaluate once from left to right. Each iteration receives fresh immutable
bindings; closures retain the iteration they captured. A `continue` evaluates supplied values from
left to right against the old bindings and installs them together. Omitted values remain unchanged.
Body fallthrough is equivalent to continuing with every value unchanged. Named values are accepted
only by state-loop `continue`; unknown, duplicate, or incompatible replacements are errors. Labels
permit `continue@outer(...)` in nested loops.

An unchanged header `let use` resource remains live. Replacing one first evaluates and acquires every
named replacement, then closes body-local resources, closes replaced header resources in reverse
declaration order, installs the new bindings, and starts the next iteration. If evaluation or cleanup
exits or defects, the next iteration does not start and newly acquired managed values are also cleaned
up. Loop exit closes the currently managed resources normally.

The activation machine implements state-loop continuation without stack growth. Return, `?`, panic,
cancellation, and proper tail calls use its ordinary cleanup path. A state loop cannot exhaust, so a
valued `break` produces `T`; a bare break produces `void`.

## Proper tail calls

Nymph guarantees proper tail calls as language semantics, including:

- Direct self-recursion
- Mutual recursion
- Generic calls
- Higher-order and dynamic calls
- Tail calls in branch and match tails

The JavaScript backend implements calls through generated defunctionalized activations driven by one
runtime continuation machine. HIR distinguishes ordinary calls, tail calls, suspension, returns, and
lexical cleanup regions. Direct, mutual, generic, and dynamic calls all use the same generated-callable
activation ABI; hidden generic runtime type objects remain ordinary calling-convention arguments.

A tail call replaces the current activation instead of pushing a logical frame. If lexical cleanup is
pending, the runtime closes the departing activation's scopes first and then performs the replacement,
so cleanup does not invalidate tail position. A cleanup defect prevents the destination call and
defects the execution after every registered close has been attempted. Non-tail calls retain an
explicit caller activation, and suspension retains an activation and resume state rather than a native
JavaScript async call chain.

Generated basic blocks may be optimized into direct JavaScript when proven safe, but every optimized
form must preserve the activation protocol. External host calls remain explicit HIR operations and do
not acquire the generated-callable ABI.

## Numeric safety

`int` and `uint` are exact fixed-width 64-bit values. Default integer arithmetic is checked in all
builds.

On the JavaScript backend, both types use native `bigint` payloads inside their uniform boxes:
`NInt.v` is an in-range signed 64-bit value and `NUint.v` is an in-range unsigned 64-bit value.
Integer literals, semantic constants, HIR, and constant folding retain exact signed or unsigned
values; they never pass through `f64` or JavaScript `number`.

Compiler policy:

1. Proven overflow, division by zero, or invalid shifts are compile errors.
2. Proven-safe operations omit runtime checks.
3. Uncertain operations retain runtime checks and panic on failure.

Direct BigInt operators implement proven-safe operations. Checked runtime helpers enforce uncertain
operations. `BigInt.asIntN` and `BigInt.asUintN` are reserved for explicit wrapping operations rather
than default arithmetic.

Range analysis should include literals and constants, branch comparisons, min/max constraints, known
collection lengths, range-loop bounds, and checked-operation refinements.

Checked, saturating, and wrapping arithmetic families are explicit.

The current unconditional `uint -> int` widening is removed because not every unsigned 64-bit value
fits in signed 64 bits. Integer conversion is implicit only when range analysis proves it safe.
Otherwise, checked, trapping, or wrapping conversion is explicit. The same applies to `int -> uint`.

A proven-safe `int`/`uint` conversion may rebox the unchanged BigInt payload. Conversion to a float or
host index crosses through JavaScript `Number` only under the applicable explicit conversion or range
proof. The external JavaScript ABI for an integer is an in-range `bigint`; marshalling unboxes and
reboxes it without silently accepting `number`, consistent with trusted FFI declarations.

Integer equality, ordering, display, and hashing consume the exact BigInt value. Equal nonnegative
`int` and `uint` values must hash equally; the structural hash algorithm is specified with the broader
equality and collection representation.

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

Any nominal type may implement the effect-parameterized synchronous cleanup interface:

```nymph
interface Close<!E> {
  func close(): void + !E
}
```

Semantics:

- A value is a managed resource when its static type satisfies `Close<!E>`; `let use` accepts exactly
  those values, including generic values with a `Close<!E>` bound.
- Management is non-transitive. A struct, enum, or collection containing a managed resource must
  implement `Close` explicitly to define its own cleanup behavior.
- Register synchronous cleanup at lexical scope exit.
- Include `!E` in the enclosing computation's effects because cleanup performs it on scope exit.
- Close on normal completion, `?`, return, panic, and cancellation.
- Close in reverse declaration order.
- `close(): void + !E` is synchronous, non-fallible, and idempotent.
- Fallible or suspending finalization is explicit through operations such as
  `finish(): Result<...>`.

Creating a resource without `let use` remains legal. Conservative warnings identify obvious unmanaged
cases and suggest `let use`; they do not claim ownership tracking. A nominal type with a direct field
whose static type satisfies `Close`, including a generic field with a `Close` bound, also warns when
the containing type does not implement `Close`. This check does not recursively inspect containers,
and intentional non-ownership may suppress it. Manual cleanup and escaping resources remain allowed.

A managed value may escape as an alias, be registered by multiple `let use` bindings, or be closed
manually. Every registration still invokes `close` once at its lexical exit, relying on the
implementation's idempotency. Implementations own alias-shared closed state and expose post-close
failures through their declared expected-error types; the compiler does not impose a universal error
type or lifecycle wrapper. The resource itself is not leaked.

The compiler enforces the `Close` signature, effect propagation, cleanup registration and ordering,
and the direct-field warning. Idempotency, alias-shared state, and safe post-close behavior are semantic
implementation obligations. External implementations remain trusted FFI promises.

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
- Generated recipes receive an explicit hidden execution frame carrying structured context,
  cancellation lineage, and `AbortSignal`; Node ambient state is not language semantics.
- Structured task ownership and execution cancellation lineage are separate runtime relationships.
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
losers, joins them after cleanup, and returns the winner result only when every loser cleans up without
defecting. A losing execution that defects during cancellation or cleanup defects the `race` execution
rather than being discarded. First settlement may be success, cancellation, or defect.

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
