const NYMPH_TAG = Symbol.for("nymph.tag");
const NYMPH_STRUCTURAL_SHAPE = Symbol.for("nymph.structural.shape");
const nymphHashCache = new WeakMap();
/* NYMPH_ECHO_REGISTRIES */

function nymphStructuralValue(value, identity, fields) {
	Object.defineProperty(value, NYMPH_STRUCTURAL_SHAPE, {
		value: Object.freeze({ identity, fields: Object.freeze([...fields]) }),
	});
	/* NYMPH_ECHO_REGISTER_STRUCTURAL */
	return value;
}

// One journal shared by every copy of the exact ESM runtime.  Keeping the
// state on globalThis is important: project modules may resolve std/box through
// different module URLs while still participating in one REPL transaction.
const NYMPH_TX_KEY = Symbol.for("nymph.transaction.journal");
const nymphTransactionJournal =
	globalThis[NYMPH_TX_KEY] ?? (globalThis[NYMPH_TX_KEY] = { stack: [], rollingBack: false });
const NYMPH_CLASS_KEY = Symbol.for("nymph.runtime.classes");
const nymphRuntimeClasses =
	globalThis[NYMPH_CLASS_KEY] ?? (globalThis[NYMPH_CLASS_KEY] = new globalThis.Map());

function nymphTransactionBegin() {
	nymphTransactionJournal.stack.push([]);
}
function nymphTransactionCommit() {
	const entries = nymphTransactionJournal.stack.pop();
	if (entries === undefined) throw new Error("no active Nymph transaction");
	const parent = nymphTransactionJournal.stack.at(-1);
	if (parent) parent.push(...entries);
}
function nymphTransactionRollback() {
	const entries = nymphTransactionJournal.stack.pop();
	if (entries === undefined) throw new Error("no active Nymph transaction");
	nymphTransactionJournal.rollingBack = true;
	try {
		for (let i = entries.length - 1; i >= 0; i--) entries[i]();
	} finally {
		nymphTransactionJournal.rollingBack = false;
	}
}
function nymphJournal(undo) {
	if (!nymphTransactionJournal.rollingBack) nymphTransactionJournal.stack.at(-1)?.push(undo);
}
function nymphSetProperty(object, key, value) {
	const descriptor = Object.getOwnPropertyDescriptor(object, key);
	const oldLength = Array.isArray(object) ? object.length : undefined;
	nymphJournal(() => {
		if (descriptor) Object.defineProperty(object, key, descriptor);
		else Reflect.deleteProperty(object, key);
		if (oldLength !== undefined && object.length !== oldLength) object.length = oldLength;
	});
	if (!Reflect.set(object, key, value))
		throw new TypeError(`cannot assign property ${String(key)}`);
	return value;
}
function nymphDeleteProperty(object, key) {
	const descriptor = Object.getOwnPropertyDescriptor(object, key);
	nymphJournal(() => {
		if (descriptor) Object.defineProperty(object, key, descriptor);
	});
	return Reflect.deleteProperty(object, key);
}
function nymphAssign(object, source) {
	for (const key of Reflect.ownKeys(source)) nymphSetProperty(object, key, source[key]);
	return object;
}
function nymphSetPrototypeOf(object, prototype) {
	const old = Object.getPrototypeOf(object);
	nymphJournal(() => Reflect.setPrototypeOf(object, old));
	Object.setPrototypeOf(object, prototype);
	return object;
}
function nymphArraySplice(array, start, deleteCount, ...items) {
	const before = array.slice();
	nymphJournal(() => {
		array.splice(0, array.length);
		array.length = before.length;
		for (const key of Object.keys(before)) array[key] = before[key];
	});
	return array.splice(start, deleteCount, ...items);
}
function nymphArrayPush(array, ...items) {
	nymphArraySplice(array, array.length, 0, ...items);
	return array.length;
}
function nymphArrayPop(array) {
	return array.length ? nymphArraySplice(array, array.length - 1, 1)[0] : undefined;
}
function nymphArraySetLength(array, length) {
	if (length < array.length) nymphArraySplice(array, length, array.length - length);
	else nymphSetProperty(array, "length", length);
	return length;
}
function nymphMapSet(map, key, value) {
	const had = map.has(key),
		old = map.get(key);
	nymphJournal(() => (had ? map.set(key, old) : map.delete(key)));
	map.set(key, value);
	return map;
}
function nymphWeakMapSet(map, key, value) {
	const had = map.has(key),
		old = map.get(key);
	nymphJournal(() => (had ? map.set(key, old) : map.delete(key)));
	map.set(key, value);
	return map;
}
function nymphRuntimeClass(module, name, implementation) {
	const key = `${module}\0${name.replace(/^\$m[^$]+\$/, "")}`;
	const existing = nymphRuntimeClasses.get(key);
	if (existing !== undefined) {
		for (const property of Reflect.ownKeys(implementation.prototype)) {
			if (property !== "constructor")
				nymphSetProperty(existing.prototype, property, implementation.prototype[property]);
		}
		for (const property of Reflect.ownKeys(implementation)) {
			if (!["length", "name", "prototype"].includes(property))
				nymphSetProperty(existing, property, implementation[property]);
		}
		return existing;
	}
	nymphMapSet(nymphRuntimeClasses, key, implementation);
	return implementation;
}
function nymphRuntimeEnum(module, name, implementation) {
	const key = `${module}\0enum:${name.replace(/^\$m[^$]+\$/, "")}`;
	const existing = nymphRuntimeClasses.get(key);
	if (existing !== undefined) {
		for (const property of Reflect.ownKeys(implementation.$nymph$type))
			nymphSetProperty(existing.$nymph$type, property, implementation.$nymph$type[property]);
		for (const property of Reflect.ownKeys(implementation)) {
			if (!(property in existing)) nymphSetProperty(existing, property, implementation[property]);
			else if (
				typeof implementation[property] === "function" &&
				!implementation[property][NYMPH_TAG]
			)
				nymphSetProperty(existing, property, implementation[property]);
		}
		return existing;
	}
	nymphMapSet(nymphRuntimeClasses, key, implementation);
	return implementation;
}

