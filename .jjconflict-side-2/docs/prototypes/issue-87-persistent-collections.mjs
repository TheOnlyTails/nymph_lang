// THROWAWAY PROTOTYPE for issue 87.
// Question: which JavaScript payloads make Nymph list/map/set updates persistent,
// slices share storage, and structural equality/hash lawful without changing uniform boxes?

import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";

const BITS = 5;
const WIDTH = 1 << BITS;
const MASK = WIDTH - 1;
const MISSING = Symbol("missing");
const hashCache = new WeakMap();

function mix32(value) {
	value = Math.imul(value ^ (value >>> 16), 0x21f0aaad);
	value = Math.imul(value ^ (value >>> 15), 0x735a2d97);
	return (value ^ (value >>> 15)) | 0;
}

function hashString(value) {
	let lo = 0x243f6a88;
	let hi = 0x85a308d3;
	for (let index = 0; index < value.length; index++) {
		lo = mix32(lo ^ value.charCodeAt(index));
		hi = mix32(hi + value.charCodeAt(index));
	}
	return [lo, hi];
}

function ordered(seed, hashes) {
	let [lo, hi] = hashString(seed);
	for (const [itemLo, itemHi] of hashes) {
		lo = mix32(lo ^ itemLo);
		hi = mix32(hi ^ itemHi ^ lo);
	}
	return [lo, hi];
}

function unordered(seed, hashes) {
	let sumLo = 0;
	let sumHi = 0;
	let xorLo = 0;
	let xorHi = 0;
	let count = 0;
	for (const [itemLo, itemHi] of hashes) {
		const lo = mix32(itemLo ^ 0x9e3779b9);
		const hi = mix32(itemHi ^ lo);
		sumLo = (sumLo + lo) | 0;
		sumHi = (sumHi + hi) | 0;
		xorLo ^= lo;
		xorHi ^= hi;
		count++;
	}
	return ordered(seed, [
		[sumLo, sumHi],
		[xorLo, xorHi],
		[count, mix32(count)],
	]);
}

function hashBigInt(value) {
	const negative = value < 0n;
	let magnitude = negative ? -value : value;
	let lo = negative ? 0x4f1bbcdc : 0x6a09e667;
	let hi = 0xbb67ae85;
	do {
		lo = mix32(lo ^ Number(magnitude & 0xffffffffn));
		hi = mix32(hi ^ lo);
		magnitude >>= 32n;
	} while (magnitude !== 0n);
	return [lo, hi];
}

function nint(value) {
	return Object.freeze({ kind: "int", value: BigInt(value) });
}

function nuint(value) {
	return Object.freeze({ kind: "uint", value: BigInt(value) });
}

function struct(type, fields) {
	return Object.freeze({ kind: "struct", type, fields: Object.freeze({ ...fields }) });
}

function embedded(sourceVariant, fields = {}) {
	return Object.freeze({
		kind: "variant",
		sourceVariant,
		fields: Object.freeze({ ...fields }),
	});
}

function numericEquals(left, right) {
	if (left.value !== right.value) return false;
	return left.kind === right.kind || left.value >= 0n;
}

function equals(left, right) {
	if (left === right) return true;
	if (left == null || right == null || typeof left !== "object" || typeof right !== "object") {
		return left === right;
	}
	if (
		(left.kind === "int" || left.kind === "uint") &&
		(right.kind === "int" || right.kind === "uint")
	) {
		return numericEquals(left, right);
	}
	if (isList(left) && isList(right)) {
		if (left.length !== right.length) return false;
		for (let index = 0; index < left.length; index++)
			if (!equals(left.get(index), right.get(index))) return false;
		return true;
	}
	if (isMap(left) && isMap(right)) {
		if (left.size !== right.size) return false;
		for (const [key, value] of left) {
			const candidate = right.get(key, MISSING);
			if (candidate === MISSING || !equals(value, candidate)) return false;
		}
		return true;
	}
	if (left.kind === "set" && right.kind === "set") {
		if (left.size !== right.size) return false;
		for (const value of left) if (!right.has(value)) return false;
		return true;
	}
	if (left.kind === "struct" && right.kind === "struct") {
		if (left.type !== right.type) return false;
		return fieldsEqual(left.fields, right.fields);
	}
	if (left.kind === "variant" && right.kind === "variant") {
		return left.sourceVariant === right.sourceVariant && fieldsEqual(left.fields, right.fields);
	}
	return false;
}

