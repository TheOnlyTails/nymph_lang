# Issue 98: functional-iteration precedents

Status: planning research, 2026-08-18. This note compares primary-source precedents; it does not
choose Nymph's representation or change the governing design. Nymph's settled inputs are the issue
itself and `docs/design/language-identity.md`: observable iterators are persistent, `next` returns
successor state, lazy callback effects are latent and sequential, effects use canonical finite rows,
general `while` is removed, cleanup covers all completion paths, and proper tail calls are guaranteed.

## Reading method and terminology

- **Language** means syntax or semantics fixed by a report/specification. **Library** means a shipped
  API and its documented contract. An implementation technique is not promoted to a language
  guarantee.
- **Successor ABI** means what one step exposes: destructive `next`, a head/tail decomposition, or a
  result containing both item and successor state.
- “Persistent” below means that an earlier traversal state remains usable. It does not imply that
  forcing it is pure, replayed, thread-safe, or free of memoization unless the source says so.
- The comparison is intentionally limited to Haskell, Clojure, OCaml, Scala, Koka, and Gleam. All six
  have adequate first-party material; none was replaced. Gleam's iterator evidence is explicitly
  versioned because the module was removed from later standard-library releases.

## Haskell 2010

### Representation, adapters, and terminals

**Language.** Lists are the persistent algebraic spine `[]` or `(:)`; `[e1, …, ek]` translates to
`e1 : … : []`. Pattern matching therefore exposes the functional successor ABI directly as
`x : xs`, retaining `xs` and the original list. Non-strict evaluation makes list production and
transformation demand-driven, but the report does not prescribe heap layout, memoization machinery,
or iterator objects [H1].

**Library.** `map`, `filter`, `concatMap`, `take`, and the folds operate on that list spine. `foldl'`
strictly forces each accumulator step; `foldr` can short-circuit or produce from an unbounded list when
its operator is lazy in the second argument. `traverse`/`mapM` are the explicit effectful counterparts
that sequence an `Applicative`/`Monad`, whereas ordinary `map :: (a -> b) -> [a] -> [b]` is not an
effect-sequencing API [H2]. Haskell tracks effects by result type (`IO a`, another applicative, etc.),
not by a finite effect row. Laziness is a language evaluation property, while particular fusion and
allocation behavior are implementation details.

### Comprehensions and control

**Language.** A list comprehension is nested, depth-first traversal. The report owns its lowering:
guards become `if`, generators become `concatMap`, and failed generator patterns skip that element
[H1]. This is collection construction, not a statement loop. Mutation-oriented looping is normally
replaced by recursion, folds, list producers/consumers, and monadic recursion for effects.

There is no list-comprehension `continue` or `break`: a guard/filter skips, bounded producers or a
short-circuiting consumer stop, and ordinary function result construction replaces `return`.
`error`/failed patterns are bottoms in the report; recoverable effects are represented in values or
monadic types. The base `bracket` contract acquires, runs an action, and releases even if the action
raises; `finally` always runs its sequel, and `mask` is the primitive used to protect acquisition and
cleanup from asynchronous exceptions [H3]. Those are `IO` library guarantees, not properties of list
iteration. Haskell's report does **not** guarantee proper tail calls as Nymph defines them, nor does it
specify structured task cancellation.

## Clojure

### Representation, adapters, and terminals

**Library/runtime contract.** Clojure deliberately specifies seqs as persistent, immutable logical
lists rather than stateful cursors. `seq` selects an `ISeq`; `first` reads an item and `rest`/`next`
returns the successor seq. Even a seq over Java `Iterable` is immutable and persistent as an object,
but may represent only one pass over the underlying data [C1]. That qualification is important:
persistent successor values do not make an external source replayable.

Most seq-producing functions are incremental and lazy. `lazy-seq` invokes its body on first `seq`,
caches the result, and reuses it; `map` and `filter` return lazy seqs. Clojure has no static effect
system. The `filter` API specifically requires a side-effect-free predicate, while delayed bodies can
otherwise perform unchecked effects when realized [C2]. Transducers separate transformation from a
source and a sink; `eduction` reapplies them for every reduction/iteration [C2].

