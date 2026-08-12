const NYMPH_TAG = Symbol.for("nymph.tag");

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
function nymphCell(value) {
	return { value };
}
function nymphCellGet(cell) {
	return cell.value;
}
function nymphCellSet(cell, value) {
	nymphSetProperty(cell, "value", value);
	return value;
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

function nymphHashString(value) {
	let hash = 0x811c9dc5;
	for (let i = 0; i < value.length; i++) {
		hash ^= value.charCodeAt(i);
		hash = Math.imul(hash, 0x01000193);
	}
	return hash | 0;
}

function nymphHashCombine(hash, value) {
	return Math.imul(hash ^ value, 0x01000193) | 0;
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
	if (typeof value === "number" || typeof value === "boolean") return String(value);
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
	if (typeof value === "string" || typeof value === "number" || typeof value === "boolean")
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
	if (left[NYMPH_TAG] !== right[NYMPH_TAG]) return false;
	if (nymphTagName(left) === "nymph.map") {
		const leftRoot = left.root,
			leftSize = left.size,
			rightRoot = right.root,
			rightSize = right.size;
		let equal = leftSize === rightSize;
		if (equal) {
			for (const [key, value] of hamtEntries(leftRoot)) {
				if (!right.has(key) || !nymphEquals(value, right.get(key))) {
					equal = false;
					break;
				}
			}
		}
		if (
			left.root !== leftRoot ||
			left.size !== leftSize ||
			right.root !== rightRoot ||
			right.size !== rightSize
		)
			throw new Error("map callback mutated a map being compared");
		return equal;
	}
	if (Array.isArray(left.v) && Array.isArray(right.v)) {
		return (
			left.v.length === right.v.length && left.v.every((value, i) => nymphEquals(value, right.v[i]))
		);
	}
	if (nymphIsPayloadBox(left) || nymphIsPayloadBox(right)) return left.v === right.v;
	if (left[NYMPH_TAG] === undefined && left.constructor !== right.constructor) return false;
	const leftKeys = Object.keys(left).sort();
	const rightKeys = Object.keys(right).sort();
	return (
		leftKeys.length === rightKeys.length &&
		leftKeys.every((key, i) => key === rightKeys[i] && nymphEquals(left[key], right[key]))
	);
}

function nymphHash(value) {
	if (value == null) return 0;
	if (typeof value !== "object") return nymphHashString(`${typeof value}:${String(value)}`);
	let hash = nymphHashString(nymphTagName(value) ?? value.constructor?.name ?? "object");
	if (nymphTagName(value) === "nymph.map") {
		const root = value.root,
			size = value.size;
		let entriesHash = 0;
		for (const [key, entryValue] of hamtEntries(root)) {
			entriesHash = (entriesHash + nymphHashCombine(nymphKeyHash(key), nymphHash(entryValue))) | 0;
		}
		if (value.root !== root || value.size !== size)
			throw new Error("map callback mutated the map being hashed");
		return nymphHashCombine(hash, entriesHash);
	}
	if (Array.isArray(value.v)) {
		for (const item of value.v) hash = nymphHashCombine(hash, nymphHash(item));
		return hash;
	}
	if (nymphIsPayloadBox(value)) return nymphHashCombine(hash, nymphHashString(String(value.v)));
	for (const key of Object.keys(value).sort()) {
		hash = nymphHashCombine(hash, nymphHashString(key));
		hash = nymphHashCombine(hash, nymphHash(value[key]));
	}
	return hash;
}

function nymphKeyHash(key) {
	return typeof key?.hash === "function" ? key.hash().v | 0 : nymphHash(key);
}

function nymphKeyEquals(left, right) {
	return typeof left?.equals === "function" ? left.equals(right).v : nymphEquals(left, right);
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
	constructor(entries = []) {
		this.root = null;
		this.size = 0;
		for (const entry of entries) {
			const pair = Array.isArray(entry.v) ? entry.v : entry;
			this.set(pair[0], pair[1]);
		}
	}
	get(key) {
		const hash = nymphKeyHash(key);
		const root = this.root,
			size = this.size;
		const value = hamtGet(root, hash, key, 0);
		if (this.root !== root || this.size !== size)
			throw new Error("map equality callback mutated the map being queried");
		return value === HAMT_NOT_FOUND ? undefined : value;
	}
	has(key) {
		const hash = nymphKeyHash(key);
		const root = this.root,
			size = this.size;
		const found = hamtGet(root, hash, key, 0) !== HAMT_NOT_FOUND;
		if (this.root !== root || this.size !== size)
			throw new Error("map equality callback mutated the map being queried");
		return found;
	}
	set(key, value) {
		const hash = nymphKeyHash(key);
		const previousRoot = this.root,
			previousSize = this.size;
		const [root, inserted] = hamtSet(previousRoot, hash, key, value, 0);
		if (this.root !== previousRoot || this.size !== previousSize)
			throw new Error("map equality callback mutated the map being updated");
		nymphSetProperty(this, "root", root);
		if (inserted) nymphSetProperty(this, "size", this.size + 1);
		return this;
	}
	delete(key) {
		const hash = nymphKeyHash(key);
		const previousRoot = this.root,
			previousSize = this.size;
		const [root, , removed] = hamtDelete(previousRoot, hash, key, 0);
		if (this.root !== previousRoot || this.size !== previousSize)
			throw new Error("map equality callback mutated the map being updated");
		if (removed) {
			nymphSetProperty(this, "root", root);
			nymphSetProperty(this, "size", this.size - 1);
		}
		return removed;
	}
	clear() {
		nymphSetProperty(this, "root", null);
		nymphSetProperty(this, "size", 0);
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

const NYMPH_OPTION_SOME = Symbol.for(`${NYMPH_OPTION_ENUM_NAME}.Some`);
const NYMPH_OPTION_NONE = Object.freeze({
	[NYMPH_TAG]: Symbol.for(`${NYMPH_OPTION_ENUM_NAME}.None`),
});

class NymphListIterator {
	constructor(items) {
		this.items = items;
		this.index = 0;
	}
	next() {
		if (this.index >= this.items.length) return NYMPH_OPTION_NONE;
		const value = this.items[this.index];
		nymphSetProperty(this, "index", this.index + 1);
		return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value };
	}
}

class NymphMapIterator {
	constructor(entries) {
		this.entries = [...entries];
		this.index = 0;
	}
	next() {
		if (this.index >= this.entries.length) return NYMPH_OPTION_NONE;
		const entry = this.entries[this.index];
		nymphSetProperty(this, "index", this.index + 1);
		return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value: new NTuple(entry) };
	}
}

class NymphRange {
	constructor({ start, end, inclusive }) {
		this.start = start;
		this.end = end;
		this.inclusive = inclusive;
	}
	iter() {
		const cursor = { current: this.start };
		const end = this.end.v;
		const inclusive = this.inclusive.v;
		return {
			next() {
				if (inclusive ? cursor.current.v > end : cursor.current.v >= end) return NYMPH_OPTION_NONE;
				const value = cursor.current;
				nymphSetProperty(cursor, "current", new cursor.current.constructor(cursor.current.v + 1));
				return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value };
			},
		};
	}
}
