import { Option } from "./option";

// `Option`-returning helpers use the L1 named-field ABI: the compiler's
// `Option.Some(..)` carries its payload as `{ value }` (option.nym declares
// `Some(value: T)`), so every `Some` below passes an object literal, mirroring
// list.ts/map.ts. `None` is the nullary `Option.None`.
export const length = ($_this: string) => $_this.length;
export const char_at = ($_this: string, i: number) =>
	i < 0
		? i >= -$_this.length
			? Option.Some({ value: $_this[$_this.length + i] })
			: Option.None
		: i < $_this.length
			? Option.Some({ value: $_this[i] })
			: Option.None;
export const substring = ($_this: string, start: number, end: number) => $_this.slice(start, end);
export const index_of = ($_this: string, needle: string) => {
	const i = $_this.indexOf(needle);
	return i === -1 ? Option.None : Option.Some({ value: i });
};
export const last_index_of = ($_this: string, needle: string) => {
	const i = $_this.lastIndexOf(needle);
	return i === -1 ? Option.None : Option.Some({ value: i });
};
export const contains = ($_this: string, needle: string) => $_this.includes(needle);
export const contains_char = ($_this: string, item: string) => $_this.includes(item);
export const starts_with = ($_this: string, prefix: string) => $_this.startsWith(prefix);
export const ends_with = ($_this: string, suffix: string) => $_this.endsWith(suffix);
export const to_upper = ($_this: string) => $_this.toUpperCase();
export const to_lower = ($_this: string) => $_this.toLowerCase();
export const trim = ($_this: string) => $_this.trim();
export const trim_start = ($_this: string) => $_this.trimStart();
export const trim_end = ($_this: string) => $_this.trimEnd();
export const split = ($_this: string, separator: string) => $_this.split(separator);
export const replace = ($_this: string, from: string, to: string) => $_this.replaceAll(from, to);
export const replace_first = ($_this: string, from: string, to: string) => $_this.replace(from, to);
export const repeat = ($_this: string, n: number) => $_this.repeat(n);
export const chars = ($_this: string) => [...$_this];
export const concat = ($_this: string, other: string) => $_this + other;
export const concat_chars = ($_this: string, other: string[]) => $_this + other.join("");
export const reversed = ($_this: string) => [...$_this].reverse().join("");
export const pad_start = ($_this: string, length: number, pad: string) =>
	$_this.padStart(length, pad);
export const pad_end = ($_this: string, length: number, pad: string) => $_this.padEnd(length, pad);
export const first = ($_this: string) =>
	$_this.length > 0 ? Option.Some({ value: $_this[0] }) : Option.None;
export const last = ($_this: string) =>
	$_this.length > 0 ? Option.Some({ value: $_this[$_this.length - 1] }) : Option.None;
export const drop = ($_this: string, n: number) => $_this.slice(n);
export const take = ($_this: string, n: number) => $_this.slice(0, n);