`reduce` is the central terminal; `into`, `transduce`, `run!`, and collection constructors are terminal
forms. A reducer can return `(reduced x)` to stop and unwrap to `x` [C2]. This is library-level early
termination, not a general `break` statement.

### Comprehensions and control

**Library macro.** `for` is a macro returning a lazy sequence; `doseq` is its eager side-effecting
relative and does not retain the seq head [C2]. Its lowering belongs to `clojure.core` source, not the
language's minimal special-form semantics. `loop` establishes a recursion point; tail-position
`recur` evaluates arguments in order, rebinds them in parallel, and jumps to `loop` or a function
method [C2, C3]. Thus the mutation-loop replacement is seq transforms/reductions plus explicit
`loop`/`recur`, with state carried in bindings.

Clojure has no `continue`; another `recur` performs the next cycle. `reduced` handles reduction break.
Java-style `throw`, `try`/`catch`/`finally`, and function return provide nonlocal completion; `finally`
runs on normal or exceptional exit [C3]. The language does not statically distinguish error, panic,
or delayed effects and does not make structured cancellation or cleanup part of seq consumption.

## OCaml

### Representation, adapters, and terminals

**Library contract.** `Seq.t` is a delayed list. Its public observation is `uncons : 'a t ->
('a * 'a t) option`; the documented underlying model is a thunk producing `Nil` or `Cons (x, xs)`.
This is the closest direct precedent for Nymph's item-plus-successor shape [O1]. The crucial contrast is
that OCaml explicitly permits three behavioral classes: persistent, ephemeral, and affine. A sequence
thunk may mutate or perform another effect. `memoize` converts either class to a persistent sequence;
`once` dynamically rejects a second query. A dispenser `unit -> 'a option` has hidden mutable state and
is always ephemeral [O1]. These are library distinctions; the ordinary OCaml type does not encode them.

`map`, `filter`, `scan`, `take`, and `flat_map` are lazy. `iter`, `fold_left`, `length`, `find`, and
`to_list` force some or all of the sequence, with left-to-right order documented where applicable
[O1]. OCaml function types do not track effects. Consequently the docs warn, for example, that
partitioning may invoke a predicate twice and therefore it should be pure and cheap. OCaml 5 effect
handlers do not add effect rows to function types [O2].

### Loops and control

**Language.** OCaml's `for` and `while` are imperative control expressions returning `unit`; `for`
iterates integer bounds, not `Seq.t` [O3]. Sequence traversal belongs to the library. Functional code
instead uses structural/tail recursion, `List`/`Seq` maps and folds, or `iter` for ordered effects.
There is no built-in `break` or `continue`; recursion, predicates, short-circuit consumers, or an
exception provide those outcomes. Exceptions and `try … with` own error unwinding. `Fun.protect`
guarantees its `finally` function after normal return or exception, and documents what happens when
both body and cleanup raise [O4]. Neither `Seq` nor the loop forms specify cancellation. Tail-call
optimization is documented and tail-mod-cons is available, but this is narrower than Nymph's
backend-independent proper-tail-call guarantee [O5].

## Scala 3

### Representation, adapters, and terminals

**Library contract.** Scala's `Iterator` is the deliberate negative contrast: it is mutable;
`hasNext` probes and `next()` returns an element while advancing the same object, throwing
`NoSuchElementException` at exhaustion. Derived iterators are lazy but consume the underlying one;
after most operations the old iterator must not be reused. `duplicate` simulates independent cursors
using buffering [S1, S2]. `map`, `filter`, and `flatMap` are lazy; `foreach`, `foldLeft`, `toList`, and
other `to` conversions consume. Scala types do not track callback effects, and the official guide
recommends pure adapter callbacks precisely because laziness changes whether/how often they run [S1].

Scala also has persistent collections and `LazyList`, but `Iterable.iterator` creates a cursor. Their
persistence does not alter `Iterator`'s destructive ABI. Collection views defer transformations and
apply them when forced, again without effect typing [S3].

### Comprehensions and control

