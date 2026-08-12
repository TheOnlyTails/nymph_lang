import assert from "node:assert/strict";

// Planning prototype only. These frozen records model the proposed ownership
// boundaries; they are not destination syntax or production runtime code.
const canonicalRow = (...effects) => Object.freeze([...new Set(effects)].sort());

const fileType = Object.freeze({
	kind: "ExternalNominal",
	definition: "package:std/module:fs/type:File",
});

const closeContract = Object.freeze({
	interface: "Close",
	effectParameters: ["E"],
	method: {
		name: "close",
		synchronous: true,
		result: "void",
		effects: canonicalRow("E"),
	},
});

const fileInterface = Object.freeze({
	type: fileType,
	implementations: [
		{
			interface: closeContract,
			arguments: { E: canonicalRow("Filesystem") },
		},
	],
	externals: [
		{
			name: "read",
			adapter: { module: "std/fs", symbol: "read" },
			effects: canonicalRow("Filesystem"),
			audit: { externalState: "Read", transaction: "Irreversible" },
			call: "Ordinary",
			parameters: [{ marshal: "OpaqueIdentity", type: fileType }],
			result: { marshal: "NymphAbi", type: "Result<string, FileError>" },
		},
		{
			name: "read_all",
			adapter: { module: "std/fs", symbol: "read_all" },
			effects: canonicalRow("Filesystem"),
			audit: { externalState: "Read", transaction: "Irreversible" },
			call: "Cancellable",
			parameters: [{ marshal: "OpaqueIdentity", type: fileType }],
			result: { marshal: "NymphAbi", type: "Result<string, FileError>" },
		},
		{
			name: "close",
			adapter: { module: "std/fs", symbol: "close" },
			effects: canonicalRow("Filesystem"),
			audit: { externalState: "Write", transaction: "Irreversible" },
			call: "Ordinary",
			parameters: [{ marshal: "OpaqueIdentity", type: fileType }],
			result: { marshal: "NymphAbi", type: "void" },
		},
	],
});

const lowerExternalCall = (external, args) => ({
	kind: "ExternalCall",
	adapter: external.adapter,
	arguments: args,
	marshalling: {
		parameters: external.parameters,
		result: external.result,
	},
	cancellation: external.call === "Cancellable" ? "ExecutionSignal" : "None",
});

const lowerCleanup = (external, resource) => ({
	kind: "CleanupCall",
	call: lowerExternalCall(external, [resource]),
});

const Ok = (value) => ({ tag: "Ok", value });
const Err = (error) => ({ tag: "Err", error });
const FileErrorClosed = Object.freeze({ type: "FileError", variant: "Closed" });
const NString = (value) => ({ type: "string", value });
const NInt = (value) => ({ type: "int", value });
const NExternal = (type, value) => ({ type, value });

const createHostFile = (contents) => ({
	// The external implementation—not the compiler—owns alias-shared state.
	state: { closed: false, contents },
});

const nodeAdapters = new Map([
	[
		"std/fs:read",
		(file) => (file.state.closed ? Err(FileErrorClosed) : Ok(NString(file.state.contents))),
	],
	[
		"std/fs:read_all",
		async (file, signal) => {
			await new Promise((resolve, reject) => {
				const timer = setTimeout(resolve, 5);
				signal.addEventListener(
					"abort",
					() => {
						clearTimeout(timer);
						reject(signal.reason);
					},
					{ once: true },
				);
			});
			return file.state.closed ? Err(FileErrorClosed) : Ok(NString(file.state.contents));
		},
	],
	[
		"std/fs:close",
		(file) => {
			file.state.closed = true;
		},
	],
]);

const adapterKey = ({ module, symbol }) => `${module}:${symbol}`;

const marshalArgument = (plan, value) => {
	switch (plan.marshal) {
		case "OpaqueIdentity":
		case "RawInt":
			return value.value;
		case "NymphAbi":
			return value;
		default:
			throw new Error(`unknown marshal plan ${plan.marshal}`);
	}
};

// Node-specific emission translates backend-neutral ExecutionSignal into the
// frame's AbortSignal. Ordinary adapters retain the existing args-only ABI.
const invokeNodeAdapter = (hir, frame) => {
	const adapter = nodeAdapters.get(adapterKey(hir.adapter));
	assert(adapter, `missing adapter ${adapterKey(hir.adapter)}`);
	const rawArgs = hir.arguments.map((argument, index) =>
		marshalArgument(hir.marshalling.parameters[index], argument),
	);
	return hir.cancellation === "ExecutionSignal"
		? adapter(...rawArgs, frame.abortSignal)
		: adapter(...rawArgs);
};

const read = fileInterface.externals.find((external) => external.name === "read");
const readAll = fileInterface.externals.find((external) => external.name === "read_all");
const close = fileInterface.externals.find((external) => external.name === "close");

assert.deepEqual(canonicalRow("Network", "Filesystem", "Network"), ["Filesystem", "Network"]);
assert.equal(fileInterface.type.kind, "ExternalNominal");
assert.equal(read.audit.externalState, "Read");
assert.equal(read.call, "Ordinary");
assert.equal(readAll.call, "Cancellable");
assert.deepEqual(closeContract.method.effects, ["E"]);
assert.match(JSON.stringify(fileInterface), /"effects":\["Filesystem"\]/);

const hostFile = createHostFile("nymph");
const file = NExternal(fileType, hostFile);
const alias = file;
const frame = { abortSignal: new AbortController().signal };
assert.equal(marshalArgument(read.parameters[0], file), hostFile);
assert.deepEqual(invokeNodeAdapter(lowerExternalCall(read, [alias]), frame), Ok(NString("nymph")));
assert.equal(lowerExternalCall(read, [file]).cancellation, "None");
assert.equal(lowerExternalCall(readAll, [file]).cancellation, "ExecutionSignal");

const cleanup = lowerCleanup(close, file);
assert.equal(cleanup.call.cancellation, "None");
invokeNodeAdapter(cleanup.call, frame);
invokeNodeAdapter(lowerExternalCall(close, [alias]), frame);
assert.deepEqual(invokeNodeAdapter(lowerExternalCall(read, [file]), frame), Err(FileErrorClosed));

const controller = new AbortController();
const pendingFile = NExternal(fileType, createHostFile("later"));
const pending = invokeNodeAdapter(lowerExternalCall(readAll, [pendingFile]), {
	abortSignal: controller.signal,
});
controller.abort(new Error("cancelled by execution"));
await assert.rejects(pending, /cancelled by execution/);

// Exact integer marshalling remains BigInt under the already settled contract.
const rawInt = marshalArgument({ marshal: "RawInt" }, NInt(9_007_199_254_740_993n));
assert.equal(typeof rawInt, "bigint");

// Trusted FFI defects are runtime outcomes at a spawned-execution boundary;
// adapters do not silently convert them to a declared Result.
const defectingAdapters = new Map([
	[
		"std/test:defect",
		() => {
			throw new TypeError("bad trusted ABI");
		},
	],
]);
const observeSpawned = async (operation) => {
	try {
		return { tag: "Produced", value: await operation() };
	} catch (defect) {
		return { tag: "Defected", defect };
	}
};
const outcome = await observeSpawned(() => defectingAdapters.get("std/test:defect")());
assert.equal(outcome.tag, "Defected");
assert.match(outcome.defect.message, /bad trusted ABI/);

console.log("issue 94 prototype: contract checks passed");
