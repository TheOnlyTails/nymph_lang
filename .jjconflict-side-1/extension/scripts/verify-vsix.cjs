const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { targetSpec } = require("./stage-server.cjs");

function verifyVsix(vsix, target) {
	const spec = targetSpec[target];
	if (!spec) throw new Error(`Unsupported VS Code target: ${target}`);
	const entries = execFileSync("unzip", ["-Z", "-1", vsix], { encoding: "utf8" })
		.trim()
		.split("\n");
	const servers = entries.filter((entry) => /(^|\/)extension\/server\//.test(entry));
	assert.deepEqual(
		servers,
		[`extension/server/${spec.binary}`],
		"VSIX must contain exactly its matching server binary",
	);
	assert.equal(
		entries.some((entry) => /(^|\/)(target|debug)(\/|$)/.test(entry)),
		false,
		"VSIX must not contain build output",
	);
	assert.equal(
		entries.some((entry) => entry.startsWith("extension/out/")),
		false,
		"VSIX must not contain unbundled TypeScript output",
	);
	if (!target.startsWith("win32-")) {
		const temp = fs.mkdtempSync(path.join(os.tmpdir(), "nymph-vsix-"));
		execFileSync("unzip", ["-qq", vsix, `extension/server/${spec.binary}`, "-d", temp]);
		assert.notEqual(
			fs.statSync(path.join(temp, "extension", "server", spec.binary)).mode & 0o111,
			0,
			"Unix server must be executable",
		);
	}
}

if (require.main === module) verifyVsix(process.argv[2], process.argv[3]);
module.exports = { verifyVsix };
