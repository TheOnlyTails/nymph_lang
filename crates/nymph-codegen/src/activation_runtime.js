const NYMPH_PUSH = Symbol("nymph.push");
const NYMPH_TAIL = Symbol("nymph.tail");
const NYMPH_RETURN = Symbol("nymph.return");
const NYMPH_SUSPEND = Symbol("nymph.suspend");
const NYMPH_DEFECT = Symbol("nymph.defect");
const NYMPH_RESUME = Symbol("nymph.resume");
const NYMPH_FRAME = Symbol("nymph.frame");
const NYMPH_CALLABLE = Symbol("nymph.callable");
const NYMPH_NO_DEFECT = Symbol("nymph.no-defect");
const NYMPH_PENDING_DEFECT = Symbol("nymph.pending-defect");
let nymphCurrentActivation = null;
let nymphNextFrameSlot = 0;

function nymphPush(callable, receiver, args, source, resumeState, resultSlot) {
	return { kind: NYMPH_PUSH, callable, receiver, args, source, resumeState, resultSlot };
}

function nymphTailCall(callable, receiver, args, source) {
	return { kind: NYMPH_TAIL, callable, receiver, args, source };
}

function nymphReturn(value) {
	return { kind: NYMPH_RETURN, value };
}

function nymphSuspend(effect, resumeState, resultSlot) {
	return { kind: NYMPH_SUSPEND, effect, resumeState, resultSlot };
}

function nymphDefect(defect) {
	return { kind: NYMPH_DEFECT, defect };
}

function nymphResume(value, resumeState, resultSlot) {
	return { kind: NYMPH_RESUME, value, resumeState, resultSlot };
}

function nymphTailCallMember(receiver, member, args, source) {
	return nymphTailCall(receiver[member], receiver, args, source);
}

function nymphCallable(step) {
	function callable(...args) {
		if (args.length === 1 && args[0]?.[NYMPH_FRAME] === true) return step.call(this, args[0]);
		return nymphActivate(callable, this, args, -1);
	}
	return nymphMarkCallable(callable);
}

function nymphCaptureFrame(liveLocals) {
	return { liveLocals };
}

function nymphMarkCallable(callable) {
	Object.defineProperty(callable, NYMPH_CALLABLE, { value: true });
	return callable;
}

function nymphRenderTasks(frame, initialMode) {
	if (frame.resumeState === 0) {
		frame.liveLocals[1] = [{ value: frame.liveLocals[0], mode: initialMode }];
		frame.liveLocals[2] = [];
		frame.resumeState = 2;
	}
	if (frame.resumeState === 1) {
		frame.liveLocals[2].push(frame.liveLocals[3].v);
		frame.resumeState = 2;
	}
	for (;;) {
		const task = frame.liveLocals[1].pop();
		if (task === undefined) return nymphReturn(new NString(frame.liveLocals[2].join("")));
		if (typeof task === "string") {
			frame.liveLocals[2].push(task);
			continue;
		}
		const { value, mode } = task;
		const member = mode === "display" ? "$nymph$display" : "$nymph$debug";
		const callable = value?.[member];
		if (callable?.[NYMPH_CALLABLE] === true) {
			return nymphPush(callable, value, [], -1, 1, 3);
		}
		if (typeof callable === "function") {
			frame.liveLocals[2].push(callable.call(value).v);
			continue;
		}
		const tag = nymphTagName(value);
		if (mode === "display" && (tag === "nymph.char" || tag === "nymph.string")) {
			frame.liveLocals[2].push(value.v);
			continue;
		}
		if (mode === "display" && ["string", "number", "bigint", "boolean"].includes(typeof value)) {
			frame.liveLocals[2].push(String(value));
			continue;
		}
		if (value === undefined) {
			frame.liveLocals[2].push("void");
		} else if (typeof value === "string") {
			frame.liveLocals[2].push(JSON.stringify(value));
		} else if (["number", "bigint", "boolean"].includes(typeof value)) {
			frame.liveLocals[2].push(String(value));
		} else if (tag === "nymph.int" || tag === "nymph.uint" || tag === "nymph.bool") {
			frame.liveLocals[2].push(String(value.v));
		} else if (tag === "nymph.float") {
			frame.liveLocals[2].push(Number.isInteger(value.v) ? value.v.toFixed(1) : String(value.v));
		} else if (tag === "nymph.char") {
			frame.liveLocals[2].push(
				`'${JSON.stringify(String(value.v)).slice(1, -1).replaceAll("'", "\\'")}'`,
			);
		} else if (tag === "nymph.string") {
			frame.liveLocals[2].push(JSON.stringify(value.v));
		} else {
			let entries;
			let open;
			let close;
			if (tag === "nymph.list" || tag === "nymph.tuple") {
				entries = Array.from(value.v);
				open = tag === "nymph.list" ? "#[" : "#(";
				close = tag === "nymph.list" ? "]" : ")";
			} else if (tag === "nymph.map") {
				entries = Array.from(value).flatMap(([key, item]) => [key, ": ", item]);
				open = "#{";
				close = "}";
			} else {
				const name = (tag ?? value?.constructor?.name ?? "Object").split("$").at(-1);
				const fields = value == null ? [] : Object.keys(value);
				entries = fields.flatMap((field) => [`${field}: `, value[field]]);
				open = fields.length === 0 ? name : `${name}(`;
				close = fields.length === 0 ? "" : ")";
			}
			frame.liveLocals[1].push(close);
			for (let index = entries.length - 1; index >= 0; index -= 1) {
				const entry = entries[index];
				frame.liveLocals[1].push(
					typeof entry === "string" && (entry === ": " || entry.endsWith(": "))
						? entry
						: { value: entry, mode: "debug" },
				);
				if (index > 0 && typeof entries[index - 1] !== "string") frame.liveLocals[1].push(", ");
			}
			frame.liveLocals[1].push(open);
		}
	}
}