**Language.** Scala owns syntax-directed for-comprehension lowering: generators, guards, and yielded
bodies translate to the receiver's `map`, `flatMap`, and `withFilter`; a no-`yield` form uses
`foreach` [S4]. Therefore semantics partly belong to whichever type supplies those methods—`List`,
`Iterator`, `Option`, `Future`, or user code—not to a canonical iteration protocol. That extensibility
is useful but makes order, persistence, eagerness, and effects receiver-dependent.

Scala has `while` and mutation. It has no loop-local `continue`; ordinary `return` exits a method,
`throw` unwinds, and `try`'s `finally` expression executes after the protected computation. Scala 3's
`boundary`/`break` offers typed nonlocal early return and is implemented by exceptions when it cannot
be optimized to labels [S5]. Those are general control facilities, not iterator cleanup. Standard
`Future` has no general cancellation contract [S6]. Scala also does not promise Nymph-style proper
tail calls: `@tailrec` verifies optimizable direct recursion, and other recursion can consume stack
[S7].

## Koka

### Representation, adapters, and terminals

**Language/library.** Koka's standard functional sequence is an immutable algebraic `list<a>` with
`Nil`/`Cons`; successor state is the tail matched from `Cons`. Standard `map`, `filter`, `foldl`,
`foldr`, and `foreach` consume finite lists rather than defining a universal cursor ABI [K1]. `map`
and `filter` are eager list transformations, not lazy iterator adapters. List recursion/folds and
effectful `foreach` replace mutation-oriented traversal.

Koka is the strongest effect precedent. An adapter callback has an effect-polymorphic arrow and the
combinator carries that same open row, e.g. `map(xs, f : a -> e b) : e list<b>` and effect-polymorphic
`foldr` [K2]. Effects are therefore neither hidden nor “stored inside” a lazy sequence in this API:
they occur during the eager call and appear in its result effect. Koka rows are open, polymorphic, and
can include effects such as `exn`, `div`, state, and I/O. This supports Nymph's propagation goal but is
not a precedent for a canonical **finite latent** row attached to a persistent iterator.

### Control and cleanup

Koka effect handlers provide typed user-defined nonlocal control, including exceptions and early
exit; handlers compile through an internal free-monad/delimited-control representation, but that is a
compiler implementation fact rather than surface semantics [K2]. General repetition uses recursive
functions and list combinators; `foreach` is an ordinary library function, not a comprehension
lowering hook.

Koka's `finally` handling is unusually relevant: automatic finalization is tied to resumption
contexts, including operations that do not resume or resume multiple times; `raw ctl` opts out and
makes finalization the programmer's responsibility [K2]. It is stronger than ordinary exception-only
`finally`, but the cited material does not specify Nymph's structured task cancellation/join protocol.
Koka's `div` effect makes possible nontermination visible, but its documentation does not establish
Nymph's broad proper-tail-call guarantee.

## Gleam (stdlib 0.38 iterator)

### Representation, adapters, and terminals

**Versioned library contract.** Gleam stdlib 0.38's now-retired `gleam/iterator` is an opaque lazy
sequence with exactly the successor ABI at issue:

```gleam
pub type Step(element, accumulator) { Next(element, accumulator) Done }
pub fn step(iterator: Iterator(a)) -> Step(a, Iterator(a))
```

`unfold` generalizes the same ABI from explicit accumulator state. `map`, `filter`, `flat_map`,
`scan`, `take`, and `transform` return lazy iterators; `fold`, `try_fold`, `to_list`, `each`, and `run`
are terminals. `try_fold` stops on `Error`; predicates such as `any`/`all` short-circuit [G1]. The API
returns successor state rather than mutating a public cursor. Its opacity leaves closure/layout and
private optimization as library implementation details.

Gleam has no static effect rows. The same function arrow can perform target-runtime effects, so lazy
callback effects are delayed but neither separated in the iterator type nor tracked. `Result` makes
expected failure explicit only when the API chooses it (`try_fold`). Panic remains a distinct runtime
failure [G2].

### Loops and control

