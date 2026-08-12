#!/usr/bin/env node

import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";

const Done = Object.freeze({ tag: "done" });
const yielded = (item, next) => Object.freeze({ tag: "yield", item, next });

// Option A: every adapter keeps its concrete successor type. This models
// `next(): Option<(Item, self)>`; callers must statically know that `self`.
const concreteRange = (at, end) =>
	Object.freeze({
		step() {
			return at < end ? [at, concreteRange(at + 1, end)] : null;
		},
	});

const concreteMap = (source, transform) =>
	Object.freeze({
		step() {
			const step = source.step();
			return step === null ? null : [transform(step[0]), concreteMap(step[1], transform)];
		},
	});

const concreteFilter = (source, predicate) =>
	Object.freeze({
		step() {
			let current = source;
			for (;;) {
				const step = current.step();
				if (step === null) return null;
				if (predicate(step[0])) return [step[0], concreteFilter(step[1], predicate)];
				current = step[1];
			}
		},
	});

// Option B: `next` returns a nominal step whose `self` successor keeps the
// receiver's static iterator capabilities.
const erasedRange = (at, end) =>
	Object.freeze({
		next() {
			return at < end ? yielded(at, erasedRange(at + 1, end)) : Done;
		},
	});

const exactRange = (at, end) =>
	Object.freeze({
		remaining() {
			return end - at;
		},
		next() {
			return at < end ? yielded(at, exactRange(at + 1, end)) : Done;
		},
	});

const exactMap = (source, transform) =>
	Object.freeze({
		remaining() {
			return source.remaining();
		},
		next() {
			const step = source.next();
			return step.tag === "done"
				? Done
				: yielded(transform(step.item), exactMap(step.next, transform));
		},
	});

const erasedMap = (source, transform) =>
	Object.freeze({
		next() {
			const step = source.next();
			return step.tag === "done"
				? Done
				: yielded(transform(step.item), erasedMap(step.next, transform));
		},
	});

const erasedFilter = (source, predicate) =>
	Object.freeze({
		next() {
			let current = source;
			for (;;) {
				const step = current.next();
				if (step.tag === "done") return Done;
				if (predicate(step.item)) return yielded(step.item, erasedFilter(step.next, predicate));
				current = step.next;
			}
		},
	});

const erasedTake = (source, remaining) =>
	Object.freeze({
		next() {
			if (remaining === 0) return Done;
			const step = source.next();
			return step.tag === "done" ? Done : yielded(step.item, erasedTake(step.next, remaining - 1));
		},
	});

const collectConcrete = (source) => {
	const result = [];
	let current = source;
	for (;;) {
		const step = current.step();
		if (step === null) return result;
		result.push(step[0]);
		current = step[1];
	}
};

const collectErased = (source) => {
	const result = [];
	let current = source;
	for (;;) {
		const step = current.next();
		if (step.tag === "done") return result;
		result.push(step.item);
		current = step.next;
	}
};

// Option C: a visitor/fold encoding fuses traversal and terminals, but has no
// successor value to retain, branch, or resume after an early exit.
const traversalRange = (start, end) => ({
	visit(visitor) {
		for (let item = start; item < end; item += 1) {
			if (visitor(item) === false) return false;
		}
		return true;
	},
});

const traversalMap = (source, transform) => ({
	visit(visitor) {
		return source.visit((item) => visitor(transform(item)));
	},
});

const traversalFilter = (source, predicate) => ({
	visit(visitor) {
		return source.visit((item) => (predicate(item) ? visitor(item) : true));
	},
});

const collectTraversal = (source) => {
	const result = [];
	source.visit((item) => void result.push(item));
	return result;
};

const firstErased = (source) => {
	const step = source.next();
	return step.tag === "done" ? null : step;
};

