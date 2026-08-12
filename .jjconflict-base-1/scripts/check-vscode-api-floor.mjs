import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const vscodeEngine = "^1.100.0";
const vscodeTypes = "1.100.0";

const manifest = JSON.parse(await readFile("extension/package.json", "utf8"));
const lockfile = await readFile("pnpm-lock.yaml", "utf8");

assert.equal(manifest.engines?.vscode, vscodeEngine, `engines.vscode must be ${vscodeEngine}`);
assert.equal(
	manifest.devDependencies?.["@types/vscode"],
	vscodeTypes,
	`@types/vscode must be pinned to ${vscodeTypes}`,
);

const extensionImporter = lockfile.match(/^  extension:\n(?<body>[\s\S]*?)(?=^  \S|^packages:)/m)
	?.groups?.body;
assert.ok(extensionImporter, "pnpm-lock.yaml must contain the extension importer");
assert.match(
	extensionImporter,
	new RegExp(
		`^      '@types/vscode':\\n        specifier: ${vscodeTypes.replaceAll(".", "\\.")}\\n        version: ${vscodeTypes.replaceAll(".", "\\.")}$`,
		"m",
	),
	`pnpm-lock.yaml must resolve extension @types/vscode to ${vscodeTypes}`,
);
assert.match(
	lockfile,
	new RegExp(`^  '@types/vscode@${vscodeTypes.replaceAll(".", "\\.")}':$`, "m"),
	`pnpm-lock.yaml must contain @types/vscode@${vscodeTypes}`,
);

console.log(`VS Code API floor is ${vscodeEngine} with @types/vscode ${vscodeTypes}`);
