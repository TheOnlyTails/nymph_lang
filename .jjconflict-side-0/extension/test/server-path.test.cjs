const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const { resolveServerPath, serverPayload } = require("../out/serverPath.js");

const targets = [
	["linux", "x64", "linux-x64", "nymph-lsp"],
	["linux", "arm64", "linux-arm64", "nymph-lsp"],
	["win32", "x64", "win32-x64", "nymph-lsp.exe"],
	["win32", "arm64", "win32-arm64", "nymph-lsp.exe"],
	["darwin", "x64", "darwin-x64", "nymph-lsp"],
	["darwin", "arm64", "darwin-arm64", "nymph-lsp"],
];

for (const [platform, arch, target, name] of targets) {
	test(`selects ${target}`, () =>
		assert.deepEqual(serverPayload(platform, arch), {
			target,
			relativePath: path.join("server", name),
		}));
}

test("rejects unsupported hosts actionably", () => {
	assert.throws(
		() => serverPayload("freebsd", "x64"),
		/Unsupported.*freebsd-x64.*six target-specific/i,
	);
});

test("uses an explicit development override", () => {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), "nymph-lsp-"));
	const binary = path.join(dir, "local-lsp");
	fs.writeFileSync(binary, "x", { mode: 0o755 });
	assert.equal(
		resolveServerPath({
			platform: "freebsd",
			arch: "x64",
			override: binary,
			asAbsolutePath: () => assert.fail(),
		}),
		binary,
	);
});

test("reports a missing development override without requiring a supported host", () => {
	assert.throws(
		() =>
			resolveServerPath({
				platform: "freebsd",
				arch: "x64",
				override: "/missing/local-lsp",
				asAbsolutePath: () => assert.fail(),
			}),
		/development override.*\/missing\/local-lsp/i,
	);
});

test("reports a missing bundled payload with its path and target", () => {
	assert.throws(
		() =>
			resolveServerPath({
				platform: "linux",
				arch: "x64",
				asAbsolutePath: (value) => `/extension/${value}`,
			}),
		/linux-x64.*\/extension\/server\/nymph-lsp.*reinstall/i,
	);
});

test("rejects a non-executable Unix payload actionably", () => {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), "nymph-lsp-"));
	const binary = path.join(dir, "nymph-lsp");
	fs.writeFileSync(binary, "x", { mode: 0o644 });
	assert.throws(
		() => resolveServerPath({ platform: "linux", arch: "x64", asAbsolutePath: () => binary }),
		/not executable.*chmod/i,
	);
});