const testPersistenceAndOrder = () => {
	const effects = [];
	const base = erasedRange(0, 6);
	const pipeline = erasedTake(
		erasedFilter(
			erasedMap(base, (item) => {
				effects.push(`map:${item}`);
				return item * 2;
			}),
			(item) => {
				effects.push(`filter:${item}`);
				return item % 4 === 0;
			},
		),
		2,
	);

	assert.deepEqual(effects, [], "adapter creation must be pure");
	const first = firstErased(pipeline);
	assert.equal(first.item, 0);
	assert.deepEqual(effects, ["map:0", "filter:0"]);
	assert.deepEqual(collectErased(first.next), [4]);
	assert.deepEqual(effects, ["map:0", "filter:0", "map:1", "filter:2", "map:2", "filter:4"]);

	assert.deepEqual(collectErased(base), [0, 1, 2, 3, 4, 5]);
	assert.deepEqual(collectErased(base), [0, 1, 2, 3, 4, 5]);
	assert.deepEqual(collectErased(first.next), [4], "saved successor must be reusable");
};

const testExactSizeCapability = () => {
	const iterator = exactMap(exactRange(2, 5), (item) => item * 10);
	assert.equal(iterator.remaining(), 3);
	const step = iterator.next();
	assert.equal(step.item, 20);
	assert.equal(step.next.remaining(), 2, "successor must retain exact-size capability");
	assert.deepEqual(collectErased(step.next), [30, 40]);
};

const Transfer = Object.freeze({
	next: (state) => ({ tag: "next", state }),
	continue: (state) => ({ tag: "continue", state }),
	break: (value) => ({ tag: "break", value }),
	return: (value) => ({ tag: "return", value }),
	question: (error) => ({ tag: "question", error }),
	panic: (defect) => ({ tag: "panic", defect }),
	cancel: () => ({ tag: "cancel" }),
});

// A dedicated For HIR operation can route every departure through the same
// activation-owned lexical cleanup path selected by issues #88 and #89.
const runFor = (source, body, trace) => {
	let current = source;
	for (;;) {
		const step = current.next();
		if (step.tag === "done") return { tag: "complete" };

		const cleanups = [];
		const use = (name) => cleanups.push(() => trace.push(`close:${name}`));
		let transfer;
		try {
			transfer = body(step.item, step.next, use);
		} catch (defect) {
			transfer = Transfer.panic(defect.message);
		} finally {
			for (const cleanup of cleanups.reverse()) cleanup();
		}

		switch (transfer.tag) {
			case "next":
			case "continue":
				current = transfer.state;
				break;
			case "break":
			case "return":
			case "question":
			case "panic":
			case "cancel":
				return transfer;
			default:
				throw new Error(`unknown transfer ${transfer.tag}`);
		}
	}
};

const runStateLoop = (initial, body, trace = [], managed = []) => {
	let state = Object.freeze(initial);
	try {
		for (;;) {
			const cleanups = [];
			const use = (name) => cleanups.push(() => trace.push(`close:${name}`));
			let transfer;
			try {
				transfer = body(state, use);
			} catch (defect) {
				transfer = Transfer.panic(defect.message);
			} finally {
				for (const cleanup of cleanups.reverse()) cleanup();
			}

			if (transfer.tag !== "continue") return transfer;
			for (const name of managed.toReversed()) {
				if (name in transfer.state) trace.push(`close:${state[name]}`);
			}
			state = Object.freeze({ ...state, ...transfer.state });
		}
	} finally {
		for (const name of managed.toReversed()) trace.push(`close:${state[name]}`);
	}
};