function fieldsEqual(left, right) {
	const leftKeys = Object.keys(left).sort();
	const rightKeys = Object.keys(right).sort();
	return (
		leftKeys.length === rightKeys.length &&
		leftKeys.every((key, index) => key === rightKeys[index] && equals(left[key], right[key]))
	);
}

function hashPair(value) {
	if (value !== null && typeof value === "object") {
		const cached = hashCache.get(value);
		if (cached !== undefined) return cached;
	}
	let result;
	if (value == null || typeof value !== "object")
		result = hashString(`${typeof value}:${String(value)}`);
	else if (value.kind === "int" || value.kind === "uint")
		result = ordered("integer", [hashBigInt(value.value)]);
	else if (isList(value)) result = ordered("list", Array.from(value, hashPair));
	else if (isMap(value))
		result = unordered(
			"map",
			Array.from(value, ([key, item]) => ordered("entry", [hashPair(key), hashPair(item)])),
		);
	else if (value.kind === "set") result = unordered("set", Array.from(value, hashPair));
	else if (value.kind === "struct") result = hashFields(`struct:${value.type}`, value.fields);
	else if (value.kind === "variant")
		result = hashFields(`variant:${value.sourceVariant}`, value.fields);
	else throw new TypeError(`value is not structurally hashable: ${String(value.kind)}`);
	if (value !== null && typeof value === "object") hashCache.set(value, result);
	return result;
}

function hashFields(seed, fields) {
	return ordered(
		seed,
		Object.keys(fields)
			.sort()
			.map((key) => ordered("field", [hashString(key), hashPair(fields[key])])),
	);
}

function hash(value) {
	const [lo, hi] = hashPair(value);
	return BigInt.asIntN(64, (BigInt(hi >>> 0) << 32n) | BigInt(lo >>> 0));
}

function trieHash(value) {
	const [lo, hi] = hashPair(value);
	return mix32(lo ^ hi) >>> 0;
}

class CopyList {
	constructor(items = []) {
		this.items = Object.freeze([...items]);
		Object.freeze(this);
	}
	get length() {
		return this.items.length;
	}
	get(index) {
		return this.items[index];
	}
	set(index, value) {
		const items = this.items.with(index, value);
		return new CopyList(items);
	}
	append(value) {
		return new CopyList([...this.items, value]);
	}
	slice(start, end) {
		return new CopyList(this.items.slice(start, end));
	}
	*[Symbol.iterator]() {
		yield* this.items;
	}
}

function tailOffset(count) {
	return count < WIDTH ? 0 : ((count - 1) >>> BITS) << BITS;
}

function newPath(level, node) {
	if (level === 0) return node;
	const result = [];
	result[0] = newPath(level - BITS, node);
	return result;
}

function pushTail(level, parent, tailNode, count) {
	const result = parent.slice();
	const index = ((count - 1) >>> level) & MASK;
	result[index] =
		level === BITS ? tailNode : pushTail(level - BITS, parent[index] ?? [], tailNode, count);
	return result;
}

function assocNode(level, node, index, value) {
	const result = node.slice();
	if (level === 0) result[index & MASK] = value;
	else {
		const child = (index >>> level) & MASK;
		result[child] = assocNode(level - BITS, node[child], index, value);
	}
	return result;
}

