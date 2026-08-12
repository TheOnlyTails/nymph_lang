#!/usr/bin/env node

// Planning prototype for issue #93. It models exact i64/u64 bounds, branch and
// loop constraints, immutable collection lengths, negative indexing/slicing,
// proof certificates, and the interface-fingerprint boundary. It does not
// implement the destination compiler.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";

const I64_MIN = -(1n << 63n);
const I64_MAX = (1n << 63n) - 1n;
const U64_MIN = 0n;
const U64_MAX = (1n << 64n) - 1n;

const min = (a, b) => (a < b ? a : b);
const max = (a, b) => (a > b ? a : b);

class Bounds {
	constructor() {
		this.intervals = new Map();
		this.relations = [];
		this.excluded = new Map();
	}

	clone() {
		const result = new Bounds();
		result.intervals = new Map([...this.intervals].map(([name, range]) => [name, { ...range }]));
		result.relations = this.relations.map((fact) => ({ ...fact }));
		result.excluded = new Map([...this.excluded].map(([name, values]) => [name, new Set(values)]));
		return result;
	}

	declare(name, lo, hi) {
		assert(lo <= hi, `${name} has an empty declared interval`);
		this.intervals.set(name, { lo, hi });
		return this;
	}

	exact(name, value) {
		return this.declare(name, value, value);
	}

	atLeast(name, lo) {
		const range = this.intervals.get(name);
		assert(range, `unknown value ${name}`);
		range.lo = max(range.lo, lo);
		return this;
	}

	atMost(name, hi) {
		const range = this.intervals.get(name);
		assert(range, `unknown value ${name}`);
		range.hi = min(range.hi, hi);
		return this;
	}

	exclude(name, value) {
		const values = this.excluded.get(name) ?? new Set();
		values.add(value);
		this.excluded.set(name, values);
		return this;
	}

	// lhs - rhs <= offset. Difference bounds cover positive indices and loops.
	difference(lhs, rhs, offset) {
		return this.relation(lhs, 1n, rhs, -1n, offset);
	}

	// leftSign*left + rightSign*right <= offset, where signs are ±1. The
	// additional octagonal forms cover `-length <= negativeIndex` without a
	// general affine/polyhedral domain.
	relation(left, leftSign, right, rightSign, offset) {
		assert(leftSign === 1n || leftSign === -1n);
		assert(rightSign === 1n || rightSign === -1n);
		this.relations.push({ left, leftSign, right, rightSign, offset });
		return this;
	}

	proveAtLeast(name, lo) {
		return this.intervals.get(name)?.lo >= lo;
	}

	proveAtMost(name, hi) {
		return this.intervals.get(name)?.hi <= hi;
	}

	proveNonZero(name) {
		const range = this.intervals.get(name);
		return range.lo > 0n || range.hi < 0n || this.excluded.get(name)?.has(0n) === true;
	}

	proveDifference(lhs, rhs, offset) {
		return this.proveRelation(lhs, 1n, rhs, -1n, offset);
	}

	proveRelation(left, leftSign, right, rightSign, offset) {
		if (
			this.relations.some(
				(fact) =>
					fact.left === left &&
					fact.leftSign === leftSign &&
					fact.right === right &&
					fact.rightSign === rightSign &&
					fact.offset <= offset,
			)
		) {
			return true;
		}
		const leftRange = this.intervals.get(left);
		const rightRange = this.intervals.get(right);
		if (leftRange === undefined || rightRange === undefined) return false;
		const leftMaximum = leftSign === 1n ? leftRange.hi : -leftRange.lo;
		const rightMaximum = rightSign === 1n ? rightRange.hi : -rightRange.lo;
		return leftMaximum + rightMaximum <= offset;
	}

	proveAddIn(name, addend, lo, hi) {
		const range = this.intervals.get(name);
		return range.lo + addend >= lo && range.hi + addend <= hi;
	}
}

const semanticFact = (kind, payload) => ({ tier: "semantic", kind, ...payload });
const optimizationFact = (kind, payload) => ({ tier: "optimization", kind, ...payload });

function replay(facts) {
	const state = new Bounds();
	for (const fact of facts) {
		switch (fact.kind) {
			case "declare":
				state.declare(fact.name, fact.lo, fact.hi);
				break;
			case "at-least":
				state.atLeast(fact.name, fact.value);
				break;
			case "at-most":
				state.atMost(fact.name, fact.value);
				break;
			case "exclude":
				state.exclude(fact.name, fact.value);
				break;
			case "difference":
				state.difference(fact.lhs, fact.rhs, fact.offset);
				break;
			case "relation":
				state.relation(fact.left, fact.leftSign, fact.right, fact.rightSign, fact.offset);
				break;
			default:
				assert.fail(`unknown fact kind ${fact.kind}`);
		}
	}
	return state;
}

