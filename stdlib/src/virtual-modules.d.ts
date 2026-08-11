declare module "std/box" {
	export interface NymphOption<T> {
		readonly [tag: symbol]: unknown;
		readonly value?: T;
	}

	export interface NymphIterator<T> {
		next(): NymphOption<T>;
	}

	export class NBox<T> {
		constructor(value: T);
		v: T;
		equals(other: unknown): NBool;
		hash(): NInt;
		display(): NString;
		debug(): NString;
		toString(): string;
	}

	export class NInt extends NBox<number> {}
	export class NUint extends NBox<number> {}
	export class NFloat extends NBox<number> {}
	export class NChar extends NBox<string> {}
	export class NBool extends NBox<boolean> {}
	export class NString extends NBox<string> {}

	export class NList<T = unknown> extends NBox<T[]> {
		index(key: NUint): T;
		push(item: T): void;
		iter(): NymphIterator<T>;
	}

	export class NTuple<T = unknown> extends NBox<T[]> {
		index(key: NUint): T;
		readonly 0: T;
		readonly 1: T;
	}

	export class NMap<K = unknown, V = unknown> extends NBox<unknown> {
		constructor(entries?: Iterable<readonly [K, V]>);
		readonly size: number;
		get(key: K): V | undefined;
		has(key: K): boolean;
		set(key: K, value: V): this;
		delete(key: K): boolean;
		clear(): void;
		keys(): IterableIterator<K>;
		values(): IterableIterator<V>;
		entries(): IterableIterator<[K, V]>;
		[Symbol.iterator](): IterableIterator<[K, V]>;
	}

	export function protocolEquals(left: unknown, right: unknown): boolean;
	export function structuralHash(value: unknown): number;
	export function structuralDisplay(value: unknown): string;
	export function structuralDebug(value: unknown): string;
	export function nymphTransactionBegin(): void;
	export function nymphTransactionCommit(): void;
	export function nymphTransactionRollback(): void;
	export function nymphArraySplice<T>(
		array: T[],
		start: number,
		deleteCount: number,
		...items: T[]
	): T[];
	export function nymphArrayPush<T>(array: T[], ...items: T[]): number;
	export function nymphArrayPop<T>(array: T[]): T | undefined;
	export function nymphArraySetLength<T>(array: T[], length: number): number;
	export function nymphSetPrototypeOf<T extends object>(object: T, prototype: object): T;
}

declare module "std/option" {
	export namespace Option {
		interface Some<T> {
			readonly [tag: symbol]: unknown;
			readonly value: T;
		}

		interface None {
			readonly [tag: symbol]: unknown;
		}

		const Some: <T>(fields: { value: T }) => Some<T>;
		const None: None;
	}
}