class TrieList {
	constructor(count = 0, shift = BITS, root = [], tail = []) {
		this.count = count;
		this.shift = shift;
		this.root = root;
		this.tail = tail;
		Object.freeze(this);
	}
	get length() {
		return this.count;
	}
	get(index) {
		if (index < 0 || index >= this.count) return undefined;
		if (index >= tailOffset(this.count)) return this.tail[index & MASK];
		let node = this.root;
		for (let level = this.shift; level > 0; level -= BITS) node = node[(index >>> level) & MASK];
		return node[index & MASK];
	}
	set(index, value) {
		if (index < 0 || index >= this.count) throw new RangeError("index out of bounds");
		if (index >= tailOffset(this.count)) {
			const tail = this.tail.slice();
			tail[index & MASK] = value;
			return new TrieList(this.count, this.shift, this.root, tail);
		}
		return new TrieList(
			this.count,
			this.shift,
			assocNode(this.shift, this.root, index, value),
			this.tail,
		);
	}
	append(value) {
		if (this.tail.length < WIDTH)
			return new TrieList(this.count + 1, this.shift, this.root, [...this.tail, value]);
		let shift = this.shift;
		let root;
		if (this.count >>> BITS > 1 << this.shift) {
			root = [this.root, newPath(this.shift, this.tail)];
			shift += BITS;
		} else root = pushTail(this.shift, this.root, this.tail, this.count);
		return new TrieList(this.count + 1, shift, root, [value]);
	}
	slice(start, end) {
		return new ListSlice(this, start, end);
	}
	*[Symbol.iterator]() {
		for (let index = 0; index < this.count; index++) yield this.get(index);
	}
}

class ListSlice {
	constructor(base, start, end) {
		this.base = base instanceof ListSlice ? base.base : base;
		const baseStart = base instanceof ListSlice ? base.start : 0;
		const length = base instanceof ListSlice ? base.length : base.length;
		this.start = baseStart + Math.max(0, Math.min(length, start));
		this.end = baseStart + Math.max(0, Math.min(length, end));
		if (this.end < this.start) this.end = this.start;
		Object.freeze(this);
	}
	get length() {
		return this.end - this.start;
	}
	get(index) {
		return index < 0 || index >= this.length ? undefined : this.base.get(this.start + index);
	}
	set(index, value) {
		return new ListSlice(this.base.set(this.start + index, value), this.start, this.end);
	}
	append(value) {
		let result = TrieList.empty;
		for (const item of this) result = result.append(item);
		return result.append(value);
	}
	slice(start, end) {
		return new ListSlice(this, start, end);
	}
	*[Symbol.iterator]() {
		for (let index = 0; index < this.length; index++) yield this.get(index);
	}
}
TrieList.empty = new TrieList();

class CopyMap {
	constructor(buckets = new Map(), size = 0) {
		this.buckets = buckets;
		this.size = size;
		Object.freeze(this);
	}
	get(key, fallback = undefined) {
		for (const [candidate, value] of this.buckets.get(trieHash(key)) ?? [])
			if (equals(candidate, key)) return value;
		return fallback;
	}
	has(key) {
		return this.get(key, MISSING) !== MISSING;
	}
	set(key, value) {
		const keyHash = trieHash(key);
		const bucket = [...(this.buckets.get(keyHash) ?? [])];
		const index = bucket.findIndex(([candidate]) => equals(candidate, key));
		if (index < 0) bucket.push([key, value]);
		else bucket[index] = [key, value];
		const buckets = new Map(this.buckets);
		buckets.set(keyHash, bucket);
		return new CopyMap(buckets, this.size + (index < 0 ? 1 : 0));
	}
	delete(key) {
		const keyHash = trieHash(key);
		const bucket = [...(this.buckets.get(keyHash) ?? [])];
		const index = bucket.findIndex(([candidate]) => equals(candidate, key));
		if (index < 0) return this;
		bucket.splice(index, 1);
		const buckets = new Map(this.buckets);
		if (bucket.length === 0) buckets.delete(keyHash);
		else buckets.set(keyHash, bucket);
		return new CopyMap(buckets, this.size - 1);
	}
	*[Symbol.iterator]() {
		for (const bucket of this.buckets.values()) yield* bucket;
	}
}

function mergeNodes(left, leftHash, right, rightHash, shift) {
	const leftIndex = (leftHash >>> shift) & MASK;
	const rightIndex = (rightHash >>> shift) & MASK;
	const leftBit = 1 << leftIndex;
	const rightBit = 1 << rightIndex;
	if (leftBit === rightBit)
		return {
			kind: "branch",
			bitmap: leftBit,
			children: [mergeNodes(left, leftHash, right, rightHash, shift + BITS)],
		};
	return {
		kind: "branch",
		bitmap: leftBit | rightBit,
		children: leftIndex < rightIndex ? [left, right] : [right, left],
	};
}

function popcount(value) {
	value -= (value >>> 1) & 0x55555555;
	value = (value & 0x33333333) + ((value >>> 2) & 0x33333333);
	return (((value + (value >>> 4)) & 0x0f0f0f0f) * 0x01010101) >>> 24;
}

