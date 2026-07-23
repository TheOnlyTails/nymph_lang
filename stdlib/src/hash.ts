import { NInt, structuralHash } from "./box";

export const hash = ($_this: unknown) => new NInt(structuralHash($_this));
