function nymphEchoPlaceholder(value) {
	if (typeof value === "function") return "<function>";
	return "<opaque external>";
}

function nymphEchoRender(value, seen = new WeakSet()) {
	if (value === undefined) return "void";
	if (value === null) return "<opaque external>";
	if (typeof value === "function") return "<function>";
	if (typeof value !== "object") return nymphEchoPlaceholder(value);
	if (seen.has(value)) return "<cycle>";
	seen.add(value);
	try {
		if (nymphEchoBoxes.has(value)) {
			const tag = value[NYMPH_TAG]?.description;
			if (tag === "nymph.int" || tag === "nymph.uint" || tag === "nymph.bool")
				return String(value.v);
			if (tag === "nymph.float")
				return Number.isInteger(value.v) ? value.v.toFixed(1) : String(value.v);
			if (tag === "nymph.char")
				return `'${JSON.stringify(value.v).slice(1, -1).replaceAll("'", "\\'")}'`;
			if (tag === "nymph.string") return JSON.stringify(value.v);
			if (tag === "nymph.list")
				return `#[${[...value.v].map((item) => nymphEchoRender(item, seen)).join(", ")}]`;
			if (tag === "nymph.tuple")
				return `#(${value.v.map((item) => nymphEchoRender(item, seen)).join(", ")})`;
			if (tag === "nymph.map")
				return `#{${[...value.v]
					.map(([key, item]) => `${nymphEchoRender(key, seen)}: ${nymphEchoRender(item, seen)}`)
					.join(", ")}}`;
			return "<opaque external>";
		}
		const shape = nymphEchoStructuralShapes.get(value);
		if (shape === undefined) return "<opaque external>";
		const [kind, name] = shape.identity.split(":", 2);
		const displayName = kind === "variant" ? name : name.split("$").at(-1);
		if (shape.fields.length === 0) return displayName;
		return `${displayName}(${shape.fields
			.map((field) => `${field}: ${nymphEchoRender(value[field], seen)}`)
			.join(", ")})`;
	} finally {
		seen.delete(value);
	}
}

function nymphEcho(value, site) {
	let rendered;
	try {
		rendered = nymphEchoRender(value);
	} catch {
		rendered = nymphEchoPlaceholder(value);
	}
	const plainLocation = `${site.file}:${site.line}:${site.column}`;
	let location = plainLocation;
	try {
		if (site.uri !== null && globalThis.process?.stderr?.isTTY === true) {
			const uri = [...site.uri]
				.filter((char) => char.codePointAt(0) > 31 && char.codePointAt(0) !== 127)
				.join("");
			location = `\u001b]8;;${uri}#L${site.line}:${site.column}\u001b\\${plainLocation}\u001b]8;;\u001b\\`;
		}
		globalThis.process?.stderr?.write(`${location}: ${rendered}\n`);
	} catch {
		// Compiler observations never affect program control flow.
	}
	return value;
}