const testStateLoop = () => {
	const trace = [];
	const captures = [];
	const result = runStateLoop(
		{ left: "a", right: "b", count: 0, file: "managed resource" },
		(state, use) => {
			use(`outer:${state.count}`);
			use(`inner:${state.count}`);
			captures.push(() => state.count);
			if (state.count === 2) return Transfer.break(state.left + state.right);
			const next = {
				left: state.right,
				right: state.left,
				count: state.count + 1,
			};
			if (state.count === 1) next.file = "replacement resource";
			return Transfer.continue(next);
		},
		trace,
		["file"],
	);
	assert.deepEqual(result, Transfer.break("ab"), "continue arguments rebind together");
	assert.deepEqual(
		captures.map((capture) => capture()),
		[0, 1, 2],
		"closures retain one iteration's bindings",
	);
	assert.deepEqual(trace, [
		"close:inner:0",
		"close:outer:0",
		"close:inner:1",
		"close:outer:1",
		"close:managed resource",
		"close:inner:2",
		"close:outer:2",
		"close:replacement resource",
	]);

	const deep = runStateLoop({ count: 0 }, ({ count }) =>
		count === 100_000 ? Transfer.break(count) : Transfer.continue({ count: count + 1 }),
	);
	assert.deepEqual(deep, Transfer.break(100_000), "continue must not grow the host stack");
};

const testForTransfers = () => {
	for (const expected of ["break", "return", "question", "panic", "cancel"]) {
		const trace = [];
		const result = runFor(
			erasedRange(0, 3),
			(item, next, use) => {
				use(`${expected}:outer:${item}`);
				use(`${expected}:inner:${item}`);
				if (item === 0) return Transfer.continue(next);
				if (expected === "panic") throw new Error("boom");
				if (expected === "break") return Transfer.break(item);
				if (expected === "return") return Transfer.return(item);
				if (expected === "question") return Transfer.question("expected error");
				return Transfer.cancel();
			},
			trace,
		);
		assert.equal(result.tag, expected);
		assert.deepEqual(trace, [
			`close:${expected}:inner:0`,
			`close:${expected}:outer:0`,
			`close:${expected}:inner:1`,
			`close:${expected}:outer:1`,
		]);
	}
};

const median = (values) => values.toSorted((a, b) => a - b)[Math.floor(values.length / 2)];

const measure = (name, makeAndCollect, rounds = 9) => {
	const samples = [];
	let checksum = 0;
	for (let round = 0; round < rounds + 2; round += 1) {
		const before = performance.now();
		const values = makeAndCollect();
		const elapsed = performance.now() - before;
		checksum += values.length + values[values.length - 1];
		if (round >= 2) samples.push(elapsed);
	}
	return { name, medianMs: Number(median(samples).toFixed(3)), checksum };
};

const benchmark = () => {
	const size = 100_000;
	return [
		measure("A concrete successor", () =>
			collectConcrete(
				concreteFilter(
					concreteMap(concreteRange(0, size), (x) => x + 1),
					(x) => x % 2 === 0,
				),
			),
		),
		measure("B nominal successor", () =>
			collectErased(
				erasedFilter(
					erasedMap(erasedRange(0, size), (x) => x + 1),
					(x) => x % 2 === 0,
				),
			),
		),
		measure("C visitor/fold", () =>
			collectTraversal(
				traversalFilter(
					traversalMap(traversalRange(0, size), (x) => x + 1),
					(x) => x % 2 === 0,
				),
			),
		),
	];
};

testPersistenceAndOrder();
testExactSizeCapability();
testStateLoop();
testForTransfers();
assert.deepEqual(collectConcrete(concreteMap(concreteRange(0, 4), (x) => x * 3)), [0, 3, 6, 9]);

console.log("issue #98 persistent iterator prototype: all behavioral checks passed");
console.table(benchmark());
console.log("\nContract comparison:");
console.table([
	{
		option: "A concrete successor",
		persistentRemainder: "yes",
		heterogeneousErasure: "needs opaque/associated type",
		terminalFusion: "no",
	},
	{
		option: "B nominal successor",
		persistentRemainder: "yes",
		heterogeneousErasure: "interface view",
		terminalFusion: "no",
	},
	{
		option: "C visitor/fold",
		persistentRemainder: "no",
		heterogeneousErasure: "yes",
		terminalFusion: "yes",
	},
]);
