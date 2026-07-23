import { NString, structuralDebug, structuralDisplay } from "std/box";

interface Displayable {
	$nymph$display?(): NString;
	$nymph$debug?(): NString;
}

export const display = ($_this: unknown) => {
	const value = $_this as Displayable | null | undefined;
	return value?.$nymph$display?.() ?? new NString(structuralDisplay($_this));
};
export const debug = ($_this: unknown) => {
	const value = $_this as Displayable | null | undefined;
	return value?.$nymph$debug?.() ?? new NString(structuralDebug($_this));
};
