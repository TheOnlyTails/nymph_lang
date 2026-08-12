const fs = require("node:fs");
const path = require("node:path");

const targetSpec = {
	"linux-x64": { rust: "x86_64-unknown-linux-gnu", binary: "nymph-lsp" },
	"linux-arm64": { rust: "aarch64-unknown-linux-gnu", binary: "nymph-lsp" },
	"win32-x64": { rust: "x86_64-pc-windows-gnu", binary: "nymph-lsp.exe" },
	"win32-arm64": { rust: "aarch64-pc-windows-gnullvm", binary: "nymph-lsp.exe" },
	"darwin-x64": { rust: "x86_64-apple-darwin", binary: "nymph-lsp" },
	"darwin-arm64": { rust: "aarch64-apple-darwin", binary: "nymph-lsp" },
};

function stageServer(target, source, extensionRoot = path.resolve(__dirname, "..")) {
	const spec = targetSpec[target];
	if (!spec) throw new Error(`Unsupported VS Code target: ${target}`);
	const serverDir = path.join(extensionRoot, "server");
	fs.rmSync(serverDir, { recursive: true, force: true });
	fs.mkdirSync(serverDir, { recursive: true });
	const output = path.join(serverDir, spec.binary);
	fs.copyFileSync(source, output);
	if (!target.startsWith("win32-")) fs.chmodSync(output, 0o755);
	return output;
}

if (require.main === module) {
	const [target, explicitSource] = process.argv.slice(2);
	const spec = targetSpec[target];
	if (!spec) throw new Error(`Usage: stage-server.cjs <${Object.keys(targetSpec).join("|")}> [binary]`);
	const source = explicitSource || path.resolve(__dirname, "..", "..", "target", spec.rust, "release", spec.binary);
	stageServer(target, source);
}

module.exports = { stageServer, targetSpec };
