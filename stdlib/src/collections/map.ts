import {
	NList,
	NMap,
	NTuple,
	nymphType,
	nymphTypeProjection,
	nymphSetPrototypeOf,
} from "std/box";
import { Option } from "std/option";

export const size = <K, V>($_this: NMap<K, V>) => BigInt($_this.size);
export const get = <K, V>($_this: NMap<K, V>, key: K) =>
	$_this.has(key) ? Option.Some({ value: $_this.get(key)! }) : Option.None;
export const inserted = <K, V>($_this: NMap<K, V>, key: K, value: V) =>
	nymphSetPrototypeOf($_this.with(key, value), Object.getPrototypeOf($_this));
export const removed = <K, V>($_this: NMap<K, V>, key: K) =>
	nymphSetPrototypeOf($_this.without(key), Object.getPrototypeOf($_this));
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
