// Behavioral prototype for issue #88. This is not destination runtime code.
//
// Candidate seam under test:
//   semantic types/effect solver -> effect charging and Task/Handle contracts
//   HIR                         -> explicit task/context/await/cleanup operations
//   generated JavaScript        -> recipe closures with a hidden execution frame
//   host runtime                -> executions, outcomes, cancellation, joins, races

import assert from "node:assert/strict";

const CANCELLED = Symbol("cancelled");
let settlementSequence = 0;

class Cancellation extends Error {
	constructor() {
		super("execution cancelled");
		this.name = "Cancellation";
		this[CANCELLED] = true;
	}
}

class CleanupDefect extends Error {
	constructor(primary, suppressed, cancellationContext = false) {
		super(primary?.message ?? "cleanup defect");
		this.name = "CleanupDefect";
		this.primary = primary;
		this.suppressed = suppressed;
		this.cancellationContext = cancellationContext;
	}
}

const isCancellation = (error) => error?.[CANCELLED] === true;
const ok = (value) => ({ tag: "produced", value });
const cancelled = () => ({ tag: "cancelled" });
const defected = (defect) => ({ tag: "defected", defect });

class Handle {
	#execution;
	#observed = false;

	constructor(execution) {
		this.#execution = execution;
	}

	get settled() {
		return this.#execution.settled;
	}

	get settlementSequence() {
		return this.#execution.settlementSequence;
	}

	get observed() {
		return this.#observed;
	}

	cancel() {
		this.#execution.cancel();
	}

	observe() {
		this.#observed = true;
		return this.#execution.outcome;
	}

	peek() {
		return this.#execution.outcome;
	}

	async await() {
		return this.observe();
	}
}

class TaskContext {
	#children = [];

	adopt(handle) {
		this.#children.push(handle);
		void handle.peek().then((outcome) => {
			if (outcome.tag === "defected") this.cancelChildren(handle);
		});
	}

	cancelChildren(except) {
		for (const child of this.#children) {
			if (child !== except && !child.settled) child.cancel();
		}
	}

	async join() {
		const outcomes = await Promise.all(this.#children.map((child) => child.peek()));
		const unobservedDefect = outcomes.findIndex(
			(outcome, index) => outcome.tag === "defected" && !this.#children[index].observed,
		);
		if (unobservedDefect !== -1) throw outcomes[unobservedDefect].defect;
	}
}

class Execution {
	#recipe;
	#controller = new AbortController();
	#context;
	#cancellationChildren = [];
	#resolve;

	settled = false;
	settlementSequence = undefined;
	outcome = new Promise((resolve) => {
		this.#resolve = resolve;
	});

	constructor(recipe) {
		this.#recipe = recipe;
		this.handle = new Handle(this);
	}

	start(context = new TaskContext()) {
		this.#context = context;
		void this.#drive();
		return this.handle;
	}

	adoptCancellationChild(handle) {
		this.#cancellationChildren.push(handle);
	}

	cancelChildren() {
		for (const child of this.#cancellationChildren) {
			if (!child.settled) child.cancel();
		}
	}

	async joinCancellationChildren() {
		return Promise.all(this.#cancellationChildren.map((child) => child.peek()));
	}

	cancel() {
		if (this.settled || this.#controller.signal.aborted) return;
		this.#controller.abort();
		this.cancelChildren();
	}

	async #drive() {
		const frame = {
			signal: this.#controller.signal,
			context: this.#context,
			execution: this,
		};
		let outcome;
		try {
			const value = await this.#recipe(frame);
			if (frame.signal.aborted) throw new Cancellation();
			outcome = ok(value);
		} catch (error) {
			let failure = error;
			this.cancelChildren();
			const childOutcomes = await this.joinCancellationChildren();
			const childDefect = childOutcomes.find((child) => child.tag === "defected");
			if (isCancellation(failure) && childDefect) failure = childDefect.defect;
			outcome = isCancellation(failure) ? cancelled() : defected(failure);
		}
		this.settled = true;
		this.settlementSequence = settlementSequence++;
		this.#resolve(outcome);
	}
}

class Task {
	#recipe;
	#defaultHandle;

	constructor(recipe) {
		this.#recipe = recipe;
	}

