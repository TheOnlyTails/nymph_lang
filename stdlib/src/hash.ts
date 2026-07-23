import { NInt, structuralHash } from "std/box";

export const hash = ($_this: unknown) => new NInt(structuralHash($_this));
