# Compile-time metaprogramming

Nymph has one pure compile-time execution phase. `const let` requires its value to be
evaluated in that phase. `const func` may be called there and may also be emitted for
runtime use when its signature contains only runtime-capable types. Values from
`std/meta` are compile-time-only. Const code may use ordinary pure Nymph control flow
and call other const-compatible code, but it cannot use effects, external functions,
managed resources, asynchronous work, or ambient host state.

Const evaluation is deterministic. Each root evaluation has fixed limits for executed
steps, allocated value cells, call depth, and emitted tokens. The compiler reports the
limit, the const call that exhausted it, and the expansion trace. These limits are part
of the compiler version rather than machine-dependent time or memory budgets.

## Token values and expansion

`\(...)` captures the tokens between its parentheses as an immutable, flat
`std/meta.Tokens` value. Capture lexes but does not parse ordinary contents. Delimiters
are tokens, trivia is omitted, and the outer parentheses are not included.

Within a token literal, `$(value)` accepts any value implementing
`Into<meta.Tokens>`. All meaningful typed `std/meta` syntax values implement that
conversion. `...$(items)` accepts any finite pure `Iterable` or `Iterator` whose items
implement `Into<meta.Tokens>`. A final token before
the closing parenthesis is a separator placed only between items, for example
`...$(items,)`, `...$(items|)`, `...$(items])`, or `...$(items->)`. Empty collections
emit nothing and singletons emit no separator. Iteration order is output order and uses
the same deterministic evaluation and allocation limits as other const execution.

Outside a token literal, `$(expression)` accepts one token-convertible value or a finite
iterable of token-convertible values and inserts the flattened tokens at the surrounding
grammar position. `Result<T, meta.Diagnostic>` and the diagnostic-list error form are
accepted for every convertible `T`. The destination parser, not the producer, decides
whether the result is a declaration, member, field, variant, parameter, generic
parameter, match arm, statement, expression, pattern, or type. There is no separate
quote operation or macro declaration.

`$name(arguments)` is exact syntax sugar for `$(name(arguments))` in every destination.
Inside a token literal it is the equivalent interpolation shorthand, and `...$name(arguments)`
is the equivalent splice. The callee is one identifier, `$` must be adjacent to it, and
arguments use ordinary call syntax. The formatter keeps the shorthand spelling. Member
calls and arbitrary callee expressions continue to use the long form.

## Attached declaration macros

`$[call(...)]` immediately precedes a declaration and adds peer declarations after it.
It cannot replace, remove, or modify its target. It is valid for every declaration kind,
including imports and external declarations. The parser builds the handwritten target
once, and the compiler retains that original exactly once. The checker supplies its
immutable typed `std/meta` value as the call's implicit first argument. A broad
`meta.Declaration` parameter accepts every declaration; narrow types such as
`meta.Function`, `meta.Struct`, and `meta.Enum` are checked before execution.

Attached calls return any `T: Into<meta.Tokens>`, any deterministic iterable of such
values, `Result<T, meta.Diagnostic>`, or the corresponding diagnostic-list form.
Every attachment may produce zero, one, or many declarations. The compiler appends each
attachment's output after the original in attachment source order.

Stacked attachments are independent. Each receives the same typed original, with the
same tokens, spans, syntax identity, and provenance; no attachment sees or transforms a
sibling's output. Each target parameter is checked against the original declaration
category. Generated declarations are ordinary peers, so re-emitting the target or
otherwise conflicting with it produces the normal declaration-collision diagnostic
with expansion provenance. There is no collection handoff or inner/outer pipeline.

Replacement, removal, and source transformation use direct `$(expression)` expansion.
Code performing such a transformation explicitly parses or carries a typed
`meta.Function`, `meta.Declaration`, or other syntax value and expands the returned
replacement at the intended grammar position.

```nymph
const let original = meta.Function.parse(\(func answer(): int = 0))
const func replace(function: meta.Function): meta.Function = meta.Function(
  visibility = function.visibility,
  name = function.name,
  is_const = function.is_const,
  is_async = function.is_async,
  parameters = function.parameters,
  return_type = function.return_type,
  body = meta.Expression.parse(\(42)),
  span = function.span,
)

$(replace(original))
```

## Expansion and imports

