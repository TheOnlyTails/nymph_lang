# Single canonical type emission

Every enum and struct is compiled to exactly one runtime definition — its class, prototype, and methods, emitted once from its `.nym` source (including the `impl`s that target it) — and every reference (user code, other modules, linked externals) imports that one definition. This replaces the previous per-consumer *materialization*, where a prelude type was re-emitted into each module that used it, producing duplicate prototype objects that interoperated only at the tag level and left externally-constructed values (e.g. an `Option` returned by `list.get`) without any methods.

## Considered options

- **Per-module materialization (previous).** Each consuming module re-emits the prelude type it uses, demand-gated to the methods actually called. Kept the linked-JS intrinsics simple, but produced N duplicate copies plus a methodless hand-written intrinsic `Option`, and broke the moment a value crossed from an intrinsic into method-calling code (which is exactly what the lazy iterator adapters did — `ListIter.next` had to re-wrap `list.get`'s result).
- **Single canonical emission (chosen).** One definition per type, imported everywhere.

## Consequences

- The hand-written intrinsic `Option` (`OPTION_MODULE_JS` in `intrinsics.rs`) is retired; Option-returning externals import the one emitted `Option`.
- Ambient/core types (`Option`, `Result`, the operator interfaces) become real emitted modules — ambient for type-checking, emitted-and-imported at runtime. Ties into the core/std split.
- Supersedes the demand-materialization machinery (`materialize_prelude_enum`/`_struct`, `pending_prelude_*_demands` in `lower_hir.rs`).
- Lets a type's full method API always be emitted (no demand-filtering), which in turn requires uniform generic dispatch — see [ADR-0002](./0002-uniform-value-boxing.md).
