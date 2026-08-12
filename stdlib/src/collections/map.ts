import {
	NBool,
	NList,
	NMap,
	NString,
	NTuple,
	NUint,
	nymphType,
	nymphTypeProjection,
	nymphSetPrototypeOf,
} from "std/box";
import { Option } from "std/option";

export const size = <K, V>($_this: NMap<K, V>) => new NUint($_this.size);
export const get = <K, V>($_this: NMap<K, V>, key: K) =>
	$_this.has(key) ? Option.Some({ value: $_this.get(key)! }) : Option.None;
export const insert = <K, V>($_this: NMap<K, V>, key: K, value: V) => {
	const existed = $_this.has(key);
	$_this.set(key, value);
	return new NBool(!existed);
};
export const remove = <K, V>($_this: NMap<K, V>, key: K) => {
	if ($_this.has(key)) {
		const value = $_this.get(key)!;
		$_this.delete(key);
		return Option.Some({ value });
	}
	return Option.None;
};
export const clear = <K, V>($_this: NMap<K, V>) => $_this.clear();
export const get_or_insert = <K, V>($_this: NMap<K, V>, key: K, defaultValue: V): V => {
	if (!$_this.has(key)) {
		$_this.set(key, defaultValue);
	}
	return $_this.get(key)!;
};
export const contains_key = <K, V>($_this: NMap<K, V>, key: K) => new NBool($_this.has(key));
export const keys = <K, V>($_this: NMap<K, V>) =>
	nymphSetPrototypeOf(
		new NList([...$_this.keys()]),
		nymphType(NList.prototype, [nymphTypeProjection($_this, [0])]),
	);
export const values = <K, V>($_this: NMap<K, V>) =>
	nymphSetPrototypeOf(
		new NList([...$_this.values()]),
		nymphType(NList.prototype, [nymphTypeProjection($_this, [1])]),
	);
export const entries = <K, V>($_this: NMap<K, V>) => {
	const key = nymphTypeProjection($_this, [0]);
	const value = nymphTypeProjection($_this, [1]);
	const tuple = nymphType(NTuple.prototype, [key, value]);
	return nymphSetPrototypeOf(
		new NList([...$_this.entries()].map((entry) => nymphSetPrototypeOf(new NTuple(entry), tuple))),
		nymphType(NList.prototype, [tuple]),
	);
};
export const merge = <K, V>($_this: NMap<K, V>, other: NMap<K, V>) =>
	nymphSetPrototypeOf(
		new NMap([...$_this.entries(), ...other.entries()]),
		Object.getPrototypeOf($_this),
	);
export const to_string = ($_this: NMap<unknown, unknown>) =>
	new NString(`#{${[...$_this.entries()].map(([key, value]) => `${key}: ${value}`).join(", ")}}`);
