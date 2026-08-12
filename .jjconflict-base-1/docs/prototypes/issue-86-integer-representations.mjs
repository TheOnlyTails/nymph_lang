// Focused representation prototype for https://github.com/TheOnlyTails/nymph_lang/issues/86.
// This is not destination runtime code. It compares arithmetic kernels before Nymph's common
// outer NInt/NUint allocation, and uses BigInt as the correctness oracle.

import { gzipSync } from "node:zlib";
import { performance } from "node:perf_hooks";

const I64_MIN = -(1n << 63n);
const I64_MAX = (1n << 63n) - 1n;
const SAFE_MIN = BigInt(Number.MIN_SAFE_INTEGER);
const SAFE_MAX = BigInt(Number.MAX_SAFE_INTEGER);
const U32_MASK = 0xffff_ffffn;

function inI64(value) {
	return value >= I64_MIN && value <= I64_MAX;
}

// Candidate A: a BigInt payload in the existing NInt/NUint box.
function bigAdd(left, right) {
	const result = left + right;
	if (!inI64(result)) throw new RangeError("int overflow");
	return result;
}

function bigMul(left, right) {
	const result = left * right;
	if (!inI64(result)) throw new RangeError("int overflow");
	return result;
}

function bigHash(value) {
	return (Number(value & U32_MASK) ^ Number((value >> 32n) & U32_MASK)) | 0;
}

function bigToHost(value) {
	return value;
}

function bigFromHost(value) {
	if (typeof value !== "bigint" || !inI64(value)) throw new TypeError("expected i64 bigint");
	return value;
}

// Candidate B: canonical Number for the safe subset, BigInt otherwise. A stable FFI still uses
// BigInt, so the union remains an internal optimization rather than leaking into every external.
function hybridFromBig(value) {
	if (!inI64(value)) throw new RangeError("int overflow");
	return value >= SAFE_MIN && value <= SAFE_MAX ? Number(value) : value;
}

function hybridToBig(value) {
	return typeof value === "bigint" ? value : BigInt(value);
}

function hybridAdd(left, right) {
	if (typeof left === "number" && typeof right === "number") {
		const result = left + right;
		if (Number.isSafeInteger(result)) return result;
	}
	return hybridFromBig(hybridToBig(left) + hybridToBig(right));
}

function hybridMul(left, right) {
	if (typeof left === "number" && typeof right === "number") {
		const result = left * right;
		if (Number.isSafeInteger(result)) return result;
	}
	return hybridFromBig(hybridToBig(left) * hybridToBig(right));
}

function hybridHash(value) {
	return bigHash(hybridToBig(value));
}

function hybridToHost(value) {
	return hybridToBig(value);
}

function hybridFromHost(value) {
	if (typeof value !== "bigint") throw new TypeError("expected i64 bigint");
	return hybridFromBig(value);
}

// Candidate C: two 32-bit words in little-endian order. These focused add/multiply kernels are
// enough to measure the option's minimum machinery; a complete runtime would additionally need
// division/remainder, shifts, conversions, parsing, formatting, and unsigned variants.
function pairFromBig(value) {
	if (!inI64(value)) throw new RangeError("int overflow");
	const bits = BigInt.asUintN(64, value);
	return { lo: Number(bits & U32_MASK), hi: Number(bits >> 32n) };
}

function pairToBig(value) {
	return BigInt.asIntN(64, (BigInt(value.hi) << 32n) | BigInt(value.lo));
}

function pairAdd(left, right) {
	const lo = (left.lo + right.lo) >>> 0;
	const hi = (left.hi + right.hi + (lo < left.lo ? 1 : 0)) >>> 0;
	const leftNegative = left.hi >= 0x8000_0000;
	const rightNegative = right.hi >= 0x8000_0000;
	const resultNegative = hi >= 0x8000_0000;
	if (leftNegative === rightNegative && leftNegative !== resultNegative)
		throw new RangeError("int overflow");
	return { lo, hi };
}

function pairNegate(value) {
	const lo = -value.lo >>> 0;
	return { lo, hi: (~value.hi + (lo === 0 ? 1 : 0)) >>> 0 };
}

function pairMagnitude(value) {
	return value.hi >= 0x8000_0000 ? pairNegate(value) : value;
}

