import assert from "node:assert/strict";

const sleep = (milliseconds, value) =>
	new Promise((resolve) => setTimeout(() => resolve(value), milliseconds));

let starts = 0;
const reusable = nymphTask(() => ++starts);
assert.equal(starts, 0);
assert.deepEqual(await nymphStartRoot(() => 42, false).outcome, nymphProduced(42));
const launched = nymphStartRoot(() => reusable, true);
assert.deepEqual(await launched.outcome, nymphProduced(1));
assert.equal(starts, 1);
assert.equal(
	nymphRenderDefect(new Error("root failed")),
	"error: program defected: Error: root failed\n",
);
assert.equal(
	nymphRenderDefect(
		new Proxy(
			{},
			{
				getOwnPropertyDescriptor() {
					throw new Error("renderer");
				},
			},
		),
	),
	"error: program defected\n",
);
assert.deepEqual(
	await nymphRunTask(
		nymphTask(async (frame) => [await reusable.drive(frame), await reusable.drive(frame)]),
	),
	[1, 1],
);
assert.deepEqual(
	await nymphRunTask(
		nymphTask(async (frame) => {
			const first = reusable.spawn(frame);
			const second = reusable.spawn(frame);
			return [(await first.observe()).value, (await second.observe()).value];
		}),
	),
	[2, 3],
);

const applicationError = Object.freeze({ tag: "error", value: "expected" });
const nestedOutcome = await nymphRunTask(
	nymphTask(async (frame) => {
		const handle = nymphTask(() => applicationError).spawn(frame);
		const first = await handle.observe();
		const second = await handle.observe();
		assert.equal(first, second);
		return first;
	}),
);
assert.equal(nestedOutcome.tag, "produced");
assert.equal(nestedOutcome.value, applicationError);
assert(Object.isFrozen(nestedOutcome));

const contextStep = nymphCallable((frame) => nymphReturn(frame.context));
const inherited = nymphTaskRecipe(contextStep, false);
const nested = nymphTaskRecipe(contextStep, true);
await nymphRunTask(
	nymphTask(async (frame) => {
		assert.equal(await inherited.drive(frame), frame.context);
		assert.notEqual(await nested.drive(frame), frame.context);
	}),
);

const cancellationOrder = [];
let cancelledResume = 0;
const childStep = nymphCallable((frame) => {
	if (frame.resumeState !== 0) {
		cancelledResume += 1;
		return nymphReturn("suppressed");
	}
	nymphRegisterCleanup(() => cancellationOrder.push("child"));
	return nymphSuspend(new Promise(() => {}), 1, 0);
});
const childTask = nymphTaskRecipe(childStep, false);
const parentStep = nymphCallable((frame) => {
	if (frame.resumeState !== 0) {
		cancelledResume += 1;
		return nymphReturn("suppressed");
	}
	nymphRegisterCleanup(() => cancellationOrder.push("parent"));
	nymphTaskSpawn(childTask);
	return nymphSuspend(new Promise(() => {}), 1, 0);
});
const parentTask = nymphTaskRecipe(parentStep, false);
const cancelledRoot = nymphStartRoot(() => parentTask, true);
cancelledRoot.cancel();
assert.equal((await cancelledRoot.outcome).tag, "cancelled");
cancellationOrder.length = 0;
const cancelledOutcome = await nymphRunTask(
	nymphTask(async (frame) => {
		const handle = parentTask.spawn(frame);
		handle.cancel();
		return handle.observe();
	}),
);
assert.equal(cancelledOutcome.tag, "cancelled");
assert.deepEqual(cancellationOrder, ["child", "parent"]);
assert.equal(cancelledResume, 0);

const cleanupCounts = { child: 0, parent: 0 };
const defectiveChild = nymphTaskRecipe(
	nymphCallable(() => {
		nymphRegisterCleanup(() => {
			cleanupCounts.child += 1;
			throw new Error("child cleanup defect");
		});
		return nymphSuspend(new Promise(() => {}), 1, 0);
	}),
	false,
);
const defectiveParent = nymphTaskRecipe(
	nymphCallable(() => {
		nymphRegisterCleanup(() => {
			cleanupCounts.parent += 1;
			throw new Error("parent cleanup defect");
		});
		nymphTaskSpawn(defectiveChild);
		return nymphSuspend(new Promise(() => {}), 1, 0);
	}),
	false,
);
const defectiveRoot = nymphStartRoot(() => defectiveParent, true);
defectiveRoot.cancel();
const defectiveRootOutcome = await defectiveRoot.outcome;
assert.equal(defectiveRootOutcome.tag, "defected");
assert.equal(defectiveRootOutcome.defect.cancellationContext, true);
assert.deepEqual(cleanupCounts, { child: 1, parent: 1 });
cleanupCounts.child = 0;
cleanupCounts.parent = 0;
const cleanupOutcome = await nymphRunTask(
	nymphTask(async (frame) => {
		const handle = defectiveParent.spawn(frame);
		handle.cancel();
		return handle.observe();
	}),
);
assert.equal(cleanupOutcome.tag, "defected");
assert.equal(cleanupOutcome.defect.cancellationContext, true);
assert.deepEqual(
	cleanupOutcome.defect.errors.map((error) => error.message),
	["execution cancelled", "child cleanup defect", "parent cleanup defect"],
);
assert.deepEqual(cleanupCounts, { child: 1, parent: 1 });

