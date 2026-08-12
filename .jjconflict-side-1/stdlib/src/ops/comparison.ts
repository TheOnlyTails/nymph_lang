import { NChar, NString } from "std/box";
export const compare_char = (first: NChar, second: NChar) =>
	BigInt(Math.sign(first.v.codePointAt(0)! - second.v.codePointAt(0)!));
export const compare_string = (first: NString, second: NString) =>
	BigInt(Math.sign(first.v.localeCompare(second.v)));
