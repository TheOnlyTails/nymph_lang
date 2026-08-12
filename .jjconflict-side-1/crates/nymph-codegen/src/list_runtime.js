const NYMPH_VECTOR_BITS = 5;
const NYMPH_VECTOR_WIDTH = 1 << NYMPH_VECTOR_BITS;
const NYMPH_VECTOR_MASK = NYMPH_VECTOR_WIDTH - 1;

function nymphVectorTailOffset(count) {
	return count < NYMPH_VECTOR_WIDTH ? 0 : ((count - 1) >>> NYMPH_VECTOR_BITS) << NYMPH_VECTOR_BITS;
}

function nymphVectorFreezeNode(node) {
	return Object.freeze(node);
}

function nymphVectorNewPath(level, node) {
	if (level === 0) return node;
	return nymphVectorFreezeNode([nymphVectorNewPath(level - NYMPH_VECTOR_BITS, node)]);
}

function nymphVectorPushTail(level, parent, tail, count) {
	const result = parent.slice();
	const index = ((count - 1) >>> level) & NYMPH_VECTOR_MASK;
	result[index] =
		level === NYMPH_VECTOR_BITS
			? tail
			: nymphVectorPushTail(
					level - NYMPH_VECTOR_BITS,
					parent[index] ?? nymphVectorFreezeNode([]),
					tail,
					count,
				);
	return nymphVectorFreezeNode(result);
}

function nymphVectorAssoc(level, node, index, value) {
	const result = node.slice();
	if (level === 0) result[index & NYMPH_VECTOR_MASK] = value;
	else {
		const child = (index >>> level) & NYMPH_VECTOR_MASK;
		result[child] = nymphVectorAssoc(level - NYMPH_VECTOR_BITS, node[child], index, value);
	}
	return nymphVectorFreezeNode(result);
}

function nymphVectorFromLeaves(leaves, count, tail) {
	if (leaves.length === 0)
		return new NymphPersistentVector(count, NYMPH_VECTOR_BITS, nymphVectorFreezeNode([]), tail);
	let level = NYMPH_VECTOR_BITS;
	let nodes = leaves;
	while (nodes.length > NYMPH_VECTOR_WIDTH) {
		const parents = [];
		for (let index = 0; index < nodes.length; index += NYMPH_VECTOR_WIDTH)
			parents.push(nymphVectorFreezeNode(nodes.slice(index, index + NYMPH_VECTOR_WIDTH)));
		nodes = parents;
		level += NYMPH_VECTOR_BITS;
	}
	return new NymphPersistentVector(count, level, nymphVectorFreezeNode(nodes), tail);
}

function nymphVectorIndexProperty(property) {
	if (typeof property !== "string" || property === "") return undefined;
	const index = Number(property);
	return Number.isSafeInteger(index) && index >= 0 && String(index) === property
		? index
		: undefined;
}

function nymphListIndex(value) {
	return nymphHostIndex(typeof value === "bigint" ? value : value.v);
}

class NymphPersistentVector {
	constructor(count, shift, root, tail) {
		this._count = count;
		this._shift = shift;
		this._root = root;
		this._tail = tail;
		Object.freeze(this);
		return new Proxy(this, {
			get(target, property, receiver) {
				const index = nymphVectorIndexProperty(property);
				return index === undefined ? Reflect.get(target, property, receiver) : target.get(index);
			},
		});
	}

	static from(iterable) {
		if (iterable instanceof NymphPersistentVector) return iterable;
		const transient = new NymphListTransient();
		for (const item of iterable) transient.append(item);
		return transient.freeze();
	}

	get length() {
		return this._count;
	}

	_leafFor(index) {
		if (index < 0 || index >= this._count) return undefined;
		if (index >= nymphVectorTailOffset(this._count)) return this._tail;
		let node = this._root;
		for (let level = this._shift; level > 0; level -= NYMPH_VECTOR_BITS)
			node = node[(index >>> level) & NYMPH_VECTOR_MASK];
		return node;
	}

	get(index) {
		const leaf = this._leafFor(index);
		return leaf?.[index & NYMPH_VECTOR_MASK];
	}

	append(value) {
		if (this._tail.length < NYMPH_VECTOR_WIDTH)
			return new NymphPersistentVector(
				this._count + 1,
				this._shift,
				this._root,
				nymphVectorFreezeNode([...this._tail, value]),
			);
		let shift = this._shift;
		let root;
		if (this._count >>> NYMPH_VECTOR_BITS > 1 << this._shift) {
			root = nymphVectorFreezeNode([this._root, nymphVectorNewPath(this._shift, this._tail)]);
			shift += NYMPH_VECTOR_BITS;
		} else root = nymphVectorPushTail(this._shift, this._root, this._tail, this._count);
		return new NymphPersistentVector(this._count + 1, shift, root, nymphVectorFreezeNode([value]));
	}

	replace(index, value) {
		if (index < 0 || index >= this._count) throw new RangeError("list index out of bounds");
		if (index >= nymphVectorTailOffset(this._count)) {
			const tail = this._tail.slice();
			tail[index & NYMPH_VECTOR_MASK] = value;
			return new NymphPersistentVector(
				this._count,
				this._shift,
				this._root,
				nymphVectorFreezeNode(tail),
			);
		}
		return new NymphPersistentVector(
			this._count,
			this._shift,
			nymphVectorAssoc(this._shift, this._root, index, value),
			this._tail,
		);
	}

	slice(start = 0, end = this._count) {
		start = Math.max(0, Math.min(this._count, start));
		end = Math.max(start, Math.min(this._count, end));
		const count = end - start;
		if (count === 0) return NymphPersistentVector.from([]);
		const tailStart = nymphVectorTailOffset(count);
		const leaves = [];
		for (let offset = 0; offset < tailStart; offset += NYMPH_VECTOR_WIDTH) {
			const source = start + offset;
			if ((source & NYMPH_VECTOR_MASK) === 0) leaves.push(this._leafFor(source));
			else {
				const leaf = [];
				for (let index = 0; index < NYMPH_VECTOR_WIDTH; index++)
					leaf.push(this.get(source + index));
				leaves.push(nymphVectorFreezeNode(leaf));
			}
		}
		const tail = [];
		for (let offset = tailStart; offset < count; offset++) tail.push(this.get(start + offset));
		return nymphVectorFromLeaves(leaves, count, nymphVectorFreezeNode(tail));
	}

	map(callback) {
		return Array.from(this, callback);
	}

	join(separator) {
		return Array.from(this).join(separator);
	}

	*[Symbol.iterator]() {
		for (let index = 0; index < this._count; index++) yield this.get(index);
	}
}

class NymphListTransient {
	constructor() {
		this.items = [];
		this.frozen = false;
	}

	append(item) {
		if (this.frozen) throw new TypeError("list transient is already frozen");
		this.items.push(item);
	}

	freeze() {
		if (this.frozen) throw new TypeError("list transient is already frozen");
		this.frozen = true;
		const count = this.items.length;
		const tailStart = nymphVectorTailOffset(count);
		const leaves = [];
		for (let index = 0; index < tailStart; index += NYMPH_VECTOR_WIDTH)
			leaves.push(nymphVectorFreezeNode(this.items.slice(index, index + NYMPH_VECTOR_WIDTH)));
		return nymphVectorFromLeaves(leaves, count, nymphVectorFreezeNode(this.items.slice(tailStart)));
	}
}
