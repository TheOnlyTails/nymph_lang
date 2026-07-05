import { Option } from "../option";

export const length = ($_this: any[]) => $_this.length;
export const insert = <T>($_this: T[], i: number, element: T) =>
	$_this.splice(i, 0, element);
export const get = <T>($_this: T[], i: number) =>
	i < $_this.length ? Option.Some($_this[i]) : Option.None;
export const remove = <T>($_this: T[], i: number): T => $_this.splice(i, 1)[0];
export const push = <T>($_this: T[], item: T) => {
	$_this.push(item);
};
export const pop = <T>($_this: T[]) =>
	$_this.length === 0 ? Option.None : Option.Some($_this.pop()!);
export const first = <T>($_this: T[]) =>
	$_this.length === 0 ? Option.None : Option.Some($_this[0]);
export const last = <T>($_this: T[]) =>
	$_this.length === 0 ? Option.None : Option.Some($_this[$_this.length - 1]);
export const clear = ($_this: any[]) => {
	$_this.length = 0;
};
export const splice = <T>($_this: T[], start: number, end: number, replacement: T[]): T[] =>
	$_this.splice(start, end - start, ...replacement);
export const slice = <T>($_this: T[], start: number, end: number): T[] =>
	$_this.slice(start, end);
export const concat = <T>($_this: T[], other: T[]): T[] => $_this.concat(other);
export const drop = <T>($_this: T[], n: number): T[] => $_this.slice(n);
export const take = <T>($_this: T[], n: number): T[] => $_this.slice(0, n);
export const reversed = <T>($_this: T[]): T[] => [...$_this].reverse();
export const sorted = <T>($_this: T[]): T[] => [...$_this].sort();
export const chunked = <T>($_this: T[], size: number): T[][] => {
	const result: T[][] = [];
	for (let i = 0; i < $_this.length; i += size) {
		result.push($_this.slice(i, i + size));
	}
	return result;
};
export const distinct = <T>($_this: T[]): T[] => [...new Set($_this)];
export const contains = <T>($_this: T[], item: T): boolean => $_this.includes(item);
export const to_string = ($_this: unknown[]): string => `#[${$_this.join(", ")}]`;
