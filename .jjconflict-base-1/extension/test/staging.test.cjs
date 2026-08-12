const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const { stageServer, targetSpec } = require("../scripts/stage-server.cjs");

test("maps all six VS Code targets to Rust targets and binary names", () => {
	assert.deepEqual(targetSpec, {
		"linux-x64": { rust: "x86_64-unknown-linux-gnu", binary: "nymph-lsp" },
		"linux-arm64": { rust: "aarch64-unknown-linux-gnu", binary: "nymph-lsp" },
		"win32-x64": { rust: "x86_64-pc-windows-gnu", binary: "nymph-lsp.exe" },
		"win32-arm64": { rust: "aarch64-pc-windows-gnullvm", binary: "nymph-lsp.exe" },
		"darwin-x64": { rust: "x86_64-apple-darwin", binary: "nymph-lsp" },
		"darwin-arm64": { rust: "aarch64-apple-darwin", binary: "nymph-lsp" },
	});
});

test("stages exactly one executable server", () => {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "nymph-stage-"));
	const source = path.join(root, "source");
	fs.writeFileSync(source, "server", { mode: 0o644 });
	const output = stageServer("linux-x64", source, root);
	assert.deepEqual(fs.readdirSync(path.join(root, "server")), ["nymph-lsp"]);
	assert.equal(fs.statSync(output).mode & 0o111, 0o111);
});