function nymphMix32(value) {
	value = Math.imul(value ^ (value >>> 16), 0x21f0aaad);
	value = Math.imul(value ^ (value >>> 15), 0x735a2d97);
	return (value ^ (value >>> 15)) | 0;
}

function nymphHashString(value) {
	let lo = 0x243f6a88;
	let hi = 0x85a308d3;
	for (let index = 0; index < value.length; index++) {
		lo = nymphMix32(lo ^ value.charCodeAt(index));
		hi = nymphMix32(hi + value.charCodeAt(index));
	}
	return [lo, hi];
}

function nymphHashOrdered(seed, hashes) {
	let [lo, hi] = nymphHashString(seed);
	for (const [itemLo, itemHi] of hashes) {
		lo = nymphMix32(lo ^ itemLo);
		hi = nymphMix32(hi ^ itemHi ^ lo);
	}
	return [lo, hi];
}

function nymphHashUnordered(seed, hashes) {
	let sumLo = 0,
		sumHi = 0,
		xorLo = 0,
		xorHi = 0,
		count = 0;
	for (const [itemLo, itemHi] of hashes) {
		const lo = nymphMix32(itemLo ^ 0x9e3779b9);
		const hi = nymphMix32(itemHi ^ lo);
		sumLo = (sumLo + lo) | 0;
		sumHi = (sumHi + hi) | 0;
		xorLo ^= lo;
		xorHi ^= hi;
		count++;
	}
	return nymphHashOrdered(seed, [
		[sumLo, sumHi],
		[xorLo, xorHi],
		[count, nymphMix32(count)],
	]);
}

function nymphHashInteger(value) {
	const negative = value < 0n;
	let magnitude = negative ? -value : value;
	let lo = negative ? 0x4f1bbcdc : 0x6a09e667;
	let hi = 0xbb67ae85;
	do {
		lo = nymphMix32(lo ^ Number(magnitude & 0xffff_ffffn));
		hi = nymphMix32(hi ^ lo);
		magnitude >>= 32n;
	} while (magnitude !== 0n);
	return nymphHashOrdered("integer", [[lo, hi]]);
}

function nymphPackHash([lo, hi]) {
	return BigInt.asIntN(64, (BigInt(hi >>> 0) << 32n) | BigInt(lo >>> 0));
}

function nymphFoldHash(hash) {
	const bits = BigInt.asUintN(64, hash);
	return Number(BigInt.asIntN(32, bits ^ (bits >> 32n)));
}

function nymphTagName(value) {
	return value?.[NYMPH_TAG]?.description;
}

function nymphIsPayloadBox(value) {
	return [
		"nymph.int",
		"nymph.uint",
		"nymph.float",
		"nymph.char",
		"nymph.bool",
		"nymph.string",
		"nymph.list",
		"nymph.tuple",
	].includes(nymphTagName(value));
}

