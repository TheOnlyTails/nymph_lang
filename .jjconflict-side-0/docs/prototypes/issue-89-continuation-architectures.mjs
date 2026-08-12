// Prototype for https://github.com/TheOnlyTails/nymph_lang/issues/89.
//
// This is deliberately runtime-shaped JavaScript, not destination compiler code.
// Each `activation` below stands in for a generated defunctionalized callable
// state machine. The driver is the proposed shared seam for ordinary calls,
// proper tail calls, suspension, and deterministic lexical cleanup.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

const DONE = "done";
const CALL = "call";
const TAIL = "tail";
const SUSPEND = "suspend";
const ENTER = "enter";
const LEAVE = "leave";
const USE = "use";

const done = (value) => ({ kind: DONE, value });
const call = (callee, args = []) => ({ kind: CALL, callee, args });
const tail = (callee, args = []) => ({ kind: TAIL, callee, args });
const suspend = (promise) => ({ kind: SUSPEND, promise });
const enter = () => ({ kind: ENTER });
const leave = () => ({ kind: LEAVE });
const use = (close) => ({ kind: USE, close });

function activation(name, step) {
	return { name, step, scopes: [[]] };
}

class Cancellation extends Error {
	constructor() {
		super("cancelled");
		this.name = "Cancellation";
	}
}

class CleanupDefect extends AggregateError {
	constructor(primary, defects) {
		super(defects, "cleanup defect");
		this.name = "CleanupDefect";
		this.primary = primary;
	}
}

function closeScopes(frame, targetDepth, primary) {
	const defects = [];
	while (frame.scopes.length > targetDepth) {
		const scope = frame.scopes.pop();
		for (let i = scope.length - 1; i >= 0; i -= 1) {
			try {
				scope[i]();
			} catch (error) {
				defects.push(error);
			}
		}
	}
	if (defects.length > 0) throw new CleanupDefect(primary, defects);
}

async function awaitSuspension(promise, signal) {
	if (!signal) return promise;
	if (signal.aborted) throw new Cancellation();
	let onAbort;
	const cancelled = new Promise((_, reject) => {
		onAbort = () => reject(new Cancellation());
		signal.addEventListener("abort", onAbort, { once: true });
	});
	try {
		return await Promise.race([promise, cancelled]);
	} finally {
		signal.removeEventListener("abort", onAbort);
	}
}

async function drive(entry, args = [], { signal } = {}) {
	const frames = [{ activation: entry(...args), resume: undefined }];
	let maxFrames = 1;
	let tailTransfers = 0;

	const unwindAll = (primary) => {
		const defects = [];
		while (frames.length > 0) {
			const { activation: frame } = frames.pop();
			try {
				closeScopes(frame, 0, primary);
			} catch (error) {
				defects.push(...error.errors);
			}
		}
		if (defects.length > 0) throw new CleanupDefect(primary, defects);
		throw primary;
	};

	try {
		while (frames.length > 0) {
			if (signal?.aborted) throw new Cancellation();

			const current = frames.at(-1);
			const operation = current.activation.step(current.resume);
			current.resume = undefined;

			switch (operation.kind) {
				case ENTER:
					current.activation.scopes.push([]);
					break;
				case USE:
					current.activation.scopes.at(-1).push(operation.close);
					break;
				case LEAVE:
					closeScopes(current.activation, current.activation.scopes.length - 1);
					break;
				case CALL:
					frames.push({
						activation: operation.callee(...operation.args),
						resume: undefined,
					});
					maxFrames = Math.max(maxFrames, frames.length);
					break;
				case TAIL:
					// Cleanup is part of transfer, but does not keep the caller alive.
					closeScopes(current.activation, 0);
					current.activation = operation.callee(...operation.args);
					tailTransfers += 1;
					break;
				case SUSPEND:
					current.resume = await awaitSuspension(operation.promise, signal);
					break;
				case DONE: {
					closeScopes(current.activation, 0);
					frames.pop();
					if (frames.length === 0) {
						return { value: operation.value, maxFrames, tailTransfers };
					}
					frames.at(-1).resume = operation.value;
					break;
				}
				default:
					throw new Error(`unknown operation: ${operation.kind}`);
			}
		}
	} catch (primary) {
		unwindAll(primary);
	}
}

// Direct proper tail recursion.
function sumTo(n, sum = 0) {
	return activation("sumTo", () => (n === 0 ? done(sum) : tail(sumTo, [n - 1, sum + n])));
}

// Mutual proper tail recursion.
function even(n) {
	return activation("even", () => (n === 0 ? done(true) : tail(odd, [n - 1])));
}
function odd(n) {
	return activation("odd", () => (n === 0 ? done(false) : tail(even, [n - 1])));
}

// A generic callable has the same activation ABI; hidden runtime type objects
// are ordinary arguments and do not create a second continuation convention.
function genericLoop(typeObject, value, n) {
	return activation("genericLoop", () =>
		n === 0
			? done(typeObject.finish(value))
			: tail(genericLoop, [typeObject, typeObject.step(value), n - 1]),
	);
}

// Higher-order/dynamic calls use the exact same tail operation.
function dynamicLoop(next, value, n) {
	return activation("dynamicLoop", () =>
		n === 0 ? done(value) : tail(next, [next, value + 1, n - 1]),
	);
}

