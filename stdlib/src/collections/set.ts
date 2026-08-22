import { NMap, NTuple } from "std/box";

interface NSet<T> {
	inner: NMap<T, NTuple>;
}

export const set_inserted = <T>($_this: NSet<T>, item: T) => {
	const result = Object.create(
		Object.getPrototypeOf($_this),
		Object.getOwnPropertyDescriptors($_this),
	);
	Object.defineProperty(result, "inner", {
		...Object.getOwnPropertyDescriptor($_this, "inner"),
		value: $_this.inner.with(item, new NTuple([])),
	});
	return result as NSet<T>;
};

export const set_removed = <T>($_this: NSet<T>, item: T) => {
	const result = Object.create(
		Object.getPrototypeOf($_this),
		Object.getOwnPropertyDescriptors($_this),
	);
	Object.defineProperty(result, "inner", {
		...Object.getOwnPropertyDescriptor($_this, "inner"),
		value: $_this.inner.without(item),
	});
	return result as NSet<T>;
};
