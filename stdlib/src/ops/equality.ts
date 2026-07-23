import { NBool, NInt, NUint, protocolEquals } from "std/box";

export const primitive_equals = ($_this: NInt | NUint, other: NInt | NUint) =>
	new NBool($_this.v === other.v);

export const equals = ($_this: unknown, other: unknown) => new NBool(protocolEquals($_this, other));

export const not_equals = ($_this: unknown, other: unknown) =>
	new NBool(!protocolEquals($_this, other));