// Non-tail calls preserve only an explicit logical activation, then resume it.
function addOneAfter(callee, value) {
	let state = 0;
	return activation("addOneAfter", (input) => {
		if (state === 0) {
			state = 1;
			return call(callee, [value]);
		}
		return done(input + 1);
	});
}
function identity(value) {
	return activation("identity", () => done(value));
}

// Suspension resumes a generated state label, not a native async-function
// call chain. A tail transfer after suspension still replaces the activation.
function suspendedCountdown(n, log) {
	let state = 0;
	return activation("suspendedCountdown", (input) => {
		if (n === 0) return done(log.length);
		if (state === 0) {
			state = 1;
			return suspend(Promise.resolve(n));
		}
		log.push(input);
		return tail(suspendedCountdown, [n - 1, log]);
	});
}

// Lexical cleanup is registered with the current activation. Leaving an inner
// scope and then tail-transferring closes b before a, without retaining a frame.
function cleanupTail(n, log) {
	let state = 0;
	return activation("cleanupTail", () => {
		switch (state) {
			case 0:
				state = 1;
				return use(() => log.push(`a${n}`));
			case 1:
				state = 2;
				return enter();
			case 2:
				state = 3;
				return use(() => log.push(`b${n}`));
			case 3:
				state = 4;
				return leave();
			default:
				return n === 0 ? done("finished") : tail(cleanupTail, [n - 1, log]);
		}
	});
}

// Cancellation crosses the same driver boundary and unwinds lexical cleanup.
function cancellable(signalStarted, log) {
	let state = 0;
	return activation("cancellable", () => {
		if (state === 0) {
			state = 1;
			return use(() => log.push("closed"));
		}
		signalStarted();
		return suspend(new Promise(() => {}));
	});
}

async function cancellationCase() {
	const controller = new AbortController();
	const log = [];
	let started;
	const hasStarted = new Promise((resolve) => {
		started = resolve;
	});
	const result = drive(cancellable, [started, log], { signal: controller.signal });
	await hasStarted;
	controller.abort();
	await assert.rejects(result, Cancellation);
	assert.deepEqual(log, ["closed"]);
}

const direct = await drive(sumTo, [100_000]);
assert.equal(direct.value, 5_000_050_000);
assert.equal(direct.maxFrames, 1);

const mutual = await drive(even, [100_001]);
assert.equal(mutual.value, false);
assert.equal(mutual.maxFrames, 1);

const generic = await drive(genericLoop, [
	{ step: (value) => value + 2, finish: (value) => `int:${value}` },
	0,
	50_000,
]);
assert.equal(generic.value, "int:100000");
assert.equal(generic.maxFrames, 1);

const dynamic = await drive(dynamicLoop, [dynamicLoop, 0, 50_000]);
assert.equal(dynamic.value, 50_000);
assert.equal(dynamic.maxFrames, 1);

const nonTail = await drive(addOneAfter, [identity, 41]);
assert.equal(nonTail.value, 42);
assert.equal(nonTail.maxFrames, 2);

const suspensionLog = [];
const suspended = await drive(suspendedCountdown, [1_000, suspensionLog]);
assert.equal(suspended.value, 1_000);
assert.deepEqual(suspensionLog.slice(0, 3), [1_000, 999, 998]);
assert.equal(suspended.maxFrames, 1);

const cleanupLog = [];
const cleaned = await drive(cleanupTail, [2, cleanupLog]);
assert.equal(cleaned.value, "finished");
assert.equal(cleaned.maxFrames, 1);
assert.deepEqual(cleanupLog, ["b2", "a2", "b1", "a1", "b0", "a0"]);

await cancellationCase();

// Cleanup defects attempt every close and prevent a pending tail transfer.
const defectLog = [];
const defective = () => {
	let state = 0;
	return activation("defective", () => {
		if (state === 0) {
			state = 1;
			return use(() => {
				defectLog.push("first");
				throw new Error("first defect");
			});
		}
		if (state === 1) {
			state = 2;
			return use(() => {
				defectLog.push("second");
				throw new Error("second defect");
			});
		}
		return tail(identity, ["must not run"]);
	});
};
await assert.rejects(drive(defective), (error) => {
	assert.equal(error.name, "CleanupDefect");
	assert.equal(error.errors.length, 2);
	return true;
});
assert.deepEqual(defectLog, ["second", "first"]);

// A split trampoline + native async architecture fails before it can suspend:
// recursive evaluation of the async callee still consumes the JavaScript stack.
// Isolate V8's PromiseRejectCallback diagnostic so prototype output stays stable.
const nativeAsync = spawnSync(
	process.execPath,
	[
		"--input-type=module",
		"-e",
		"async function f(n) { return n === 0 ? 0 : f(n - 1) }\n" +
			"try { await f(100000) } catch (error) { console.log(error.name) }",
	],
	{ encoding: "utf8" },
);
assert.equal(nativeAsync.status, 0);
assert.equal(nativeAsync.stdout.trim(), "RangeError");

console.log(
	JSON.stringify({
		assertions: 25,
		directTailFrames: direct.maxFrames,
		mutualTailFrames: mutual.maxFrames,
		dynamicTailFrames: dynamic.maxFrames,
		suspendedTailFrames: suspended.maxFrames,
		cleanupOrder: cleanupLog,
		nativeAsyncTail: "RangeError",
	}),
);