function nymphDebug(value) {
	if (value === undefined) return "void";
	const tag = nymphTagName(value);
	if (typeof value === "string") return JSON.stringify(value);
	if (typeof value === "number" || typeof value === "bigint" || typeof value === "boolean")
		return String(value);
	if (tag === "nymph.int" || tag === "nymph.uint" || tag === "nymph.bool") return String(value.v);
	if (tag === "nymph.float")
		return Number.isInteger(value.v) ? value.v.toFixed(1) : String(value.v);
	if (tag === "nymph.char") {
		const escaped = JSON.stringify(String(value.v)).slice(1, -1).replaceAll("'", "\\'");
		return `'${escaped}'`;
	}
	if (tag === "nymph.string") return JSON.stringify(value.v);
	if (tag === "nymph.list") return `#[${value.v.map(nymphDebugValue).join(", ")}]`;
	if (tag === "nymph.tuple") return `#(${value.v.map(nymphDebugValue).join(", ")})`;
	if (tag === "nymph.map")
		return `#{${[...value].map(([k, v]) => `${nymphDebugValue(k)}: ${nymphDebugValue(v)}`).join(", ")}}`;
	if (value == null) return String(value);
	const name = (tag ?? value.constructor?.name ?? "Object").split("$").at(-1);
	const fields = Object.keys(value);
	return fields.length === 0
		? name
		: `${name}(${fields.map((key) => `${key}: ${nymphDebugValue(value[key])}`).join(", ")})`;
}

function nymphDebugValue(value) {
	return typeof value?.$nymph$debug === "function" ? value.$nymph$debug().v : nymphDebug(value);
}

function nymphDisplay(value) {
	const tag = nymphTagName(value);
	if (
		typeof value === "string" ||
		typeof value === "number" ||
		typeof value === "bigint" ||
		typeof value === "boolean"
	)
		return String(value);
	if (tag === "nymph.char" || tag === "nymph.string") return value.v;
	return nymphDebugValue(value);
}

function nymphProtocolDisplay(value) {
	return typeof value?.$nymph$display === "function"
		? value.$nymph$display()
		: new NString(nymphDisplay(value));
}

function nymphProtocolDebug(value) {
	return typeof value?.$nymph$debug === "function"
		? value.$nymph$debug()
		: new NString(nymphDebug(value));
}

function nymphEquals(left, right) {
	if (left === right) return true;
	if (left == null || right == null || typeof left !== "object" || typeof right !== "object") {
		return false;
	}
	const leftTag = nymphTagName(left),
		rightTag = nymphTagName(right);
	if (
		(leftTag === "nymph.int" || leftTag === "nymph.uint") &&
		(rightTag === "nymph.int" || rightTag === "nymph.uint")
	)
		return left.v === right.v && (leftTag === rightTag || left.v >= 0n);
	if (left[NYMPH_TAG] !== right[NYMPH_TAG]) return false;
	if (leftTag === "nymph.map") {
		const leftRoot = left.v.root,
			leftSize = left.size,
			rightRoot = right.v.root,
			rightSize = right.size;
		let equal = leftSize === rightSize;
		if (equal) {
			for (const [key, value] of hamtEntries(leftRoot)) {
				if (!right.has(key) || !nymphKeyEquals(value, right.get(key))) {
					equal = false;
					break;
				}
			}
		}
		if (
			left.v.root !== leftRoot ||
			left.size !== leftSize ||
			right.v.root !== rightRoot ||
			right.size !== rightSize
		)
			throw new Error("map callback mutated a map being compared");
		return equal;
	}
	if (
		(leftTag === "nymph.list" && rightTag === "nymph.list") ||
		(Array.isArray(left.v) && Array.isArray(right.v))
	) {
		return (
			left.v.length === right.v.length &&
			Array.from(left.v).every((value, index) => nymphKeyEquals(value, right.v[index]))
		);
	}
	if (nymphIsPayloadBox(left) || nymphIsPayloadBox(right)) return left.v === right.v;
	const leftShape = left[NYMPH_STRUCTURAL_SHAPE],
		rightShape = right[NYMPH_STRUCTURAL_SHAPE];
	return (
		leftShape !== undefined &&
		rightShape !== undefined &&
		leftShape.identity === rightShape.identity &&
		leftShape.fields.length === rightShape.fields.length &&
		leftShape.fields.every(
			(field, index) =>
				field === rightShape.fields[index] && nymphKeyEquals(left[field], right[field]),
		)
	);
}

