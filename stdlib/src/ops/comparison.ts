import { NChar, NFloat, NInt, NString, NUint } from "std/box";

type Numeric = NInt | NUint | NFloat;

export const compare_number = (first: Numeric, second: Numeric) =>
	new NInt(Math.sign(first.v - second.v));
export const compare_char = (first: NChar, second: NChar) =>
	new NInt(Math.sign(first.v.codePointAt(0)! - second.v.codePointAt(0)!));
export const compare_string = (first: NString, second: NString) =>
	new NInt(Math.sign(first.v.localeCompare(second.v)));
