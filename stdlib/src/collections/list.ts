import { Option } from "../option";
import { NBool, NList, NMap, NString, NUint, protocolEquals } from "../box";

export const length = ($_this: NList) => new NUint($_this.v.length);
export const insert = <T>($_this: NList<T>, i: NUint, element: T) => {
	$_this.v.splice(i.v, 0, element);
};
// Gap 3 (L1): the compiler's own emitted `Option` ABI (`emit_enum`,
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
	i.v < $_this.v.length ? Option.Some({ value: $_this.v.splice(i.v, 1)[0] }) : Option.None;
export const push = <T>($_this: NList<T>, item: T) => {
	$_this.v.push(item);
};
export const pop = <T>($_this: NList<T>) =>
	$_this.v.length === 0 ? Option.None : Option.Some({ value: $_this.v.pop()! });
export const first = <T>($_this: NList<T>) =>
	$_this.v.length === 0 ? Option.None : Option.Some({ value: $_this.v[0] });
export const last = <T>($_this: NList<T>) =>
	$_this.v.length === 0 ? Option.None : Option.Some({ value: $_this.v[$_this.v.length - 1] });
export const clear = ($_this: NList) => {
	$_this.v.length = 0;
};
export const splice = <T>($_this: NList<T>, start: NUint, end: NUint, replacement: NList<T>) =>
	new NList($_this.v.splice(start.v, end.v - start.v, ...replacement.v));
export const slice = <T>($_this: NList<T>, start: NUint, end: NUint) =>
	new NList($_this.v.slice(start.v, end.v));
export const concat = <T>($_this: NList<T>, other: NList<T>) => new NList($_this.v.concat(other.v));
export const drop = <T>($_this: NList<T>, n: NUint) => new NList($_this.v.slice(n.v));
export const take = <T>($_this: NList<T>, n: NUint) => new NList($_this.v.slice(0, n.v));
export const reversed = <T>($_this: NList<T>) => new NList([...$_this.v].reverse());
export const sorted = <T extends { v: unknown }>($_this: NList<T>) =>
	new NList([...$_this.v].sort((a, b) => (a.v < b.v ? -1 : a.v > b.v ? 1 : 0)));
export const chunked = <T>($_this: NList<T>, size: NUint) => {
	const result: NList<T>[] = [];
	if (size.v <= 0) return new NList(result);
	for (let i = 0; i < $_this.v.length; i += size.v) {
		result.push(new NList($_this.v.slice(i, i + size.v)));
	}
	return new NList(result);
};
export const distinct = <T>($_this: NList<T>) => {
	const seen = new NMap();
	const result: T[] = [];
	for (const item of $_this.v) {
		if (!seen.has(item)) {
			seen.set(item, true);
			result.push(item);
		}
	}
	return new NList(result);
};
export const contains = <T>($_this: NList<T>, item: T) =>
	new NBool($_this.v.some((candidate) => protocolEquals(candidate, item)));
export const to_string = ($_this: NList) => new NString(`#[${$_this.v.join(", ")}]`);