	spawn(frame) {
		const handle = new Execution(this.#recipe).start(frame.context);
		frame.context.adopt(handle);
		frame.execution.adoptCancellationChild(handle);
		return handle;
	}

	async await(frame) {
		this.#defaultHandle ??= this.spawn(frame);
		const outcome = await this.#defaultHandle.await();
		if (outcome.tag === "produced") return outcome.value;
		if (outcome.tag === "cancelled") throw new Cancellation();
		throw outcome.defect;
	}
}

const task = (recipe) => new Task(recipe);

async function withContext(frame, body) {
	const context = new TaskContext();
	const nestedFrame = { ...frame, context };
	let value;
	let failure;
	try {
		value = await body(nestedFrame);
	} catch (error) {
		failure = error;
		context.cancelChildren();
	}
	try {
		await context.join();
	} catch (childDefect) {
		failure ??= childDefect;
	}
	if (failure) throw failure;
	return value;
}

async function withCleanup(frame, body) {
	const closers = [];
	const register = (close) => closers.push(close);
	let value;
	let failure;
	try {
		value = await body(register);
	} catch (error) {
		failure = error;
	}
	if (failure) {
		frame.execution.cancelChildren();
		const childOutcomes = await frame.execution.joinCancellationChildren();
		const childDefect = childOutcomes.find((child) => child.tag === "defected");
		if (isCancellation(failure) && childDefect) failure = childDefect.defect;
	}

	const cleanupDefects = [];
	for (let index = closers.length - 1; index >= 0; index--) {
		try {
			const result = closers[index]();
			assert.equal(result, undefined, "Close must remain synchronous");
		} catch (error) {
			cleanupDefects.push(error);
		}
	}

	if (cleanupDefects.length > 0) {
		if (isCancellation(failure)) {
			throw new CleanupDefect(cleanupDefects[0], cleanupDefects.slice(1), true);
		}
		if (failure) throw new CleanupDefect(failure, cleanupDefects);
		throw new CleanupDefect(cleanupDefects[0], cleanupDefects.slice(1));
	}
	if (failure) throw failure;
	return value;
}

function checkpoint(frame) {
	if (frame.signal.aborted) throw new Cancellation();
}

async function cancellableHostAwait(frame, operation) {
	checkpoint(frame);
	try {
		const value = await operation(frame.signal);
		checkpoint(frame);
		return value;
	} catch (error) {
		if (frame.signal.aborted) throw new Cancellation();
		throw error;
	}
}

function firstSettledTask(handles) {
	return task(async (frame) => {
		// The language rule deliberately prefers the lowest input index when the
		// selection starts with multiple terminal handles.
		const alreadySettled = handles.findIndex((handle) => handle.settled);
		if (alreadySettled !== -1) {
			const handle = handles[alreadySettled];
			return { index: alreadySettled, result: await handle.await() };
		}

		return new Promise((resolve, reject) => {
			let selected = false;
			const abort = () => {
				if (selected) return;
				selected = true;
				reject(new Cancellation());
			};
			frame.signal.addEventListener("abort", abort, { once: true });
			handles.forEach((handle) => {
				void handle.peek().then(async () => {
					if (selected) return;
					const winner = handles.reduce((best, candidate, candidateIndex) => {
						if (!candidate.settled) return best;
						if (best === undefined) return candidateIndex;
						const bestHandle = handles[best];
						return candidate.settlementSequence < bestHandle.settlementSequence
							? candidateIndex
							: best;
					}, undefined);
					selected = true;
					frame.signal.removeEventListener("abort", abort);
					resolve({ index: winner, result: await handles[winner].await() });
				});
			});
		});
	});
}

function raceTask(tasks) {
	return task(async (frame) =>
		withContext(frame, async (raceFrame) => {
			const handles = tasks.map((candidate) => candidate.spawn(raceFrame));
			const selection = await firstSettledTask(handles).await(raceFrame);
			handles.forEach((handle, index) => {
				if (index !== selection.index) handle.cancel();
			});
			const loserOutcomes = await Promise.all(handles.map((handle) => handle.await()));
			const loserDefect = loserOutcomes.find(
				(outcome, index) => index !== selection.index && outcome.tag === "defected",
			);
			// Frontier exposed by the prototype: returning only `selection.result`
			// would silently discard a losing execution's cleanup defect.
			if (loserDefect) throw loserDefect.defect;
			return selection.result;
		}),
	);
}

async function driveRoot(rootTask) {
	const rootContext = new TaskContext();
	const rootExecution = {
		adoptCancellationChild() {},
		cancelChildren() {},
		async joinCancellationChildren() {
			return [];
		},
	};
	const rootFrame = {
		signal: new AbortController().signal,
		context: rootContext,
		execution: rootExecution,
	};
	let value;
	let failure;
	try {
		value = await rootTask.await(rootFrame);
	} catch (error) {
		failure = error;
		rootContext.cancelChildren();
	}
	try {
		await rootContext.join();
	} catch (error) {
		failure ??= error;
	}
	if (failure) throw failure;
	return value;
}

const delay = (milliseconds, signal, value) =>
	new Promise((resolve, reject) => {
		const timer = setTimeout(() => resolve(value), milliseconds);
		signal.addEventListener(
			"abort",
			() => {
				clearTimeout(timer);
				reject(new DOMException("aborted", "AbortError"));
			},
			{ once: true },
		);
	});

async function runPrototype() {
	let starts = 0;
	const reusable = task(async () => ++starts);
	assert.equal(starts, 0, "recipes are cold");
	const defaultPair = await driveRoot(
		task(async (frame) => [await reusable.await(frame), await reusable.await(frame)]),
	);
	assert.deepEqual(defaultPair, [1, 1], "direct await memoizes one default execution");
	const freshPair = await driveRoot(
		task(async (frame) => {
			const first = reusable.spawn(frame);
			const second = reusable.spawn(frame);
			return [(await first.await()).value, (await second.await()).value];
		}),
	);
	assert.deepEqual(freshPair, [2, 3], "spawn creates fresh executions");

	const cancellationEvents = [];
	const cancellable = task((frame) =>
		withCleanup(frame, async (register) => {
			register(() => cancellationEvents.push("close outer") && undefined);
			register(() => cancellationEvents.push("close inner") && undefined);
			await cancellableHostAwait(frame, (signal) => delay(100, signal));
		}),
	);
	const cancelledOutcome = await driveRoot(
		task(async (frame) => {
			const handle = cancellable.spawn(frame);
			handle.cancel();
			return handle.await();
		}),
	);
	assert.equal(cancelledOutcome.tag, "cancelled");
	assert.deepEqual(cancellationEvents, ["close inner", "close outer"]);

	const lineageEvents = [];
	const parent = task((frame) =>
		withCleanup(frame, async (register) => {
			register(() => lineageEvents.push("parent cleanup") && undefined);
			task((childFrame) =>
				withCleanup(childFrame, async (registerChild) => {
					registerChild(() => lineageEvents.push("child cleanup") && undefined);
					await cancellableHostAwait(childFrame, (signal) => delay(100, signal));
				}),
			).spawn(frame);
			await cancellableHostAwait(frame, (signal) => delay(100, signal));
		}),
	);
	const parentOutcome = await driveRoot(
		task(async (frame) => {
			const handle = parent.spawn(frame);
			handle.cancel();
			return handle.await();
		}),
	);
	assert.equal(parentOutcome.tag, "cancelled");
	assert.deepEqual(
		lineageEvents,
		["child cleanup", "parent cleanup"],
		"execution cancellation lineage is separate from inherited structured context",
	);

	const siblingEvents = [];
	const childContext = new TaskContext();
	const childDefect = await new Execution((frame) =>
		withContext(frame, async (nestedFrame) => {
			task(async () => {
				throw new Error("child panic");
			}).spawn(nestedFrame);
			task((childFrame) =>
				withCleanup(childFrame, async (register) => {
					register(() => siblingEvents.push("sibling cleanup") && undefined);
					await cancellableHostAwait(childFrame, (signal) => delay(100, signal));
				}),
			).spawn(nestedFrame);
		}),
	)
		.start(childContext)
		.await();
	assert.equal(childDefect.tag, "defected");
	assert.equal(childDefect.defect.message, "child panic");
	assert.deepEqual(siblingEvents, ["sibling cleanup"]);

	const settledFrame = {
		signal: new AbortController().signal,
		context: new TaskContext(),
		execution: { adoptCancellationChild() {} },
	};
	const settledHandles = [
		task(async () => "zero").spawn(settledFrame),
		task(async () => "one").spawn(settledFrame),
	];
	await Promise.all(settledHandles.map((handle) => handle.peek()));
	const selection = await driveRoot(firstSettledTask(settledHandles));
	assert.equal(selection.index, 0, "already-settled selection uses lowest input index");
	assert.equal(settledHandles[1].observed, false, "selection neither owns nor observes losers");

	const raceEvents = [];
	const raced = raceTask([
		task(async (frame) => cancellableHostAwait(frame, (signal) => delay(5, signal, "winner"))),
		task((frame) =>
			withCleanup(frame, async (register) => {
				register(() => raceEvents.push("loser cleanup") && undefined);
				return cancellableHostAwait(frame, (signal) => delay(100, signal, "loser"));
			}),
		),
	]);
	assert.deepEqual(await driveRoot(raced), ok("winner"));
	assert.deepEqual(raceEvents, ["loser cleanup"], "race joins loser cleanup before returning");

	const cleanupConflict = raceTask([
		task(async () => "winner"),
		task((frame) =>
			withCleanup(frame, async (register) => {
				register(() => {
					throw new Error("loser close panic");
				});
				await cancellableHostAwait(frame, (signal) => delay(100, signal));
			}),
		),
	]);
	await assert.rejects(
		() => driveRoot(cleanupConflict),
		(error) => error instanceof CleanupDefect && error.cancellationContext,
		"prototype candidate defects race rather than discarding loser cleanup defects",
	);

	console.log("issue #88 async seam prototype: 20 behavioral assertions passed");
}

await runPrototype();
