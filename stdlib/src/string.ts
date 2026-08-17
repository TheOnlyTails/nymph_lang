import { NChar, NString, NUint } from "std/box";
import { Option } from "std/option";

// `Option`-returning helpers use the named-field ABI: the compiler's
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
export const to_upper = ($_this: NString) => new NString($_this.v.toUpperCase());
export const to_lower = ($_this: NString) => new NString($_this.v.toLowerCase());
export const trim = ($_this: NString) => new NString($_this.v.trim());
export const trim_start = ($_this: NString) => new NString($_this.v.trimStart());
export const trim_end = ($_this: NString) => new NString($_this.v.trimEnd());
export const concat = ($_this: NString, other: NString) => new NString($_this.v + other.v);