**Language.** Gleam has no loops; official guidance says iteration uses top-level recursion, with
stdlib functions covering common patterns and manual recursion for complex ones [G3]. Tail calls are
optimized on both Erlang and JavaScript targets, but the language tour advises tail recursion rather
than specifying Nymph's full mutual/higher-order/dynamic PTC scope [G4]. Gleam `use` is callback
inversion syntax, not a for-comprehension protocol; the old iterator's `yield` used it as a library
convenience [G1, G5].

There is no `break`, `continue`, or statement `return`; base cases, pattern matching, short-circuiting
terminals, and `Result` replace them. `panic` is not recoverable Gleam control flow [G2]. OTP process
exit and supervision are runtime facilities, but neither the language iterator API nor recursion tour
specifies structured cancellation or lexical resource cleanup. That is a gap, not evidence that no
cleanup occurs on a particular target.

## Cross-language matrix

| Language   | Persistent successor representation                                                             | Lazy adapters / effects                                                                       | Terminals                                              | Loop/comprehension ownership                                           | Early exit and cleanup                                                                                | Main mutation-loop replacement                                |
| ---------- | ----------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------ | ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
| Haskell    | Language list `x:xs`; both values retained                                                      | Language laziness; pure `map`; effects sequenced through typed `Applicative`/`Monad`, no rows | `foldr`, `foldl'`, `traverse`, producers/consumers     | Report lowers list comprehensions to `concatMap`/guards                | Short-circuit folds; exceptions plus `bracket`/`finally`/`mask`; no structured cancellation guarantee | Recursion, folds, comprehensions, monadic recursion           |
| Clojure    | Runtime/library `ISeq`: `first` + persistent `rest`/`next`; external source may remain one-pass | Cached lazy seqs/transducers; effects unchecked, some callbacks required pure                 | `reduce`, `transduce`, `into`, `run!`; `reduced` stops | `for`/`doseq` are core macros                                          | `reduced`, `recur`, throw, `finally`; no seq cleanup/cancellation contract                            | Seq pipelines, reductions, `loop`/`recur`                     |
| OCaml      | Library `uncons -> option(item, tail)`; type also permits ephemeral/affine thunks               | Lazy `Seq`; effects unchecked/untracked; `memoize` makes persistent                           | `fold_left`, `iter`, `find`, `to_list`                 | Language loops are integer/imperative; Seq traversal is library-owned  | Short-circuit consumers/exceptions; `Fun.protect`; no cancellation contract                           | Recursion, List/Seq combinators and folds                     |
| Scala      | `Iterator` is destructive cursor (contrast); persistent collections create cursors              | Iterator/views lazy; effects unchecked and discouraged in adapters                            | `foreach`, folds, `toList`/`to`                        | Language lowers to receiver `map`/`flatMap`/`withFilter`/`foreach`     | `boundary.break`, throw, `finally`; Future not generally cancellable                                  | Collection combinators, comprehensions, plus imperative loops |
| Koka       | Immutable `list` head/tail; no universal lazy iterator ABI cited                                | Eager combinators; callback effect propagated in open effect row                              | `foldl`/`foldr`, `foreach`                             | Library combinators, recursion; handlers own advanced control          | Typed handler exits; resumption-aware finalization; structured cancellation unspecified               | Recursion, effect-polymorphic maps/folds/foreach              |
| Gleam 0.38 | Opaque iterator; `Step(item, successor)` or `Done`                                              | Lazy; effects delayed but untracked                                                           | `fold`, `try_fold`, `to_list`, `run`                   | No loop/comprehension; library + recursion; `use` only callback syntax | `Result`/short-circuit/base case; panic not recovery; cleanup/cancellation unspecified                | Tail recursion and stdlib combinators                         |

## Design lessons and constraints for Nymph

These constrain the issue; they intentionally do not select a final HIR or ABI.

1. **An item-plus-successor sum is established, but persistence needs a semantic promise.** OCaml
   `uncons` and Gleam `step` validate `Done | Next(item, successor)`. OCaml also demonstrates that the
   same shape can hide affine effects. Nymph must make persistence/replay obligations explicit rather
   than infer them from the return shape.
