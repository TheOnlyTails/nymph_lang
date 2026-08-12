import initCompiler, { inspect } from "../../wasm/nymph_wasm.js";

type InspectRequest = {
	id: number;
	source: string;
};

type WorkerResponse =
	| { type: "ready" }
	| { type: "result"; id: number; result: unknown }
	| { type: "error"; id?: number; message: string };

const compilerReady = initCompiler();

compilerReady.then(
	() => post({ type: "ready" }),
	(error: unknown) => post({ type: "error", message: errorMessage(error) }),
);

self.addEventListener("message", async (event: MessageEvent<InspectRequest>) => {
	try {
		await compilerReady;
		post({ type: "result", id: event.data.id, result: inspect(event.data.source) });
	} catch (error) {
		post({ type: "error", id: event.data.id, message: errorMessage(error) });
	}
});

function post(response: WorkerResponse) {
	self.postMessage(response);
}

function errorMessage(error: unknown) {
	return error instanceof Error ? error.message : String(error);
}
