import { Option } from "../option";

export const length = ($_this: any[]) => $_this.length;
export const insert = <T>($_this: T[], i: number, element: T) =>
	$_this.splice(i, 0, element);
// Gap 3 (L1): the compiler's own emitted `Option` ABI (`emit_enum`,
// `nymph-codegen`) builds a field variant via `Object.assign(<tag>, fields)`
// spreading a FIELDS OBJECT (`{ value: X }`) into the result — never a
// positional argument — because that's the exact shape the checker's
// generated `Some(value)` pattern binding reads back (`_subj.value`). Every
// `Option.Some(..)` call below must pass an object literal naming the field,
// not the bare value, to interoperate with a `match` in the user's own
// Nymph program.
export const get = <T>($_this: T[], i: number) =>
	i < $_this.length ? Option.Some({ value: $_this[i] }) : Option.None;
export const remove = <T>($_this: T[], i: number) =>
	i < $_this.length ? Option.Some({ value: $_this.splice(i, 1)[0] }) : Option.None;
export const push = <T>($_this: T[], item: T) => {
	$_this.push(item);
};
export const pop = <T>($_this: T[]) =>
	$_this.length === 0 ? Option.None : Option.Some({ value: $_this.pop()! });
export const first = <T>($_this: T[]) =>
	$_this.length === 0 ? Option.None : Option.Some({ value: $_this[0] });
export const last = <T>($_this: T[]) =>
	$_this.length === 0 ? Option.None : Option.Some({ value: $_this[$_this.length - 1] });
export const clear = ($_this: any[]) => {
	$_this.length = 0;
};
export const splice = <T>($_this: T[], start: number, end: number, replacement: T[]): T[] =>
	$_this.splice(start, end - start, ...replacement);
export const slice = <T>($_this: T[], start: number, end: number): T[] =>
	$_this.slice(start, end);
export const concat = <T>($_this: T[], other: T[]): T[] => $_this.concat(other);
export const drop = <T>($_this: T[], n: number): T[] => $_this.slice(n);
export const take = <T>($_this: T[], n: number): T[] => $_this.slice(0, n);
export const reversed = <T>($_this: T[]): T[] => [...$_this].reverse();
export const sorted = <T>($_this: T[]): T[] =>
	[...$_this].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
export const chunked = <T>($_this: T[], size: number): T[][] => {
	const result: T[][] = [];
	if (size <= 0) return result;
	for (let i = 0; i < $_this.length; i += size) {
		result.push($_this.slice(i, i + size));
	}
	return result;
};
export const distinct = <T>($_this: T[]): T[] => [...new Set($_this)];
export const contains = <T>($_this: T[], item: T): boolean => $_this.includes(item);
export const to_string = ($_this: unknown[]): string => `#[${$_this.join(", ")}]`;