const nymphProtocolDisplayStep = nymphCallable((frame) => nymphRenderTasks(frame, "display"));

function nymphOutputStep(frame, newline) {
	if (frame.resumeState === 0) {
		return nymphPush(nymphProtocolDisplayStep, undefined, [frame.liveLocals[0]], -1, 1, 1);
	}
	if (newline) console.log(frame.liveLocals[1].v);
	else process.stdout.write(frame.liveLocals[1].v);
	return nymphReturn(undefined);
}

const nymphPrintStep = nymphCallable((frame) => nymphOutputStep(frame, false));
const nymphPrintlnStep = nymphCallable((frame) => nymphOutputStep(frame, true));

function nymphMethodStep(receiver, member, args, step) {
	if (args.length === 1 && args[0]?.[NYMPH_FRAME] === true) return step.call(receiver, args[0]);
	return nymphActivate(receiver[member], receiver, Array.from(args), -1);
}

function nymphRegisterCleanup(cleanup) {
	const frame = nymphCurrentActivation?.frames.at(-1);
	if (frame === undefined) throw new Error("cleanup registration requires a Nymph activation");
	frame.cleanupScopes.at(-1).push(cleanup);
	return cleanup;
}

function nymphCommitStateTransition(headerDepth, replacements) {
	const frame = nymphCurrentActivation?.frames.at(-1);
	if (frame === undefined || frame.cleanupScopes.length <= headerDepth) {
		throw new Error("state transition requires a replacement cleanup scope");
	}
	const acquired = frame.cleanupScopes.pop();
	let primary = nymphUnwindScopes(frame, headerDepth);
	const header = frame.cleanupScopes.at(-1);
	const slots = [];
	for (let index = 0; index < replacements.length; index += 2) {
		const slot = header.indexOf(replacements[index]);
		if (slot < 0) throw new Error("state cleanup is not active");
		slots.push(slot);
	}
	for (let index = replacements.length - 2; index >= 0; index -= 2) {
		const oldCleanup = replacements[index];
		try {
			nymphRunCleanup(oldCleanup);
		} catch (cleanup) {
			primary = nymphCleanupDefect(primary, cleanup);
		}
	}
	if (primary !== NYMPH_NO_DEFECT) {
		for (const slot of slots.sort((left, right) => right - left)) header.splice(slot, 1);
		for (let index = acquired.length - 1; index >= 0; index -= 1) {
			try {
				nymphRunCleanup(acquired[index]);
			} catch (cleanup) {
				primary = nymphCleanupDefect(primary, cleanup);
			}
		}
		throw primary;
	}
	for (let index = 0; index < replacements.length; index += 2) {
		const newCleanup = replacements[index + 1];
		header[slots[index / 2]] = newCleanup;
	}
}