function pairMul(left, right) {
	const negative = left.hi >= 0x8000_0000 !== right.hi >= 0x8000_0000;
	const a = pairMagnitude(left);
	const b = pairMagnitude(right);
	const al = [a.lo & 0xffff, a.lo >>> 16, a.hi & 0xffff, a.hi >>> 16];
	const bl = [b.lo & 0xffff, b.lo >>> 16, b.hi & 0xffff, b.hi >>> 16];
	const limbs = Array(8).fill(0);
	for (let i = 0; i < 4; i++) for (let j = 0; j < 4; j++) limbs[i + j] += al[i] * bl[j];
	for (let i = 0; i < 7; i++) {
		const carry = Math.floor(limbs[i] / 0x1_0000);
		limbs[i] %= 0x1_0000;
		limbs[i + 1] += carry;
	}
	const limit = negative ? [0, 0, 0, 0x8000] : [0xffff, 0xffff, 0xffff, 0x7fff];
	let overflow = limbs.slice(4).some(Boolean);
	for (let i = 3; !overflow && i >= 0; i--) {
		if (limbs[i] !== limit[i]) {
			overflow = limbs[i] > limit[i];
			break;
		}
	}
	if (overflow) throw new RangeError("int overflow");
	let result = {
		lo: (limbs[0] | (limbs[1] << 16)) >>> 0,
		hi: (limbs[2] | (limbs[3] << 16)) >>> 0,
	};
	if (negative) result = pairNegate(result);
	return result;
}

function pairHash(value) {
	return (value.lo ^ value.hi) | 0;
}

function pairToHost(value) {
	return pairToBig(value);
}

function pairFromHost(value) {
	if (typeof value !== "bigint") throw new TypeError("expected i64 bigint");
	return pairFromBig(value);
}

function assertEqual(actual, expected, context) {
	if (actual !== expected) throw new Error(`${context}: expected ${expected}, got ${actual}`);
}

function outcome(operation) {
	try {
		return operation();
	} catch (error) {
		if (error instanceof RangeError) return "overflow";
		throw error;
	}
}

function xorshift64(state) {
	state ^= state << 13n;
	state ^= state >> 7n;
	state ^= state << 17n;
	return BigInt.asUintN(64, state);
}

function verify() {
	const edges = [
		I64_MIN,
		I64_MIN + 1n,
		-9_007_199_254_740_992n,
		-9_007_199_254_740_991n,
		-1n,
		0n,
		1n,
		9_007_199_254_740_991n,
		9_007_199_254_740_992n,
		I64_MAX - 1n,
		I64_MAX,
	];
	let seed = 0x9e37_79b9_7f4a_7c15n;
	const values = [...edges];
	for (let i = 0; i < 20_000; i++) {
		seed = xorshift64(seed);
		values.push(BigInt.asIntN(64, seed));
	}
	for (let i = 0; i < values.length; i++) {
		const left = values[i];
		const right = values[(i * 8191 + 17) % values.length];
		const expectedAdd = inI64(left + right) ? left + right : "overflow";
		const expectedMul = inI64(left * right) ? left * right : "overflow";
		assertEqual(
			outcome(() => bigAdd(left, right)),
			expectedAdd,
			"big add",
		);
		assertEqual(
			outcome(() => hybridToBig(hybridAdd(hybridFromBig(left), hybridFromBig(right)))),
			expectedAdd,
			"hybrid add",
		);
		assertEqual(
			outcome(() => pairToBig(pairAdd(pairFromBig(left), pairFromBig(right)))),
			expectedAdd,
			"pair add",
		);
		assertEqual(
			outcome(() => bigMul(left, right)),
			expectedMul,
			"big multiply",
		);
		assertEqual(
			outcome(() => hybridToBig(hybridMul(hybridFromBig(left), hybridFromBig(right)))),
			expectedMul,
			"hybrid multiply",
		);
		assertEqual(
			outcome(() => pairToBig(pairMul(pairFromBig(left), pairFromBig(right)))),
			expectedMul,
			"pair multiply",
		);
	}
	return { vectors: values.length, operationsPerCandidate: values.length * 2 };
}

let sink;

function measure(name, iterations, operation) {
	for (let i = 0; i < 3; i++) operation(Math.max(1, Math.floor(iterations / 10)));
	const samples = [];
	for (let sample = 0; sample < 7; sample++) {
		const start = performance.now();
		sink = operation(iterations);
		samples.push(((performance.now() - start) * 1e6) / iterations);
	}
	samples.sort((a, b) => a - b);
	return { name, medianNsPerOp: Math.round(samples[3] * 10) / 10 };
}

function sourceSize(functions) {
	const source = functions.map((fn) => fn.toString()).join("\n");
	return { sourceBytes: Buffer.byteLength(source), gzipBytes: gzipSync(source).byteLength };
}

class NInt {
	constructor(value) {
		this.v = value;
	}
}

const correctness = verify();
const iterations = 1_000_000;
const smallLeft = 1_234_567;
const smallRight = 7_654_321;
const smallBigLeft = BigInt(smallLeft);
const smallBigRight = BigInt(smallRight);
const wideLeft = 4_611_686_018_427_387_000n;
const wideRight = 777n;
const nearSqrtMax = 3_037_000_499n;

