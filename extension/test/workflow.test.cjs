const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

test("CI cross-builds and packages every supported target", () => {
	const workflow = fs.readFileSync(path.join(__dirname, "..", "..", ".github", "workflows", "vscode.yml"), "utf8");
	for (const target of ["linux-x64", "linux-arm64", "win32-x64", "win32-arm64", "darwin-x64", "darwin-arm64"])
		assert.match(workflow, new RegExp(`vscode: ${target}`));
	assert.match(workflow, /cargo build --release -p nymph-lsp --target/);
	assert.match(workflow, /vsce package .*--target/);
	assert.match(workflow, /verify-vsix/);
	assert.match(workflow, /upload-artifact/);
});