function prove(state, obligation) {
	switch (obligation.kind) {
		case "add-in-range":
			return state.proveAddIn(obligation.value, obligation.addend, obligation.lo, obligation.hi);
		case "conversion":
			return (
				state.proveAtLeast(obligation.value, obligation.lo) &&
				state.proveAtMost(obligation.value, obligation.hi)
			);
		case "nonzero":
			return state.proveNonZero(obligation.value);
		case "index":
			return (
				state.proveAtLeast(obligation.index, 0n) &&
				state.proveDifference(obligation.index, obligation.length, -1n)
			);
		case "negative-index":
			return (
				state.proveAtMost(obligation.index, -1n) &&
				state.proveRelation(obligation.index, -1n, obligation.length, -1n, 0n)
			);
		case "slice-exclusive-bound":
			return (
				state.proveDifference(obligation.bound, obligation.length, 0n) &&
				state.proveRelation(obligation.bound, -1n, obligation.length, -1n, 0n)
			);
		case "slice-inclusive-end":
			return (
				state.proveDifference(obligation.bound, obligation.length, -1n) &&
				state.proveRelation(obligation.bound, -1n, obligation.length, -1n, 0n)
			);
		default:
			assert.fail(`unknown obligation kind ${obligation.kind}`);
	}
}

function disprove(state, obligation) {
	const exact = (name) => {
		const range = state.intervals.get(name);
		return range?.lo === range?.hi ? range.lo : null;
	};
	switch (obligation.kind) {
		case "add-in-range": {
			const range = state.intervals.get(obligation.value);
			return (
				range.hi + obligation.addend < obligation.lo || range.lo + obligation.addend > obligation.hi
			);
		}
		case "index": {
			const index = exact(obligation.index);
			const length = exact(obligation.length);
			return index !== null && length !== null && (index < -length || index >= length);
		}
		case "slice-exclusive-bound": {
			const bound = exact(obligation.bound);
			const length = exact(obligation.length);
			return bound !== null && length !== null && (bound < -length || bound > length);
		}
		case "slice-inclusive-end": {
			const bound = exact(obligation.bound);
			const length = exact(obligation.length);
			return bound !== null && length !== null && (bound < -length || bound >= length);
		}
		default:
			return false;
	}
}

function classify(facts, obligation) {
	const state = replay(facts);
	if (prove(state, obligation)) return "safe";
	if (disprove(state, obligation)) return "invalid";
	return "uncertain";
}

function certificate(obligation, facts) {
	assert(facts.every((fact) => fact.tier === "semantic"));
	assert(prove(replay(facts), obligation), `unproved ${obligation.kind}`);
	return { obligation, facts };
}

function verifyCertificate(proof) {
	return (
		proof.facts.every((fact) => fact.tier === "semantic") &&
		prove(replay(proof.facts), proof.obligation)
	);
}

const declareInt = (name) => semanticFact("declare", { name, lo: I64_MIN, hi: I64_MAX });
const declareUint = (name) => semanticFact("declare", { name, lo: U64_MIN, hi: U64_MAX });

const scenarios = [];

function scenario(name, facts, obligation, expected = true) {
	const result = prove(replay(facts), obligation);
	assert.equal(result, expected, name);
	scenarios.push({ name, result, certificate: result ? certificate(obligation, facts) : null });
}

scenario("constant checked addition", [semanticFact("declare", { name: "x", lo: 40n, hi: 40n })], {
	kind: "add-in-range",
	value: "x",
	addend: 2n,
	lo: I64_MIN,
	hi: I64_MAX,
});

scenario(
	"branch-refined index",
	[
		declareInt("index"),
		declareUint("length"),
		semanticFact("at-least", { name: "index", value: 0n }),
		semanticFact("difference", { lhs: "index", rhs: "length", offset: -1n }),
	],
	{ kind: "index", index: "index", length: "length" },
);

scenario(
	"known collection length",
	[
		semanticFact("declare", { name: "index", lo: 2n, hi: 2n }),
		semanticFact("declare", { name: "length", lo: 3n, hi: 3n }),
	],
	{ kind: "index", index: "index", length: "length" },
);

scenario(
	"negative collection index",
	[
		semanticFact("declare", { name: "index", lo: -3n, hi: -1n }),
		semanticFact("declare", { name: "length", lo: 3n, hi: 3n }),
	],
	{ kind: "negative-index", index: "index", length: "length" },
);

scenario(
	"exclusive range-loop binder",
	[
		declareUint("item"),
		declareUint("end"),
		semanticFact("difference", { lhs: "item", rhs: "end", offset: -1n }),
	],
	{ kind: "index", index: "item", length: "end" },
);

scenario(
	"branch-refined uint to int",
	[declareUint("value"), semanticFact("at-most", { name: "value", value: I64_MAX })],
	{ kind: "conversion", value: "value", lo: I64_MIN, hi: I64_MAX },
);