const candidates = {
	numberReference: {
		codeSize: { sourceBytes: 0, gzipBytes: 0 },
		benchmarks: [
			measure("small add", iterations, (count) => {
				let value = 0;
				for (let i = 0; i < count; i++) value += 1;
				return value;
			}),
			measure("boxed small add", iterations, (count) => {
				let value = new NInt(0);
				for (let i = 0; i < count; i++) value = new NInt(value.v + 1);
				return value;
			}),
			measure("small multiply", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = smallLeft * smallRight;
				return value;
			}),
		],
	},
	bigint: {
		codeSize: sourceSize([inI64, bigAdd, bigMul, bigHash, bigToHost, bigFromHost]),
		benchmarks: [
			measure("small add", iterations, (count) => {
				let value = 0n;
				for (let i = 0; i < count; i++) value = bigAdd(value, 1n);
				return value;
			}),
			measure("boxed small add", iterations, (count) => {
				let value = new NInt(0n);
				for (let i = 0; i < count; i++) value = new NInt(bigAdd(value.v, 1n));
				return value;
			}),
			measure("small multiply", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = bigMul(smallBigLeft, smallBigRight);
				return value;
			}),
			measure("wide add", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = bigAdd(wideLeft, wideRight);
				return value;
			}),
			measure("wide multiply", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = bigMul(nearSqrtMax, nearSqrtMax);
				return value;
			}),
			measure("hash", iterations, (count) => {
				let value = 0;
				for (let i = 0; i < count; i++) value ^= bigHash(wideLeft);
				return value;
			}),
			measure("FFI round trip", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = bigFromHost(bigToHost(wideLeft));
				return value;
			}),
		],
	},
	hybrid: {
		codeSize: sourceSize([
			inI64,
			hybridFromBig,
			hybridToBig,
			hybridAdd,
			hybridMul,
			hybridHash,
			hybridToHost,
			hybridFromHost,
		]),
		benchmarks: [
			measure("small add", iterations, (count) => {
				let value = 0;
				for (let i = 0; i < count; i++) value = hybridAdd(value, 1);
				return value;
			}),
			measure("boxed small add", iterations, (count) => {
				let value = new NInt(0);
				for (let i = 0; i < count; i++) value = new NInt(hybridAdd(value.v, 1));
				return value;
			}),
			measure("small multiply", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = hybridMul(smallLeft, smallRight);
				return value;
			}),
			measure("wide add", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = hybridAdd(wideLeft, wideRight);
				return value;
			}),
			measure("wide multiply", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = hybridMul(nearSqrtMax, nearSqrtMax);
				return value;
			}),
			measure("hash", iterations, (count) => {
				let value = 0;
				for (let i = 0; i < count; i++) value ^= hybridHash(wideLeft);
				return value;
			}),
			measure("FFI round trip", iterations, (count) => {
				let value;
				for (let i = 0; i < count; i++) value = hybridFromHost(hybridToHost(wideLeft));
				return value;
			}),
		],
	},
	pair32: {
		codeSize: sourceSize([
			pairFromBig,
			pairToBig,
			pairAdd,
			pairNegate,
			pairMagnitude,
			pairMul,
			pairHash,
			pairToHost,
			pairFromHost,
		]),
		benchmarks: [
			measure("small add", iterations, (count) => {
				let value = pairFromBig(0n);
				const one = pairFromBig(1n);
				for (let i = 0; i < count; i++) value = pairAdd(value, one);
				return value;
			}),
			measure("boxed small add", iterations, (count) => {
				let value = new NInt(pairFromBig(0n));
				const one = pairFromBig(1n);
				for (let i = 0; i < count; i++) value = new NInt(pairAdd(value.v, one));
				return value;
			}),
			measure("small multiply", iterations, (count) => {
				let value;
				const left = pairFromBig(BigInt(smallLeft));
				const right = pairFromBig(BigInt(smallRight));
				for (let i = 0; i < count; i++) value = pairMul(left, right);
				return value;
			}),
			measure("wide add", iterations, (count) => {
				let value;
				const left = pairFromBig(wideLeft);
				const right = pairFromBig(wideRight);
				for (let i = 0; i < count; i++) value = pairAdd(left, right);
				return value;
			}),
			measure("wide multiply", iterations, (count) => {
				let value;
				const operand = pairFromBig(nearSqrtMax);
				for (let i = 0; i < count; i++) value = pairMul(operand, operand);
				return value;
			}),
			measure("hash", iterations, (count) => {
				let value = 0;
				const input = pairFromBig(wideLeft);
				for (let i = 0; i < count; i++) value ^= pairHash(input);
				return value;
			}),
			measure("FFI round trip", iterations, (count) => {
				let value;
				const input = pairFromBig(wideLeft);
				for (let i = 0; i < count; i++) value = pairFromHost(pairToHost(input));
				return value;
			}),
		],
	},
};

console.log(
	JSON.stringify(
		{
			node: process.version,
			platform: `${process.platform}/${process.arch}`,
			correctness,
			caveats: [
				"Median of seven warmed samples; timings are directional, not a release gate.",
				"All candidates exclude the common outer NInt/NUint allocation.",
				"pair32 code size is a lower bound because the prototype implements only signed add/multiply/hash/BigInt FFI.",
				"numberReference is inexact and unchecked; it is not a viable representation.",
			],
			candidates,
		},
		null,
		2,
	),
);

void sink;
