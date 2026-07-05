import { Option } from "./option";

export const length = ($_this: string) => $_this.length;
export const char_at = ($_this: string, i: number) =>
	i < 0
		? Option.Some($_this[$_this.length + i])
		: i < $_this.length
			? Option.Some($_this[i])
			: Option.None;
export const substring = ($_this: string, start: number, end: number) => $_this.slice(start, end);
export const index_of = ($_this: string, needle: string) => {
	const i = $_this.indexOf(needle);
	return i === -1 ? Option.None : Option.Some(i);
};
export const last_index_of = ($_this: string, needle: string) => {
	const i = $_this.lastIndexOf(needle);
	return i === -1 ? Option.None : Option.Some(i);
};
export const contains = ($_this: string, needle: string) => $_this.includes(needle);
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
export const reversed = ($_this: string) => [...$_this].reverse().join("");
export const pad_start = ($_this: string, length: number, pad: string) =>
	$_this.padStart(length, pad);
export const pad_end = ($_this: string, length: number, pad: string) => $_this.padEnd(length, pad);
