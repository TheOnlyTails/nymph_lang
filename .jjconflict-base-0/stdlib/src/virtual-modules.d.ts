declare module "std/box" {
	export interface NymphOption<T> {
		readonly [tag: symbol]: unknown;
		readonly value?: T;
	}

	export type NymphTaskOutcome<T> =
		| { readonly tag: "produced"; readonly value: T }
		| { readonly tag: "cancelled" }
		| { readonly tag: "defected"; readonly defect: unknown };

	export interface NymphTask<T> {}

	export interface NymphHandle<T> {}

	export class NBox<T> {
		constructor(value: T);
		v: T;
	}

	export class NInt extends NBox<bigint> {
		constructor(value: bigint | number);
	}
	export class NUint extends NBox<bigint> {
		constructor(value: bigint | number);
	}
	export class NFloat extends NBox<number> {}
	export class NChar extends NBox<string> {}
	export class NBool extends NBox<boolean> {}
	export class NString extends NBox<string> {}

	export interface NymphPersistentVector<T> extends Iterable<T> {
		readonly length: number;
		get(index: number): T | undefined;
		map<U>(callback: (item: T, index: number) => U): U[];
		join(separator?: string): string;
		readonly [index: number]: T;
	}

	export class NList<T = unknown> extends NBox<NymphPersistentVector<T>> {
		constructor(items: Iterable<T>);
		index(key: NInt | NUint): T;
		appended(item: T): NList<T>;
		replaced(key: NUint | bigint, item: T): NList<T>;
		slice(start: NUint | bigint, end: NUint | bigint): NList<T>;
	}

	export class NTuple<T = unknown> extends NBox<T[]> {
		index(key: NInt | NUint): T;
		readonly 0: T;
		readonly 1: T;
	}

	export class NMap<K = unknown, V = unknown> extends NBox<unknown> {
		constructor(entries?: Iterable<readonly [K, V]>);
		readonly size: number;
		get(key: K): V | undefined;
		has(key: K): boolean;
		with(key: K, value: V): NMap<K, V>;
		without(key: K): NMap<K, V>;
		keys(): IterableIterator<K>;
		values(): IterableIterator<V>;
		entries(): IterableIterator<[K, V]>;
		[Symbol.iterator](): IterableIterator<[K, V]>;
	}

	export function nymphStructuralValue<T>(value: T, identity: string, fields: string[]): T;
	export function nymphProtocolDisplay(value: unknown): NString;
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
	export function nymphType(base: object, args: object[]): object;
	export function nymphTypeProjection(receiver: object, path: number[]): object;
	export function nymphSetPrototypeOf<T extends object>(object: T, prototype: object): T;
	export function nymphHostIndex(value: bigint): number;
	export function nymphFloatToInteger(value: number, unsigned: boolean): bigint;
	export function nymphIntegerToFloat(value: bigint | number): number;
	export function nymphCheckedDivide(left: bigint | number, right: bigint | number): number;
	export function nymphCharCode(value: bigint): number;
	export function nymphCheckedShift(value: bigint, count: bigint, left: boolean): bigint;
	export function nymphCheckedPower(value: bigint, exponent: bigint): bigint;
	export function nymphTrustedInt(value: unknown): bigint;
	export function nymphTrustedUInt(value: unknown): bigint;
	export function nymphActivate(
		callable: (...args: unknown[]) => unknown,
		receiver: unknown,
		args: ArrayLike<unknown>,
		source: number,
	): unknown;
	export function nymphCallable<T extends (...args: never[]) => unknown>(step: T): T;
	export function nymphMarkCallable<T extends (...args: never[]) => unknown>(callable: T): T;
	export function nymphMethodStep(
		receiver: Record<string, (...args: unknown[]) => unknown>,
		member: string,
		args: ArrayLike<unknown>,
		step: (frame: unknown) => unknown,
	): unknown;
	export function nymphPush(
		callable: (...args: unknown[]) => unknown,
		receiver: unknown,
		args: unknown[],
		source: number,
		resumeState: number,
		resultSlot: number,
	): unknown;
	export function nymphRegisterCleanup(cleanup: () => void): void;
	export function nymphEnterCleanupScope(): void;
	export function nymphLeaveCleanupScope(): void;
	export function nymphUnwindCleanupScopes(targetDepth: number): void;
	export function nymphTailCall(
		callable: (...args: unknown[]) => unknown,
		receiver: unknown,
		args: unknown[],
		source: number,
	): unknown;
	export function nymphTailCallMember(
		receiver: Record<string, (...args: unknown[]) => unknown>,
		member: string,
		args: unknown[],
		source: number,
	): unknown;
	export function nymphReturn(value: unknown): unknown;
	export function nymphSuspend(effect: unknown, resumeState: number, resultSlot: number): unknown;
	export function nymphDefect(defect: unknown): unknown;
	export function nymphResume(value: unknown, resumeState: number, resultSlot: number): unknown;
	export function nymphTaskRecipe<T>(
		callable: (...args: never[]) => unknown,
		nested: boolean,
	): NymphTask<T>;
	export function nymphTaskDrive<T>(task: NymphTask<T>): Promise<T>;
	export function nymphTaskSpawn<T>(task: NymphTask<T>): NymphHandle<T>;
	export function nymphHandleObserve<T>(handle: NymphHandle<T>): Promise<NymphTaskOutcome<T>>;
	export function nymphHandleCancel<T>(handle: NymphHandle<T>): void;
	export function nymphCheckpoint(): void;
	export function nymphTaskSelect<T>(handles: NymphHandle<T>[]): NymphTask<unknown>;
	export function nymphTaskRace<T>(tasks: NymphTask<T>[]): NymphTask<NymphTaskOutcome<T>>;
	export function nymphStartRoot<T>(
		main: () => T | NymphTask<T>,
		taskRoot: boolean,
	): { cancel(): void; outcome: Promise<NymphTaskOutcome<T>> };
	export function nymphRenderDefect(defect: unknown): string;
	export function nymphRunTask<T>(task: NymphTask<T>): Promise<T>;
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
