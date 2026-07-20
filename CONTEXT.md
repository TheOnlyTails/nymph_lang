# Nymph compiler

The compiler for the Nymph language (Rust → JavaScript). This glossary fixes the project's terms for how Nymph values and types are represented and compiled.

## Language

**Boxed value**:
The runtime form of every Nymph value — a wrapper object carrying its type's methods via a prototype, so `x.method(...)` dispatches uniformly for primitives and objects alike. See [ADR-0002](./docs/adr/0002-uniform-value-boxing.md).
_Avoid_: unboxed value.

**Raw value**:
The un-wrapped JavaScript form of a value (`3`, `"x"`, a native array) — appears only transiently, at a marshalling boundary or inside a condition (`if (x.v)`).
_Avoid_: primitive (a Nymph `int` is a boxed value; its raw value is a JS number).

**Canonical emission**:
Compiling each enum/struct to exactly one runtime definition, imported everywhere it is used. See [ADR-0001](./docs/adr/0001-single-canonical-type-emission.md).
_Avoid_: materialization (the superseded per-consumer scheme).

**Materialization**:
The superseded mechanism that re-emitted a prelude type into each module that used it, producing duplicate definitions. Replaced by canonical emission; the term survives only in existing code (`materialize_prelude_*`).
_Avoid_: use "canonical emission" for the current model.

**Intrinsic**:
A stdlib operation implemented directly in hand-written JS/TS and linked in (e.g. `list.get`), as opposed to one with a Nymph body the compiler lowers.
_Avoid_: builtin.

**Marshalling**:
Converting a boxed value to a raw value, or the reverse, at the JavaScript-interop boundary.
_Avoid_: conversion, casting (a cast is a Nymph-level `as` between types).
