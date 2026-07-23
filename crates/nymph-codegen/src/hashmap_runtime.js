const NYMPH_TAG = Symbol.for("nymph.tag");

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
		if (left.size !== right.size) return false;
		for (const [key, value] of left) {
			if (!right.has(key) || !nymphEquals(value, right.get(key))) return false;
		}
		return true;
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
		let entriesHash = 0;
		for (const [key, entryValue] of value) {
			entriesHash = (entriesHash + nymphHashCombine(nymphKeyHash(key), nymphHash(entryValue))) | 0;
		}
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
			node.value = value;
			return [node, false];
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
		const entry = node.entries.find(([candidate]) => nymphKeyEquals(candidate, key));
		if (entry) {
			entry[1] = value;
			return [node, false];
		}
		node.entries.push([key, value]);
		return [node, true];
	}
	const bit = 1 << ((hash >>> shift) & 31);
	const index = hamtPopcount(node.bitmap & (bit - 1));
	if ((node.bitmap & bit) === 0) {
		node.bitmap |= bit;
		node.children.splice(index, 0, { kind: "leaf", hash, key, value });
		return [node, true];
	}
	const [child, inserted] = hamtSet(node.children[index], hash, key, value, shift + 5);
	node.children[index] = child;
	return [node, inserted];
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
		const [[, value]] = node.entries.splice(index, 1);
		if (node.entries.length === 1) {
			const [[remainingKey, remainingValue]] = node.entries;
			return [{ kind: "leaf", hash, key: remainingKey, value: remainingValue }, value, true];
		}
		return [node, value, true];
	}
	const bit = 1 << ((hash >>> shift) & 31);
	if ((node.bitmap & bit) === 0) return [node, undefined, false];
	const index = hamtPopcount(node.bitmap & (bit - 1));
	const [child, value, removed] = hamtDelete(node.children[index], hash, key, shift + 5);
	if (!removed) return [node, undefined, false];
	if (child == null) {
		node.bitmap &= ~bit;
		node.children.splice(index, 1);
		if (node.children.length === 0) return [null, value, true];
		if (node.children.length === 1 && node.children[0].kind !== "bitmap")
			return [node.children[0], value, true];
	} else {
		node.children[index] = child;
	}
	return [node, value, true];
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
		const value = hamtGet(this.root, nymphKeyHash(key), key, 0);
		return value === HAMT_NOT_FOUND ? undefined : value;
	}
	has(key) {
		return hamtGet(this.root, nymphKeyHash(key), key, 0) !== HAMT_NOT_FOUND;
	}
	set(key, value) {
		const [root, inserted] = hamtSet(this.root, nymphKeyHash(key), key, value, 0);
		this.root = root;
		if (inserted) this.size++;
		return this;
	}
	delete(key) {
		const [root, , removed] = hamtDelete(this.root, nymphKeyHash(key), key, 0);
		this.root = root;
		if (removed) this.size--;
		return removed;
	}
	clear() {
		this.root = null;
		this.size = 0;
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

const NYMPH_OPTION_SOME = Symbol.for("Option.Some");
const NYMPH_OPTION_NONE = Object.freeze({ [NYMPH_TAG]: Symbol.for("Option.None") });

class NymphListIterator {
	constructor(items) {
		this.items = items;
		this.index = 0;
	}
	next() {
		if (this.index >= this.items.length) return NYMPH_OPTION_NONE;
		return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value: this.items[this.index++] };
	}
}

class NymphMapIterator {
	constructor(entries) {
		this.entries = entries;
	}
	next() {
		const entry = this.entries.next();
		if (entry.done) return NYMPH_OPTION_NONE;
		return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value: new NTuple(entry.value) };
	}
}

class NymphRange {
	constructor({ start, end, inclusive }) {
		this.start = start;
		this.end = end;
		this.inclusive = inclusive;
	}
	iter() {
		let current = this.start;
		const end = this.end.v;
		const inclusive = this.inclusive.v;
		return {
			next() {
				if (inclusive ? current.v > end : current.v >= end) return NYMPH_OPTION_NONE;
				const value = current;
				current = new current.constructor(current.v + 1);
				return { [NYMPH_TAG]: NYMPH_OPTION_SOME, value };
			},
		};
	}
}