function nymphHashPair(value) {
	if (value !== null && typeof value === "object") {
		const cached = nymphHashCache.get(value);
		if (Array.isArray(cached)) return cached;
	}
	if (value == null || typeof value !== "object")
		return nymphHashString(`${typeof value}:${String(value)}`);
	const tag = nymphTagName(value);
	if (tag === "nymph.int" || tag === "nymph.uint") return nymphHashInteger(value.v);
	let result;
	if (tag === "nymph.map") {
		const root = value.v.root,
			size = value.size;
		const cached = nymphHashCache.get(value);
		if (cached?.root === root) return cached.hash;
		result = nymphHashUnordered(
			"map",
			Array.from(hamtEntries(root), ([key, entryValue]) =>
				nymphHashOrdered("entry", [nymphKeyHashPair(key), nymphKeyHashPair(entryValue)]),
			),
		);
		if (value.v.root !== root || value.size !== size)
			throw new Error("map callback mutated the map being hashed");
		nymphHashCache.set(value, { root, hash: result });
		return result;
	}
	if (tag === "nymph.list" || Array.isArray(value.v))
		return nymphHashOrdered(tag === "nymph.list" ? "list" : "tuple", value.v.map(nymphKeyHashPair));
	if (tag === "nymph.float") throw new TypeError("float has no lawful structural hash");
	if (nymphIsPayloadBox(value))
		return nymphHashOrdered(tag ?? "payload", [nymphHashString(String(value.v))]);
	const shape = value[NYMPH_STRUCTURAL_SHAPE];
	if (shape === undefined) throw new TypeError("value has no structural hash capability");
	result = nymphHashOrdered(
		`nominal:${shape.identity}`,
		shape.fields.map((field) =>
			nymphHashOrdered("field", [nymphHashString(field), nymphKeyHashPair(value[field])]),
		),
	);
	if (Object.isFrozen(value)) nymphHashCache.set(value, result);
	return result;
}

function nymphHash(value) {
	return nymphPackHash(nymphHashPair(value));
}

function nymphKeyHash(key) {
	return nymphFoldHash(nymphHash(key));
}

function nymphKeyHashPair(key) {
	return nymphHashPair(key);
}

function nymphKeyEquals(left, right) {
	return nymphEquals(left, right);
}

const HAMT_NOT_FOUND = Symbol("nymph.hamt.not_found");

function hamtPopcount(value) {
	value -= (value >>> 1) & 0x55555555;
	value = (value & 0x33333333) + ((value >>> 2) & 0x33333333);
	return (((value + (value >>> 4)) & 0x0f0f0f0f) * 0x01010101) >>> 24;
}

function hamtMerge(left, leftHash, right, rightHash, shift) {
	const leftIndex = (leftHash >>> shift) & 31;
	const rightIndex = (rightHash >>> shift) & 31;
	const leftBit = 1 << leftIndex;
	const rightBit = 1 << rightIndex;
	if (leftBit === rightBit) {
		return {
			kind: "bitmap",
			bitmap: leftBit,
			children: [hamtMerge(left, leftHash, right, rightHash, shift + 5)],
		};
	}
	return {
		kind: "bitmap",
		bitmap: leftBit | rightBit,
		children: leftIndex < rightIndex ? [left, right] : [right, left],
	};
}

function hamtGet(node, hash, key, shift) {
	if (node == null) return HAMT_NOT_FOUND;
	if (node.kind === "leaf")
		return node.hash === hash && nymphKeyEquals(node.key, key) ? node.value : HAMT_NOT_FOUND;
	if (node.kind === "collision") {
		if (node.hash !== hash) return HAMT_NOT_FOUND;
		const entry = node.entries.find(([candidate]) => nymphKeyEquals(candidate, key));
		return entry ? entry[1] : HAMT_NOT_FOUND;
	}
	const bit = 1 << ((hash >>> shift) & 31);
	if ((node.bitmap & bit) === 0) return HAMT_NOT_FOUND;
	const index = hamtPopcount(node.bitmap & (bit - 1));
	return hamtGet(node.children[index], hash, key, shift + 5);
}

