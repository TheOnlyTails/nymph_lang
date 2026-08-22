# Uniform value boxing

Every Nymph value is compiled to a _boxed value_ uniformly across primitives
(`int`/`uint`/`float`/`char`/`boolean`/`string`), collections, enums, and structs. A
boxed value carries its native nominal methods through a prototype, but method selection follows the
receiver's statically resolved type. Concrete calls target that canonical method directly. Generic
and interface calls use the hidden canonical runtime type object supplied for the caller's static
view. This distinction lets an unchanged embedded enum variant be viewed and dispatched as a
destination enum without changing its runtime value.

The compiler appends one hidden canonical runtime type object for each relevant declared binder, in
declaration order, after all source arguments. A body lowers both `T.default()` and receiver methods
selected through `T` using that object. Generic-to-generic calls forward the same object exactly once;
concrete calls pass the canonical primitive-box prototype or nominal class/enum type object selected
by the checker. This is calling-convention data only: it is not a source-visible `Type<T>`, reflection
facility, or general interface dictionary. Parameterized types retain the identity of their one
canonical emitted artifact, consistent with [ADR-0001](./0001-single-canonical-type-emission.md).

## Considered options

- **Dictionary / witness passing.** The generic call site (which knows the concrete type) passes the impl as a hidden argument; the body calls `$dict.method(...)`. Keeps every value's runtime representation identical to today (raw numbers stay raw), so native `Map`, native arithmetic, and JS interop are untouched, and the cost is localized to generic code. Rejected: it needs two dispatch shapes (a dictionary for bounds a primitive could satisfy — `Plus`/`Comparable`/`Default` — versus duck-typed `recv.method()` for object-only bounds like `Iterator`), and it leaves the language's runtime semantics non-uniform.
- **Uniform boxing (chosen).** One runtime model, one dispatch shape, semantics that hold in JS-land. Costs are accepted (below).

## Consequences

- **Condition unwrap.** `if`/`while`/`!` read the raw value (`if (x.v)`), because `ToBoolean(object)` is unconditionally `true` and consults no method; `&&`/`||` desugar to `a.v ? b : a` (a boxed operand is always truthy, so native `&&` can't short-circuit correctly).
- **Value-equality `HashMap`/`HashSet`** replace native `Map`/`Set`, because boxed keys are identity-distinct (`SameValueZero` consults no method). This requires a `Hash` interface plus `Hash`/`Equals` impls for every key type — and, as a bonus, fixes composite (struct/tuple) keys, which are silently identity-broken today.
- **FFI marshalling** at the JS-interop boundary (`console.log`, JSON, raw arrays handed to intrinsics): unwrap on the way out, wrap on the way in.
- **Allocation** on every primitive value and every arithmetic/predicate result — the accepted price of uniform semantics.
- Generic dispatch uses the hidden static type object for receiver and receiverless methods. This
  supersedes the still-generic-dispatch handling in lowering, supports enum views, and dissolves the
  erased-generic hazard (`Option::map_or_default`'s `R.default()` just works) without general witness
  dictionaries.
- Hidden type-object arguments are deterministic and appended, so source argument evaluation remains left-to-right and source arity/labels are unaffected. Two distinct binders instantiated as the same concrete type still occupy two declared slots; no deduplication changes the calling convention.

This is a design decision recorded ahead of implementation; it is a large effort that reworks codegen, the stdlib collections, and the FFI boundary.
