import { NBool, NChar, NList, NString, NUint } from "std/box";
import { Option } from "std/option";

// `Option`-returning helpers use the L1 named-field ABI: the compiler's
// `Option.Some(..)` carries its payload as `{ value }` (option.nym declares
// `Some(value: T)`), so every `Some` below passes an object literal, mirroring
// list.ts/map.ts. `None` is the nullary `Option.None`.
export const length = ($_this: NString) => new NUint(Array.from($_this.v).length);
export const char_at = ($_this: NString, i: NUint) => {
	const char = Array.from($_this.v)[i.v];
	return char === undefined ? Option.None : Option.Some({ value: new NChar(char) });
};
export const substring = ($_this: NString, start: NUint, end: NUint) =>
	new NString(Array.from($_this.v).slice(start.v, end.v).join(""));
export const index_of = ($_this: NString, needle: NString) => {
	const i = $_this.v.indexOf(needle.v);
	return i === -1
		? Option.None
		: Option.Some({ value: new NUint(Array.from($_this.v.slice(0, i)).length) });
};
export const last_index_of = ($_this: NString, needle: NString) => {
	const i = $_this.v.lastIndexOf(needle.v);
	return i === -1
		? Option.None
		: Option.Some({ value: new NUint(Array.from($_this.v.slice(0, i)).length) });
};
export const contains = ($_this: NString, needle: NString) =>
	new NBool($_this.v.includes(needle.v));
export const contains_char = ($_this: NString, item: NChar) => new NBool($_this.v.includes(item.v));
export const starts_with = ($_this: NString, prefix: NString) =>
	new NBool($_this.v.startsWith(prefix.v));
export const ends_with = ($_this: NString, suffix: NString) =>
	new NBool($_this.v.endsWith(suffix.v));
export const to_upper = ($_this: NString) => new NString($_this.v.toUpperCase());
export const to_lower = ($_this: NString) => new NString($_this.v.toLowerCase());
export const trim = ($_this: NString) => new NString($_this.v.trim());
export const trim_start = ($_this: NString) => new NString($_this.v.trimStart());
export const trim_end = ($_this: NString) => new NString($_this.v.trimEnd());
export const split = ($_this: NString, separator: NString) =>
	new NList($_this.v.split(separator.v).map((part) => new NString(part)));
export const replace = ($_this: NString, from: NString, to: NString) =>
	new NString($_this.v.replaceAll(from.v, to.v));
export const replace_first = ($_this: NString, from: NString, to: NString) =>
	new NString($_this.v.replace(from.v, to.v));
export const repeat = ($_this: NString, n: NUint) => new NString($_this.v.repeat(n.v));
export const chars = ($_this: NString) =>
	new NList(Array.from($_this.v, (char) => new NChar(char)));
export const concat = ($_this: NString, other: NString) => new NString($_this.v + other.v);
export const concat_chars = ($_this: NString, other: NList<NChar>) =>
	new NString($_this.v + other.v.map((char) => char.v).join(""));
export const reversed = ($_this: NString) => new NString(Array.from($_this.v).reverse().join(""));
export const pad_start = ($_this: NString, length: NUint, pad: NChar) => {
	const padding = Math.max(0, length.v - Array.from($_this.v).length);
	return new NString(pad.v.repeat(padding) + $_this.v);
};
export const pad_end = ($_this: NString, length: NUint, pad: NChar) => {
	const padding = Math.max(0, length.v - Array.from($_this.v).length);
	return new NString($_this.v + pad.v.repeat(padding));
};