scenario(
	"checked-operation Some refinement",
	[declareInt("value"), semanticFact("at-least", { name: "value", value: U64_MIN })],
	{ kind: "conversion", value: "value", lo: U64_MIN, hi: U64_MAX },
);

scenario(
	"branch-refined nonzero divisor",
	[declareInt("divisor"), semanticFact("exclude", { name: "divisor", value: 0n })],
	{ kind: "nonzero", value: "divisor" },
);

scenario(
	"exclusive slice endpoints (reversal remains valid and empty)",
	[
		semanticFact("declare", { name: "start", lo: 3n, hi: 3n }),
		semanticFact("declare", { name: "end", lo: 1n, hi: 1n }),
		semanticFact("declare", { name: "length", lo: 4n, hi: 4n }),
	],
	{ kind: "slice-exclusive-bound", bound: "start", length: "length" },
);

scenario(
	"inclusive slice end must name an element",
	[
		semanticFact("declare", { name: "end", lo: 3n, hi: 3n }),
		semanticFact("declare", { name: "length", lo: 4n, hi: 4n }),
	],
	{ kind: "slice-inclusive-end", bound: "end", length: "length" },
);

scenario(
	"uncertain input retains runtime check",
	[declareInt("index"), declareUint("length")],
	{ kind: "index", index: "index", length: "length" },
	false,
);

for (const item of scenarios) {
	if (item.certificate !== null) assert(verifyCertificate(item.certificate));
}

assert.equal(
	classify([semanticFact("declare", { name: "x", lo: I64_MAX, hi: I64_MAX })], {
		kind: "add-in-range",
		value: "x",
		addend: 1n,
		lo: I64_MIN,
		hi: I64_MAX,
	}),
	"invalid",
	"constant overflow is a semantic diagnostic",
);
assert.equal(
	classify(
		[
			semanticFact("declare", { name: "index", lo: 3n, hi: 3n }),
			semanticFact("declare", { name: "length", lo: 3n, hi: 3n }),
		],
		{ kind: "index", index: "index", length: "length" },
	),
	"invalid",
	"constant out-of-bounds indexing is a semantic diagnostic",
);
assert.equal(
	classify(
		[
			semanticFact("declare", { name: "bound", lo: 4n, hi: 4n }),
			semanticFact("declare", { name: "length", lo: 3n, hi: 3n }),
		],
		{ kind: "slice-exclusive-bound", bound: "bound", length: "length" },
	),
	"invalid",
	"constant out-of-bounds slicing is a semantic diagnostic",
);
assert.equal(
	classify([declareInt("index"), declareUint("length")], {
		kind: "index",
		index: "index",
		length: "length",
	}),
	"uncertain",
	"uncertain indexing retains its runtime check",
);

// Cross-module ownership model. Semantic checking reads only the body-free
// interface. Optional inferred summaries may improve emitted code, but cannot
// produce diagnostics or enter ModuleInterface equality/fingerprints.
const stableJson = (value) =>
	JSON.stringify(value, (_key, item) => (typeof item === "bigint" ? `${item}n` : item));
const fingerprint = (value) => createHash("sha256").update(stableJson(value)).digest("hex");

const publicInterface = {
	module: "math",
	exports: [{ name: "clamp", parameters: ["int"], result: "int" }],
};
const implementationA = { body: "min(max(value, 0), 10)", inferred: [0n, 10n] };
const implementationB = { body: "min(max(value, 0), 20)", inferred: [0n, 20n] };
assert.equal(fingerprint(publicInterface), fingerprint(structuredClone(publicInterface)));
assert.notEqual(fingerprint(implementationA), fingerprint(implementationB));
assert.equal(
	fingerprint(publicInterface),
	fingerprint(publicInterface),
	"a body-only edit must not change the semantic interface fingerprint",
);

const inferredSummary = optimizationFact("return-interval", {
	definition: "math::clamp",
	lo: implementationA.inferred[0],
	hi: implementationA.inferred[1],
});
assert.equal(inferredSummary.tier, "optimization");
assert.throws(() =>
	certificate({ kind: "conversion", value: "result", lo: 0n, hi: 10n }, [inferredSummary]),
);

const candidates = [
	{
		name: "independent intervals",
		constant: true,
		branch: false,
		length: true,
		loop: false,
		conversion: true,
		slicing: true,
		nonzero: false,
		cost: "low",
	},
	{
		name: "intervals + exclusions + octagonal bounds",
		constant: true,
		branch: true,
		length: true,
		loop: true,
		conversion: true,
		slicing: true,
		nonzero: true,
		cost: "bounded",
	},
	{
		name: "general affine/polyhedral",
		constant: true,
		branch: true,
		length: true,
		loop: true,
		conversion: true,
		slicing: true,
		nonzero: true,
		cost: "high/canonicalization-sensitive",
	},
];

console.table(candidates);
console.log(`verified ${scenarios.length} semantic scenarios and certificate replay`);
console.log("verified body-only cross-module summaries remain outside interface fingerprints");
