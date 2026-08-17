import {
	NList,
	NString,
	NUint,
	nymphArrayPush,
	nymphArraySetLength,
	nymphArraySplice,
	nymphSetPrototypeOf,
} from "std/box";
import { Option } from "std/option";

export const length = ($_this: NList) => new NUint($_this.v.length);
export const insert = <T>($_this: NList<T>, i: NUint, element: T) => {
	nymphArraySplice($_this.v, i.v, 0, element);
};
// The compiler's emitted `Option` ABI (`emit_enum`,
// `nymph-codegen`) builds a field variant via `Object.assign(<tag>, fields)`
// spreading a FIELDS OBJECT (`{ value: X }`) into the result — never a
// positional argument — because that's the exact shape the checker's
// generated `Some(value)` pattern binding reads back (`_subj.value`). Every
// `Option.Some(..)` call below must pass an object literal naming the field,
// not the bare value, to interoperate with a `match` in the user's own
// Nymph program.
export const get = <T>($_this: NList<T>, i: NUint) =>
	i.v < $_this.v.length ? Option.Some({ value: $_this.v[i.v] }) : Option.None;
export const remove = <T>($_this: NList<T>, i: NUint) =>
	i.v < $_this.v.length
		? Option.Some({ value: nymphArraySplice($_this.v, i.v, 1)[0] })
		: Option.None;
export const push = <T>($_this: NList<T>, item: T) => {
	nymphArrayPush($_this.v, item);
};
export const clear = ($_this: NList) => {
	nymphArraySetLength($_this.v, 0);
};
export const splice = <T>($_this: NList<T>, start: NUint, end: NUint, replacement: NList<T>) =>
	nymphSetPrototypeOf(
		new NList(nymphArraySplice($_this.v, start.v, end.v - start.v, ...replacement.v)),
		Object.getPrototypeOf($_this),
	);
export const slice = <T>($_this: NList<T>, start: NUint, end: NUint) =>
	nymphSetPrototypeOf(new NList($_this.v.slice(start.v, end.v)), Object.getPrototypeOf($_this));
export const to_string = ($_this: NList) => new NString(`#[${$_this.v.join(", ")}]`);