Expansion precedes every ordinary AST consumer: import collection, interface
extraction, name and type checking, lowering, formatting and tooling views, and code
generation all read the expanded module plus its provenance.

Imports produced by expansion participate in a deterministic fixed point:

1. Parse the root module and imports currently known for reachable modules.
2. Expand reachable modules in canonical module-path order. Within a module, process
   source declarations and generated output from left to right.
3. Resolve newly emitted imports in that same order, add previously unseen modules,
   and repeat expansion for modules whose explicit const dependencies changed.
4. Stop when a round adds neither an import edge nor a changed expansion result.

An expansion may use only const definitions available through imports from the
preceding round. This removes scheduling dependence: a newly generated import becomes
usable in the next round, never midway through the current one. Hashes of source,
resolved const interfaces, compiler version, and evaluator limits key expansion
results. Repeating an identical graph or const call while it is active is an expansion
cycle. Cycles, unresolved imports, and exhaustion of the fixed round/import/output
limits are diagnostics; no partial result proceeds to lowering.

The canonical project query also retains the final provenance-aware token stream for
the expanded runtime tree. Tooling renders and formats that stream rather than printing
the internal AST. `nymph expand <module-path>` and the compiler lab's **Macro Expansion**
panel are views over this same query: they contain no separate expansion pass, expose
no partial output after an error, and leave structured diagnostics intact for terminal,
LSP, and browser consumers.

## `std/meta`, hygiene, and provenance

`std/meta` provides immutable `Tokens`, flat `Token`, `Name`, `Span`, `Diagnostic`,
`Declaration`, and specific construct values including `Function`, `Let`, `Struct`,
`Enum`, `Interface`, `Implementation`, `Namespace`, `Import`, `Effect`, `Expression`,
`Statement`, `Pattern`, `Type`, `Parameter`, `Field`, `Variant`, and `MatchArm`.
Constructs parse from and implement `Into<Tokens>` back to token streams without losing
spans, hygiene, or provenance. This conversion applies to `Declaration`, every narrow
declaration record, and meaningful expression, statement, pattern, type, parameter,
field, variant, match-arm, import, and member values. The compiler adapter implements
the stable language schema rather than exposing its internal Rust AST. `Token` mirrors
the compiler's flat token vocabulary, including `Const`
and standalone printable punctuation such as `Hash`, `Dollar`, `Backslash`, and
`Backtick`.

Captured tokens keep caller context, literal-written tokens use the const definition's
context, and interpolated tokens retain their existing context. `Name.fresh` creates a
new hygienic name and `Name.exposed` deliberately resolves at the expansion site.
Generated node IDs derive from the invocation identity, expansion step, and output
position. Generated spans retain both their token origin and the invocation/attachment
chain so diagnostics can show a macro expansion trace. Hidden runtime type objects
remain calling-convention data and are not exposed as reflection.

## Diagnostics

Const evaluation, token conversion, iteration, independent attachment execution, generated
parsing and typing, compile-time-only leakage, resource limits, cycles, and generated
import fixed-point failures all produce the normal structured Nymph diagnostic. The
primary label identifies the failing source operation. Secondary labels identify const
calls, the attached target and expected parameter type, and every available macro
invocation and definition in the embedded provenance chain. The terminal uses the
standard visual renderer. The LSP consumes the same structured value, while WASM exposes both that
structured value and the canonical no-ANSI visual rendering used by the compiler lab.

Every diagnostic includes a practical correction. Missing const dependencies recommend
`const func` or `const let`; conversion failures name `Into<meta.Tokens>`; iterator
failures name the required finite pure interfaces; limit failures identify the active
macro/function and recommend terminating recursion or reducing output; generated parse
and type failures retain their ordinary diagnostic and point back through the expansion
trace. User-returned `meta.Diagnostic` values receive the same rendering and provenance
handling.

## Consequences

- Lexer acceptance is broader than the Nymph grammar. Printable punctuation is
  tokenized so macros can consume it and the parser can reject foreign syntax with a
  useful diagnostic. Existing compounds retain longest-match behavior.
- Generated declarations pass through all normal visibility, coherence, effect,
  checking, lowering, tooling, and code-generation stages.
- A runtime use of a `const let` refers to its one canonical boxed binding; compile-time
  substitution must not create fresh runtime boxes.