const checkpointStep = nymphCallable((frame) => {
	if (frame.resumeState === 0) return nymphSuspend(() => nymphCheckpoint(), 1, 0);
	return nymphReturn("checkpoint resumed");
});
const checkpointTask = nymphTaskRecipe(checkpointStep, false);
const checkpointOutcome = await nymphRunTask(
	nymphTask(async (frame) => {
		const handle = checkpointTask.spawn(frame);
		handle.cancel();
		return handle.observe();
	}),
);
assert.equal(checkpointOutcome.tag, "cancelled");

const noCheckpointStep = nymphCallable(() => {
	let total = 0;
	for (let index = 0; index < 10000; index += 1) total += index;
	return nymphReturn(total);
});
const noCheckpointHandle = nymphTaskRecipe(noCheckpointStep, false).spawn();
noCheckpointHandle.cancel();
assert.equal((await noCheckpointHandle.observe()).tag, "produced");

const suppressedCancellation = nymphTask(async (frame) => {
	try {
		await nymphAwaitCancellable(frame, new Promise(() => {}));
	} catch {
		return "suppressed";
	}
});
const suppressedHandle = suppressedCancellation.spawn();
suppressedHandle.cancel();
assert.equal((await suppressedHandle.observe()).tag, "cancelled");

await nymphRunTask(
	nymphTask(async (frame) => {
		const earlier = nymphTask(() => "earlier").spawn(frame);
		const later = nymphTask(() => "later").spawn(frame);
		await Promise.all([earlier.peek(), later.peek()]);
		const handles = [later, earlier];
		const selection = await nymphTaskSelect(handles).drive(frame);
		assert.equal(selection.index, 0);
		assert.equal(selection.result.value, "later");
		assert.equal(earlier.observed, false);

		const slow = nymphTask(async () => sleep(20, "slow")).spawn(frame);
		const fast = nymphTask(async () => sleep(1, "fast")).spawn(frame);
		const first = await nymphTaskSelect([slow, fast]).drive(frame);
		assert.equal(first.index, 1);
		assert.equal(slow.observed, false);
		slow.cancel();
		await slow.observe();
	}),
);

const raceOrder = [];
const winner = nymphTaskRecipe(
	nymphCallable(() => nymphReturn("winner")),
	false,
);
const losingStep = nymphCallable((frame) => {
	if (frame.resumeState === 0) {
		nymphRegisterCleanup(() => raceOrder.push("loser cleanup"));
		return nymphSuspend(new Promise(() => {}), 1, 0);
	}
	return nymphReturn("loser");
});
const raceResult = await nymphRunTask(nymphTaskRace([winner, nymphTaskRecipe(losingStep, false)]));
assert.deepEqual(raceResult, nymphProduced("winner"));
assert.deepEqual(raceOrder, ["loser cleanup"]);

const defectiveLoserStep = nymphCallable((frame) => {
	if (frame.resumeState === 0) {
		nymphRegisterCleanup(() => {
			throw new Error("loser cleanup defect");
		});
		return nymphSuspend(new Promise(() => {}), 1, 0);
	}
	return nymphReturn("loser");
});
await assert.rejects(
	() => nymphRunTask(nymphTaskRace([winner, nymphTaskRecipe(defectiveLoserStep, false)])),
	(error) =>
		error instanceof AggregateError &&
		error.cancellationContext === true &&
		error.errors[1].message === "loser cleanup defect",
);

const siblingOrder = [];
const siblingStep = nymphCallable((frame) => {
	if (frame.resumeState === 0) {
		nymphRegisterCleanup(() => siblingOrder.push("sibling cleanup"));
		return nymphSuspend(new Promise(() => {}), 1, 0);
	}
	return nymphReturn(undefined);
});
await assert.rejects(
	() =>
		nymphRunTask(
			nymphTask((frame) =>
				nymphWithTaskContext(frame, (nestedFrame) => {
					nymphTask(() => {
						throw new Error("child defect");
					}).spawn(nestedFrame);
					nymphTaskRecipe(siblingStep, false).spawn(nestedFrame);
				}),
			),
		),
	/child defect/,
);
assert.deepEqual(siblingOrder, ["sibling cleanup"]);

const observedDefect = await nymphRunTask(
	nymphTask(async (frame) => {
		const handle = nymphTask(() => {
			throw new Error("isolated");
		}).spawn(frame);
		return handle.observe();
	}),
);
assert.equal(observedDefect.tag, "defected");
assert.equal(observedDefect.defect.message, "isolated");

const cycleCount = 10000;
const beforeSlots = nymphNextFrameSlot;
const cycleStep = nymphCallable((frame) => {
	const remaining = frame.liveLocals[0];
	if (remaining === 0 && frame.resumeState === 0) return nymphReturn("done");
	if (frame.resumeState === 0) return nymphSuspend(Promise.resolve(), 1, 1);
	return nymphTailCall(cycleStep, undefined, [remaining - 1], -1);
});
assert.equal(
	await nymphRunTask(nymphTask((frame) => nymphRunTaskActivation(cycleStep, frame, [cycleCount]))),
	"done",
);
assert.equal(nymphNextFrameSlot - beforeSlots, 1);

console.log("structured task runtime assertions passed");