2. **Separate managed external streams from persistent values.** Clojure's persistent wrapper over a
   one-pass `Iterable` and OCaml's dispensers show why a persistent-looking handle is insufficient.
   This supports Nymph's settled rule that file/network streams are managed resources, not ordinary
   persistent iterators.
3. **Latent effects need both timing and typing.** Haskell distinguishes pure mapping from monadic
   traversal; Koka precisely propagates callback effects but eagerly; Scala/Clojure/OCaml/Gleam delay
   unchecked effects and must document purity caveats. None supplies Nymph's exact combination of a
   persistent lazy successor and canonical finite latent effect row. Nymph must specify when the row
   is attached, when callbacks run, replay behavior, and left-to-right ordering.
4. **Do not let surface sugar accidentally delegate core semantics.** Haskell gives a fixed list
   translation; Scala delegates to receiver methods; Clojure uses macros. Nymph must decide whether
   `for` lowering targets canonical iterator HIR or open method names. Its settled sequential order,
   effects, cleanup, and diagnostics favor recording those facts before backend lowering, regardless
   of surface desugaring.
5. **Adapters and terminals need different contracts.** Every lazy precedent separates deferred
   transformation from forcing. Terminals should state finiteness requirements, exact order,
   short-circuit points, result/error shape, and whether abandoned successor state requires cleanup.
6. **Model early completion once.** Guards/filter are `continue`-like; `take`, short-circuit predicates,
   `reduced`, `try_fold`, base cases, and typed handler exits are `break`-like. Nymph's `for`, `?`,
   return, panic, and cancellation should converge on explicit completion forms rather than ad-hoc
   adapter flags, so the same completion can trigger settled reverse lexical cleanup.
7. **Cleanup cannot be copied from ordinary collection libraries.** Haskell `bracket`, OCaml
   `Fun.protect`, Scala `finally`, and Koka resumption finalization cover different unwind sets. None
   alone covers Nymph's settled normal/`?`/return/panic/cancellation cleanup, suppressed defects,
   child cancellation, and join-after-cleanup. Iterator lowering must preserve the structured cleanup
   continuation rather than treating early exhaustion as an unobservable branch.
8. **Private mutation is an optimization, not an ABI.** Scala demonstrates the observable aliasing
   cost of a destructive cursor; OCaml/Gleam show a functional observation boundary. Nymph may compile
   a uniquely consumed successor chain to private mutation only if aliasing, replay, effect count,
   cleanup, and diagnostics remain observationally identical.
9. **Removing `while` requires an ergonomic state-carrying path.** Across the functional-first set,
   the replacement is recursion plus folds/maps, with Clojure's `loop`/`recur` as the clearest explicit
   state-threading form. Nymph needs folds and shadowing for accumulation and `for` for traversal and
   early exits; complex state machines must remain expressible without public mutation.
10. **Tail calls and cleanup constrain lowering together.** Clojure verifies `recur`, OCaml/Gleam
    optimize tail recursion, and Scala verifies a narrow direct case; none establishes Nymph's settled
    broad PTC guarantee. A loop lowering must not silently consume the tail position or skip pending
    cleanup. Tail calls with lexical resources may need the already-anticipated cleanup continuation.

## Primary sources

All URLs are official language sites, specifications, or first-party standard-library docs/source.

### Haskell

- **H1** — Haskell 2010 Report, expressions (lists and list-comprehension translation):
  <https://www.haskell.org/onlinereport/haskell2010/haskellch3.html>
- **H2** — `base` `Data.List` / list `Foldable` and `Traversable` APIs:
  <https://hackage.haskell.org/package/base/docs/Data-List.html>
- **H3** — `base` `Control.Exception` (`bracket`, `finally`, asynchronous exceptions, masking):
  <https://hackage.haskell.org/package/base/docs/Control-Exception.html>

### Clojure

- **C1** — official sequence reference:
  <https://clojure.org/reference/sequences>
- **C2** — official generated `clojure.core` API and linked first-party source:
  <https://clojure.github.io/clojure/clojure.core-api.html>
