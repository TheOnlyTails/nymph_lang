import readline from "node:readline";
import vm from "node:vm";

const PREFIX = "\x1enymph-repl:";
const context = vm.createContext({ console });
vm.runInContext(
	`globalThis[Symbol.for("nymph.transaction.journal")] = { stack: [], rollingBack: false };`,
	context,
);
const registry = new Map();
const sources = new Map();

function transaction(command) {
	vm.runInContext(command, context);
}

function reply(value) {
	process.stderr.write(`${PREFIX}${JSON.stringify(value)}\n`);
}

for await (const line of readline.createInterface({ input: process.stdin, crlfDelay: Infinity })) {
	let request;
	try {
		request = JSON.parse(line);
	} catch (error) {
		reply({ ok: false, error: `invalid worker request: ${error.message}` });
		continue;
	}

	const added = [];
	try {
		for (const [key, source] of Object.entries(request.modules)) {
			if (sources.has(key)) {
				if (sources.get(key) !== source) throw new Error(`committed module source changed: ${key}`);
				continue;
			}
			const module = new vm.SourceTextModule(source, {
				context,
				identifier: key,
				initializeImportMeta(meta) {
					meta.url = `nymph:${key}`;
				},
				importModuleDynamically() {
					throw new Error("asynchronous module loading is disabled in strict REPL mode");
				},
			});
			registry.set(key, module);
			sources.set(key, source);
			added.push(key);
		}

		const entry = registry.get(request.entry);
		if (!entry) throw new Error(`missing REPL entry module: ${request.entry}`);
		if (entry.status === "unlinked") {
			await entry.link((specifier) => {
				const dependency = registry.get(specifier);
				if (!dependency) throw new Error(`missing module ${specifier}`);
				return dependency;
			});
		}

		transaction(`globalThis[Symbol.for("nymph.transaction.journal")].stack.push([])`);
		try {
			if (entry.status !== "evaluated") await entry.evaluate();
			if (request.render !== null) {
				const render = entry.namespace[request.render];
				if (typeof render !== "function")
					throw new Error(`missing REPL render export: ${request.render}`);
				const value = render();
				if (value !== null && typeof value?.then === "function")
					throw new Error("asynchronous REPL rendering is disabled in strict REPL mode");
				const debug = entry.namespace["$nymph$replDebug"];
				if (typeof debug !== "function") throw new Error("missing REPL debug adapter");
				const rendered = debug(value);
				console.log(rendered.v);
			}
			transaction(`globalThis[Symbol.for("nymph.transaction.journal")].stack.pop()`);
			for (const key of added) {
				if (registry.get(key)?.status === "unlinked") {
					registry.delete(key);
					sources.delete(key);
				}
			}
			const retained = added.filter((key) => registry.has(key));
			reply({ ok: true, retained });
		} catch (error) {
			transaction(`{
				const journal = globalThis[Symbol.for("nymph.transaction.journal")];
				const entries = journal.stack.pop() ?? [];
				journal.rollingBack = true;
				let rollbackError;
				try {
					for (let index = entries.length - 1; index >= 0; index--) {
						try { entries[index](); } catch (failure) { rollbackError ??= failure; }
					}
				} finally { journal.rollingBack = false; }
				if (rollbackError !== undefined) throw rollbackError;
			}`);
			throw error;
		}
	} catch (error) {
		for (const key of added) {
			registry.delete(key);
			sources.delete(key);
		}
		reply({ ok: false, error: error?.stack ?? String(error) });
	}
}
