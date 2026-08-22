import { NBool } from "std/box";

export const primitive_equals = ($_this: bigint, other: bigint) => new NBool($_this === other);
