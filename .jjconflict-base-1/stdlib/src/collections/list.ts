import { NList, nymphHostIndex } from "std/box";
import { Option } from "std/option";

export const length = ($_this: NList) => BigInt($_this.v.length);
// Gap 3 (L1): the compiler's own emitted `Option` ABI (`emit_enum`,
// `nymph-codegen`) builds a field variant via `Object.assign(<tag>, fields)`
// spreading a FIELDS OBJECT (`{ value: X }`) into the result — never a
// positional argument — because that's the exact shape the checker's
// generated `Some(value)` pattern binding reads back (`_subj.value`). Every
// `Option.Some(..)` call below must pass an object literal naming the field,
// not the bare value, to interoperate with a `match` in the user's own
// Nymph program.
export const get = <T>($_this: NList<T>, i: bigint) => {
	const index = nymphHostIndex(i);
	return index < $_this.v.length ? Option.Some({ value: $_this.v.get(index) }) : Option.None;
};
export const appended = <T>($_this: NList<T>, item: T) => $_this.appended(item);
export const replaced = <T>($_this: NList<T>, i: bigint, item: T) => $_this.replaced(i, item);
export const slice = <T>($_this: NList<T>, start: bigint, end: bigint) => $_this.slice(start, end);