- **C3** — official special forms (`loop*`, `recur`, `throw`, `try`):
  <https://clojure.org/reference/special_forms>

### OCaml

- **O1** — OCaml 5.3 `Seq` library contract:
  <https://ocaml.org/manual/5.3/api/Seq.html>
- **O2** — OCaml 5.3 language manual, effect handlers:
  <https://ocaml.org/manual/5.3/effects.html>
- **O3** — OCaml 5.3 language manual, expressions (`while` and `for`):
  <https://ocaml.org/manual/5.3/expr.html>
- **O4** — OCaml 5.3 `Fun.protect`:
  <https://ocaml.org/manual/5.3/api/Fun.html#VALprotect>
- **O5** — OCaml manual, tail recursion and tail-modulo-cons:
  <https://ocaml.org/manual/5.3/tail_mod_cons.html>

### Scala

- **S1** — official Scala collections guide, iterators:
  <https://docs.scala-lang.org/overviews/collections-2.13/iterators.html>
- **S2** — Scala 3 standard-library `Iterator` contract and linked source:
  <https://www.scala-lang.org/api/3.x/scala/collection/Iterator.html>
- **S3** — official Scala collections guide, views:
  <https://docs.scala-lang.org/overviews/collections-2.13/views.html>
- **S4** — Scala 3 specification, for-comprehension translation:
  <https://scala-lang.org/files/archive/spec/3.4/06-expressions.html#for-comprehensions-and-for-loops>
- **S5** — Scala 3 reference, `boundary` and `break`:
  <https://docs.scala-lang.org/scala3/reference/dropped-features/nonlocal-returns.html>
- **S6** — Scala standard-library `Future` overview (non-cancellability):
  <https://docs.scala-lang.org/overviews/core/futures.html>
- **S7** — Scala 3 `@tailrec` API:
  <https://www.scala-lang.org/api/3.x/scala/annotation/tailrec.html>

### Koka

- **K1** — Koka standard `list` API and source links:
  <https://koka-lang.github.io/koka/doc/std_core_list.html>
- **K2** — Koka book/specification (effect-polymorphic `map`/`foldr`, handlers, masking, and
  `initially`/`finally`): <https://koka-lang.github.io/koka/doc/book.html>

### Gleam

- **G1** — official Gleam stdlib 0.38 `iterator` docs and first-party source link:
  <https://hexdocs.pm/gleam_stdlib/0.38.0/gleam/iterator.html>
- **G2** — official Gleam language tour, recoverable `Result` and crashing `panic`:
  <https://tour.gleam.run/data-types/results/> and
  <https://tour.gleam.run/advanced-features/panic/>
- **G3** — official Gleam language tour, recursion instead of loops:
  <https://tour.gleam.run/flow-control/recursion/>
- **G4** — official Gleam language tour, tail calls:
  <https://tour.gleam.run/flow-control/tail-calls/>
- **G5** — official Gleam language tour, `use` expressions:
  <https://tour.gleam.run/advanced-features/use/>

## Gaps and cautions

- No source in this set combines all of Nymph's settled properties: persistent successor state,
  laziness, canonical finite latent effect rows, deterministic structured cancellation/cleanup, and
  broad proper tail calls.
- Official sources generally specify source semantics and library behavior, not a stable machine ABI.
  Closure layout, allocation, fusion, buffering, and cursor scalar replacement remain implementation
  details unless stated otherwise.
- Gleam's evidence is historical but first-party: `gleam/iterator` 0.38 covers the question exactly,
  while current stdlib releases no longer expose that module. It should be treated as a precedent, not
  a current Gleam recommendation.
- Cancellation is the least covered dimension. Haskell documents asynchronous exceptions, Scala
  documents Future's lack of cancellation, and Koka documents control/resumption finalization; none
  specifies Nymph's task-tree cancellation, child join, and suppressed-cleanup-defect rules.
- The cited docs do not settle whether a Nymph terminal owns iterator cleanup, whether only managed
  sources own it, or how loop HIR represents an abandoned successor. Those remain issue-98 design
  questions constrained by Nymph's governing cleanup model.
