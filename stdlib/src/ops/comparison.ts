import { NChar, NInt, NString } from "std/box";
export const compare_char = (first: NChar, second: NChar) =>
	new NInt(Math.sign(first.v.codePointAt(0)! - second.v.codePointAt(0)!));
export const compare_string = (first: NString, second: NString) =>
	new NInt(Math.sign(first.v.localeCompare(second.v)));