function nymphRunCleanup(cleanup) {
	const owner = nymphCurrentActivation;
	nymphCurrentActivation = null;
	try {
		const result = cleanup();
		if (result?.kind === "suspended") {
			throw new Error("Close.close must complete synchronously");
		}
		return result;
	} finally {
		nymphCurrentActivation = owner;
	}
}

function nymphEnterCleanupScope() {
	const frame = nymphCurrentActivation?.frames.at(-1);
	if (frame === undefined) throw new Error("cleanup scope entry requires a Nymph activation");
	frame.cleanupScopes.push([]);
}

function nymphCleanupDefect(primary, cleanup) {
	if (primary === NYMPH_NO_DEFECT) return cleanup;
	const defects = primary instanceof AggregateError ? [...primary.errors] : [primary];
	defects.push(cleanup);
	return new AggregateError(defects, "Nymph activation cleanup failed");
}

function nymphUnwindScopes(frame, targetDepth, primary = NYMPH_NO_DEFECT) {
	while (frame.cleanupScopes.length > targetDepth) {
		const cleanups = frame.cleanupScopes.pop();
		for (let index = cleanups.length - 1; index >= 0; index -= 1) {
			try {
				nymphRunCleanup(cleanups[index]);
			} catch (cleanup) {
				primary = nymphCleanupDefect(primary, cleanup);
			}
		}
	}
	return primary;
}

function nymphUnwindCleanupScopes(targetDepth) {
	const frame = nymphCurrentActivation?.frames.at(-1);
	if (frame === undefined) throw new Error("cleanup unwind requires a Nymph activation");
	const defect = nymphUnwindScopes(frame, targetDepth);
	if (defect !== NYMPH_NO_DEFECT) throw defect;
}

function nymphLeaveCleanupScope() {
	const frame = nymphCurrentActivation?.frames.at(-1);
	if (frame === undefined || frame.cleanupScopes.length === 1) {
		throw new Error("cleanup scope exit requires a nested Nymph cleanup scope");
	}
	const defect = nymphUnwindScopes(frame, frame.cleanupScopes.length - 1);
	if (defect !== NYMPH_NO_DEFECT) throw defect;
}

function nymphFrame(
	callable,
	receiver,
	args,
	source,
	resultSlot = null,
	slot = nymphNextFrameSlot++,
	executionFrame = null,
) {
	return {
		[NYMPH_FRAME]: true,
		callable,
		receiver,
		resumeState: 0,
		liveLocals: [...args],
		cleanupScopes: [[]],
		frameSlot: slot,
		resultSlot,
		source,
		context: executionFrame?.context ?? null,
		cancellation: executionFrame?.execution ?? null,
		signal: executionFrame?.signal ?? null,
	};
}

function nymphUnwindActivation(activation, primary) {
	while (activation.frames.length !== 0) {
		primary = nymphUnwindScopes(activation.frames.pop(), 0, primary);
	}
	return primary;
}

function nymphThrowActivationDefect(activation, primary) {
	if (activation.executionFrame !== null) {
		throw { [NYMPH_PENDING_DEFECT]: true, activation, primary };
	}
	throw nymphUnwindActivation(activation, primary);
}

function nymphIsPendingActivationDefect(value) {
	return value?.[NYMPH_PENDING_DEFECT] === true;
}

function nymphFinalizeActivationDefect(pending, primary = pending.primary) {
	return nymphUnwindActivation(pending.activation, primary);
}

function nymphSuspension(activation, packet, value) {
	let retained = activation;
	return Object.freeze({
		kind: "suspended",
		value,
		resume(result = value) {
			if (retained === null) throw new Error("Nymph suspension is already settled");
			const current = retained.frames.at(-1);
			current.liveLocals[packet.resultSlot] = result;
			const resumed = retained;
			retained = null;
			return nymphResumeActivation(resumed);
		},
		cancel(reason = new Error("Nymph activation cancelled")) {
			if (retained === null) throw new Error("Nymph suspension is already settled");
			const cancelled = retained;
			retained = null;
			throw nymphUnwindActivation(cancelled, reason);
		},
	});
}