function hamtSet(node, hash, key, value, shift) {
	if (node == null) return [{ kind: "leaf", hash, key, value }, true];
	if (node.kind === "leaf") {
		if (node.hash === hash && nymphKeyEquals(node.key, key)) {
			return [{ ...node, value }, false];
		}
		if (node.hash === hash) {
			return [
				{
					kind: "collision",
					hash,
					entries: [
						[node.key, node.value],
						[key, value],
					],
				},
				true,
			];
		}
		return [hamtMerge(node, node.hash, { kind: "leaf", hash, key, value }, hash, shift), true];
	}
	if (node.kind === "collision") {
		if (node.hash !== hash) {
			return [hamtMerge(node, node.hash, { kind: "leaf", hash, key, value }, hash, shift), true];
		}
		const entryIndex = node.entries.findIndex(([candidate]) => nymphKeyEquals(candidate, key));
		if (entryIndex >= 0) {
			const entries = node.entries.slice();
			entries[entryIndex] = [key, value];
			return [{ ...node, entries }, false];
		}
		return [{ ...node, entries: [...node.entries, [key, value]] }, true];
	}
	const bit = 1 << ((hash >>> shift) & 31);
	const index = hamtPopcount(node.bitmap & (bit - 1));
	if ((node.bitmap & bit) === 0) {
		const children = node.children.slice();
		children.splice(index, 0, { kind: "leaf", hash, key, value });
		return [{ ...node, bitmap: node.bitmap | bit, children }, true];
	}
	const [child, inserted] = hamtSet(node.children[index], hash, key, value, shift + 5);
	const children = node.children.slice();
	children[index] = child;
	return [{ ...node, children }, inserted];
}

function hamtDelete(node, hash, key, shift) {
	if (node == null) return [null, undefined, false];
	if (node.kind === "leaf") {
		return node.hash === hash && nymphKeyEquals(node.key, key)
			? [null, node.value, true]
			: [node, undefined, false];
	}
	if (node.kind === "collision") {
		if (node.hash !== hash) return [node, undefined, false];
		const index = node.entries.findIndex(([candidate]) => nymphKeyEquals(candidate, key));
		if (index < 0) return [node, undefined, false];
		const value = node.entries[index][1];
		const entries = node.entries.toSpliced(index, 1);
		if (entries.length === 1) {
			const [[remainingKey, remainingValue]] = entries;
			return [{ kind: "leaf", hash, key: remainingKey, value: remainingValue }, value, true];
		}
		return [{ ...node, entries }, value, true];
	}
	const bit = 1 << ((hash >>> shift) & 31);
	if ((node.bitmap & bit) === 0) return [node, undefined, false];
	const index = hamtPopcount(node.bitmap & (bit - 1));
	const [child, value, removed] = hamtDelete(node.children[index], hash, key, shift + 5);
	if (!removed) return [node, undefined, false];
	if (child == null) {
		const children = node.children.toSpliced(index, 1);
		if (children.length === 0) return [null, value, true];
		if (children.length === 1 && children[0].kind !== "bitmap") return [children[0], value, true];
		return [{ ...node, bitmap: node.bitmap & ~bit, children }, value, true];
	} else {
		const children = node.children.slice();
		children[index] = child;
		return [{ ...node, children }, value, true];
	}
}

function* hamtEntries(node) {
	if (node == null) return;
	if (node.kind === "leaf") {
		yield [node.key, node.value];
		return;
	}
	if (node.kind === "collision") {
		yield* node.entries;
		return;
	}
	for (const child of node.children) yield* hamtEntries(child);
}

class NymphHamt {
	constructor(root = null, size = 0) {
		this.root = root;
		this.size = size;
		Object.freeze(this);
	}
	static from(entries = []) {
		let result = NymphHamt.empty;
		for (const entry of entries) {
			const pair = Array.isArray(entry.v) ? entry.v : entry;
			result = result.set(pair[0], pair[1]);
		}
		return result;
	}
	get(key) {
		const hash = nymphKeyHash(key);
		const value = hamtGet(this.root, hash, key, 0);
		return value === HAMT_NOT_FOUND ? undefined : value;
	}
	has(key) {
		const hash = nymphKeyHash(key);
		return hamtGet(this.root, hash, key, 0) !== HAMT_NOT_FOUND;
	}
	set(key, value) {
		const hash = nymphKeyHash(key);
		const [root, inserted] = hamtSet(this.root, hash, key, value, 0);
		return new NymphHamt(root, this.size + (inserted ? 1 : 0));
	}
	delete(key) {
		const hash = nymphKeyHash(key);
		const [root, value, removed] = hamtDelete(this.root, hash, key, 0);
		return [removed ? new NymphHamt(root, this.size - 1) : this, value, removed];
	}
	*entries() {
		yield* hamtEntries(this.root);
	}
	*keys() {
		for (const [key] of this.entries()) yield key;
	}
	*values() {
		for (const [, value] of this.entries()) yield value;
	}
	[Symbol.iterator]() {
		return this.entries();
	}
}
NymphHamt.empty = new NymphHamt();

class NymphRange {
	constructor({ start, end, inclusive }) {
		this.start = start;
		this.end = end;
		this.inclusive = inclusive;
	}
}
