type RunRequest = { js: string };

for (const level of ["log", "info", "warn", "error"] as const) {
	console[level] = (...values: unknown[]) => {
		self.postMessage({ type: "output", level, text: values.map(formatValue).join(" ") });
	};
}

self.addEventListener("message", async (event: MessageEvent<RunRequest>) => {
	const url = URL.createObjectURL(new Blob([event.data.js], { type: "text/javascript" }));
	try {
		const module = (await import(/* @vite-ignore */ url)) as { main?: () => unknown };
		if (typeof module.main !== "function") {
			throw new Error("No exported `main` function. Add `func main() = ...` to run this module.");
		}
		const value = await module.main();
		self.postMessage({ type: "result", text: formatValue(value) });
	} catch (error) {
		self.postMessage({ type: "runtime-error", text: errorMessage(error) });
	} finally {
		URL.revokeObjectURL(url);
	}
});

function formatValue(value: unknown): string {
	if (value === undefined) return "void";
	if (typeof value === "string") return value;
	if (typeof value === "symbol") return value.toString();
	if (typeof value === "object" && value !== null && "v" in value) {
		return formatValue((value as { v: unknown }).v);
	}
	try {
		if (typeof value === "object") return JSON.stringify(value) ?? "null";
		return String(value as string | number | boolean | bigint | null);
	} catch {
		return "[unserializable value]";
	}
}

function errorMessage(error: unknown) {
	return error instanceof Error ? `${error.name}: ${error.message}` : String(error);
}