function hamtGet(node, keyHash, key, shift) {
	if (node == null) return MISSING;
	if (node.kind === "leaf")
		return node.hash === keyHash && equals(node.key, key) ? node.value : MISSING;
	if (node.kind === "collision") {
		if (node.hash !== keyHash) return MISSING;
		const entry = node.entries.find(([candidate]) => equals(candidate, key));
		return entry === undefined ? MISSING : entry[1];
	}
	const bit = 1 << ((keyHash >>> shift) & MASK);
	if ((node.bitmap & bit) === 0) return MISSING;
	return hamtGet(node.children[popcount(node.bitmap & (bit - 1))], keyHash, key, shift + BITS);
}

function hamtSet(node, keyHash, key, value, shift) {
	if (node == null) return [{ kind: "leaf", hash: keyHash, key, value }, true];
	if (node.kind === "leaf") {
		if (node.hash === keyHash && equals(node.key, key)) return [{ ...node, key, value }, false];
		if (node.hash === keyHash)
			return [
				{
					kind: "collision",
					hash: keyHash,
					entries: [
						[node.key, node.value],
						[key, value],
					],
				},
				true,
			];
		return [
			mergeNodes(node, node.hash, { kind: "leaf", hash: keyHash, key, value }, keyHash, shift),
			true,
		];
	}
	if (node.kind === "collision") {
		if (node.hash !== keyHash)
			return [
				mergeNodes(node, node.hash, { kind: "leaf", hash: keyHash, key, value }, keyHash, shift),
				true,
			];
		const entries = node.entries.slice();
		const index = entries.findIndex(([candidate]) => equals(candidate, key));
		if (index < 0) entries.push([key, value]);
		else entries[index] = [key, value];
		return [{ ...node, entries }, index < 0];
	}
	const bit = 1 << ((keyHash >>> shift) & MASK);
	const index = popcount(node.bitmap & (bit - 1));
	const children = node.children.slice();
	if ((node.bitmap & bit) === 0) {
		children.splice(index, 0, { kind: "leaf", hash: keyHash, key, value });
		return [{ ...node, bitmap: node.bitmap | bit, children }, true];
	}
	const [child, inserted] = hamtSet(children[index], keyHash, key, value, shift + BITS);
	children[index] = child;
	return [{ ...node, children }, inserted];
}

class HamtMap {
	constructor(root = null, size = 0) {
		this.root = root;
		this.size = size;
		Object.freeze(this);
	}
	get(key, fallback = undefined) {
		const value = hamtGet(this.root, trieHash(key), key, 0);
		return value === MISSING ? fallback : value;
	}
	has(key) {
		return hamtGet(this.root, trieHash(key), key, 0) !== MISSING;
	}
	set(key, value) {
		const [root, inserted] = hamtSet(this.root, trieHash(key), key, value, 0);
		return new HamtMap(root, this.size + (inserted ? 1 : 0));
	}
	*[Symbol.iterator]() {
		yield* hamtEntries(this.root);
	}
}

function* hamtEntries(node) {
	if (node == null) return;
	if (node.kind === "leaf") yield [node.key, node.value];
	else if (node.kind === "collision") yield* node.entries;
	else for (const child of node.children) yield* hamtEntries(child);
}

class PersistentSet {
	constructor(map = new HamtMap()) {
		this.kind = "set";
		this.map = map;
		Object.freeze(this);
	}
	get size() {
		return this.map.size;
	}
	has(value) {
		return this.map.has(value);
	}
	add(value) {
		return new PersistentSet(this.map.set(value, true));
	}
	*[Symbol.iterator]() {
		for (const [value] of this.map) yield value;
	}
}

function isList(value) {
	return value instanceof CopyList || value instanceof TrieList || value instanceof ListSlice;
}
function isMap(value) {
	return value instanceof CopyMap || value instanceof HamtMap;
}

