import { Option } from "../option";

export const size = <K, V>($_this: Map<K, V>) => $_this.size;
export const get = <K, V>($_this: Map<K, V>, key: K) =>
	$_this.has(key) ? Option.Some({ value: $_this.get(key)! }) : Option.None;
export const insert = <K, V>($_this: Map<K, V>, key: K, value: V): boolean => {
	const existed = $_this.has(key);
	$_this.set(key, value);
	return !existed;
};
export const remove = <K, V>($_this: Map<K, V>, key: K) => {
	if ($_this.has(key)) {
		const value = $_this.get(key)!;
		$_this.delete(key);
		return Option.Some({ value });
	}
	return Option.None;
};
export const clear = <K, V>($_this: Map<K, V>) => $_this.clear();
export const get_or_insert = <K, V>($_this: Map<K, V>, key: K, defaultValue: V): V => {
	if (!$_this.has(key)) {
		$_this.set(key, defaultValue);
	}
	return $_this.get(key)!;
};
export const contains_key = <K, V>($_this: Map<K, V>, key: K): boolean => $_this.has(key);
export const keys = <K, V>($_this: Map<K, V>): K[] => [...$_this.keys()];
export const values = <K, V>($_this: Map<K, V>): V[] => [...$_this.values()];
export const entries = <K, V>($_this: Map<K, V>): [K, V][] => [...$_this.entries()];
export const merge = <K, V>($_this: Map<K, V>, other: Map<K, V>): Map<K, V> =>
	new Map([...$_this.entries(), ...other.entries()]);
export const to_string = ($_this: Map<unknown, unknown>): string =>
	`#{${[...$_this.entries()].map(([key, value]) => `${key}: ${value}`).join(", ")}}`;