function nymphDrive(activation) {
	for (;;) {
		const frame = activation.frames.at(-1);
		let outcome;
		try {
			outcome = frame.callable.apply(frame.receiver, [frame]);
		} catch (defect) {
			outcome = nymphDefect(defect);
		}
		if (outcome === null || typeof outcome !== "object") {
			outcome = nymphDefect(new Error("generated Nymph state returned a non-terminal value"));
		}
		switch (outcome.kind) {
			case NYMPH_PUSH:
				frame.resumeState = outcome.resumeState;
				if (outcome.callable?.[NYMPH_CALLABLE] === true) {
					activation.frames.push(
						nymphFrame(
							outcome.callable,
							outcome.receiver,
							outcome.args,
							outcome.source,
							outcome.resultSlot,
							undefined,
							activation.executionFrame,
						),
					);
				} else {
					try {
						frame.liveLocals[outcome.resultSlot] = outcome.callable.apply(
							outcome.receiver,
							outcome.args,
						);
					} catch (defect) {
						nymphThrowActivationDefect(activation, defect);
					}
				}
				break;
			case NYMPH_TAIL: {
				activation.frames.pop();
				const defect = nymphUnwindScopes(frame, 0);
				if (defect !== NYMPH_NO_DEFECT) nymphThrowActivationDefect(activation, defect);
				if (outcome.callable?.[NYMPH_CALLABLE] === true) {
					activation.frames.push(
						nymphFrame(
							outcome.callable,
							outcome.receiver,
							outcome.args,
							outcome.source,
							frame.resultSlot,
							frame.frameSlot,
							activation.executionFrame,
						),
					);
				} else {
					let value;
					try {
						value = outcome.callable.apply(outcome.receiver, outcome.args);
					} catch (externalDefect) {
						nymphThrowActivationDefect(activation, externalDefect);
					}
					if (activation.frames.length === 0) return value;
					activation.frames.at(-1).liveLocals[frame.resultSlot] = value;
				}
				break;
			}
			case NYMPH_RETURN: {
				activation.frames.pop();
				const defect = nymphUnwindScopes(frame, 0);
				if (defect !== NYMPH_NO_DEFECT) nymphThrowActivationDefect(activation, defect);
				if (activation.frames.length === 0) return outcome.value;
				activation.frames.at(-1).liveLocals[frame.resultSlot] = outcome.value;
				break;
			}
			case NYMPH_RESUME:
				frame.liveLocals[outcome.resultSlot] = outcome.value;
				frame.resumeState = outcome.resumeState;
				break;
			case NYMPH_SUSPEND: {
				frame.resumeState = outcome.resumeState;
				let value;
				try {
					value = typeof outcome.effect === "function" ? outcome.effect() : outcome.effect;
				} catch (defect) {
					nymphThrowActivationDefect(activation, defect);
				}
				return nymphSuspension(activation, outcome, value);
			}
			case NYMPH_DEFECT:
				nymphThrowActivationDefect(activation, outcome.defect);
				break;
			default:
				nymphThrowActivationDefect(activation, new Error("unknown Nymph activation terminal"));
		}
	}
}

function nymphResumeActivation(activation) {
	if (nymphCurrentActivation !== null)
		throw new Error("cannot resume a Nymph activation reentrantly");
	nymphCurrentActivation = activation;
	let result;
	try {
		result = nymphDrive(activation);
	} catch (defect) {
		nymphCurrentActivation = null;
		throw defect;
	}
	nymphCurrentActivation = null;
	return result;
}

function nymphActivate(callable, receiver, args, source, executionFrame = null) {
	if (nymphCurrentActivation !== null) {
		throw new Error("generated Nymph calls must be pushed by the activation driver");
	}
	return nymphResumeActivation({
		frames: [nymphFrame(callable, receiver, args, source, null, undefined, executionFrame)],
		executionFrame,
	});
}