function correctnessEvidence() {
	let trie = TrieList.empty;
	let copy = new CopyList();
	for (let index = 0; index < 2050; index++) {
		const value = nint(index);
		trie = trie.append(value);
		copy = copy.append(value);
	}
	assert.ok(equals(trie, copy));
	const changed = trie.set(1024, nint(99));
	assert.equal(trie.get(1024).value, 1024n);
	assert.equal(changed.get(1024).value, 99n);
	assert.equal(trie.slice(100, 200).slice(10, 20).base, trie);

	let map = new HamtMap();
	const keyA = struct("Key", { public: nint(1), "#hidden": nint(2) });
	const keyB = struct("Key", { "#hidden": nint(2), public: nuint(1) });
	map = map.set(keyA, "first");
	const map2 = map.set(keyB, "replaced");
	assert.equal(map.get(keyA), "first");
	assert.equal(map2.size, 1);
	assert.equal(map2.get(keyA), "replaced");
	assert.equal(hash(keyA), hash(keyB));
	assert.notEqual(hash(keyA), hash(struct("Key", { public: nint(1), "#hidden": nint(3) })));

	const left = new HamtMap().set(nint(1), "one").set(nint(2), "two");
	const right = new HamtMap().set(nuint(2), "two").set(nuint(1), "one");
	assert.ok(equals(left, right));
	assert.equal(hash(left), hash(right));
	assert.equal(hash(nint(42)), hash(nuint(42)));
	assert.ok(equals(embedded("Foo.A"), embedded("Foo.A")));
	assert.equal(hash(embedded("Foo.A")), hash(embedded("Foo.A")));

	let set = new PersistentSet().add(keyA);
	const oldSet = set;
	set = set.add(keyB).add(struct("Key", { public: nint(2), "#hidden": nint(2) }));
	assert.equal(oldSet.size, 1);
	assert.equal(set.size, 2);
	return 15;
}

function benchmark(name, iterations, operation) {
	for (let index = 0; index < Math.min(iterations, 100); index++) operation(index);
	const samples = [];
	for (let round = 0; round < 7; round++) {
		const start = performance.now();
		for (let index = 0; index < iterations; index++) operation(index);
		samples.push(((performance.now() - start) * 1e6) / iterations);
	}
	samples.sort((left, right) => left - right);
	return { operation: name, "median ns/op": Math.round(samples[3] * 10) / 10 };
}

function buildList(List, count) {
	let list = List === TrieList ? TrieList.empty : new CopyList();
	for (let index = 0; index < count; index++) list = list.append(nint(index));
	return list;
}

function buildMap(MapType, count) {
	let map = new MapType();
	for (let index = 0; index < count; index++) map = map.set(nint(index), nint(index));
	return map;
}

function performanceEvidence() {
	const count = 4096;
	const copyList = buildList(CopyList, count);
	const trieList = buildList(TrieList, count);
	const copyMap = buildMap(CopyMap, count);
	const hamtMap = buildMap(HamtMap, count);
	const rows = [
		benchmark("list build: frozen array copy", 20, () => buildList(CopyList, count)),
		benchmark("list build: vector trie", 20, () => buildList(TrieList, count)),
		benchmark("list update: frozen array copy", 2000, (i) =>
			copyList.set(i & (count - 1), nint(i)),
		),
		benchmark("list update: vector trie", 2000, (i) => trieList.set(i & (count - 1), nint(i))),
		benchmark("list slice: array copy", 5000, (i) => copyList.slice(i & 1023, (i & 1023) + 2048)),
		benchmark("list slice: shared view", 5000, (i) => trieList.slice(i & 1023, (i & 1023) + 2048)),
		benchmark("map update: copied buckets", 500, (i) =>
			copyMap.set(nint(i & (count - 1)), nint(i)),
		),
		benchmark("map update: HAMT", 5000, (i) => hamtMap.set(nint(i & (count - 1)), nint(i))),
		benchmark("map lookup: copied buckets", 10000, (i) => copyMap.get(nint(i & (count - 1)))),
		benchmark("map lookup: HAMT", 10000, (i) => hamtMap.get(nint(i & (count - 1)))),
		benchmark("list equality: equal distinct", 100, () => equals(copyList, trieList)),
		benchmark("list hash: uncached view", 100, () => hash(new ListSlice(trieList, 0, count))),
		benchmark("list hash: cached", 10000, () => hash(trieList)),
	];
	console.table(rows);
}

console.log(`correctness scenarios passed: ${correctnessEvidence()}`);
performanceEvidence();
