const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { targetSpec } = require("./stage-server.cjs");

const extensionRoot = path.resolve(__dirname, "..");
const repositoryRoot = path.resolve(extensionRoot, "..");
const readmePath = path.join(extensionRoot, "README.md");
const readme = fs.readFileSync(readmePath, "utf8");
const markdownFiles = fs
	.readdirSync(extensionRoot)
	.filter((name) => name.endsWith(".md"))
	.map((name) => [name, fs.readFileSync(path.join(extensionRoot, name), "utf8")]);

assert.deepEqual(
	markdownFiles.map(([name]) => name).sort((left, right) => left.localeCompare(right)),
	["CHANGELOG.md", "README.md"],
	"README.md must be the extension's single maintained documentation surface",
);

const staleClaims = [
	["legacy source suffix", new RegExp(String.raw`\.nym` + "ph" + String.raw`\b`, "i")],
	["parent debug/release lookup", /target\/(?:debug|release)/i],
	["one-line separate LSP build", /cargo build[^\n]*nymph-lsp/i],
	[
		"activation-time server download",
		/(?:activation|startup)[^\n]*(?:downloads?|fetches?)[^\n]*(?:server|lsp)/i,
	],
	["untargeted VSIX packaging", /vsce package(?![^\n]*--target)/i],
	["scaffold quickstart", /vsc-extension-quickstart/i],
];

for (const [file, contents] of markdownFiles) {
	for (const [description, pattern] of staleClaims)
		assert.doesNotMatch(contents, pattern, `${file} contains ${description}`);
}

const supportedTargets = Object.keys(targetSpec);
const documentedTargets = new Set(readme.match(/\b(?:linux|win32|darwin)-(?:x64|arm64)\b/g) || []);
assert.deepEqual(
	[...documentedTargets].sort((left, right) => left.localeCompare(right)),
	[...supportedTargets].sort((left, right) => left.localeCompare(right)),
	"README.md must document exactly the targets supported by the packaging scripts",
);
for (const [target, { binary }] of Object.entries(targetSpec)) {
	assert.match(
		readme,
		new RegExp(
			String.raw`\|[^\n]*\b${target}\b[^\n]*\bserver/${binary.replace(".", String.raw`\.`)}\b[^\n]*\|`,
		),
		`README.md must pair ${target} with server/${binary}`,
	);
}

assert.match(readme, /VS Code 1\.100/i);
assert.match(readme, /\.nym\b/);
assert.match(readme, /nymph\.server\.path/);
assert.match(readme, /Unsupported\s+Nymph LSP host/i);
assert.match(readme, /Nymph Language Server[\s\S]*?Output/i);
assert.match(readme, /vsce package[^\n]*--target/i);

const functionReference = fs.readFileSync(
	path.join(repositoryRoot, "docs", "reference", "functions.md"),
	"utf8",
);
assert.match(functionReference, /`func name\(params\): ReturnType = body`/);
const snippets = [...readme.matchAll(/```nym\n([\s\S]*?)```/g)].map((match) => match[1]);
assert.ok(snippets.length > 0, "README.md must contain a Nymph example");
for (const snippet of snippets) {
	assert.doesNotMatch(snippet, /^\s*fn\b/m, "Nymph examples must not use obsolete fn syntax");
	assert.match(
		snippet,
		/^func\s+\w+\([^)]*\)(?::\s*[^=\n]+)?\s*=\s*.+/m,
		"Nymph examples must use func name(params): ReturnType = body syntax",
	);
}

for (const match of readme.matchAll(/\[[^\]]+\]\(([^)]+)\)/g)) {
	const destination = match[1];
	if (/^(?:https?:|mailto:)/.test(destination)) continue;
	const relativePath = decodeURIComponent(destination.split("#", 1)[0]);
	if (!relativePath) continue;
	assert.ok(
		fs.existsSync(path.resolve(extensionRoot, relativePath)),
		`README.md has a broken local link: ${destination}`,
	);
}

console.log(
	`Checked ${markdownFiles.length} Markdown files, ${snippets.length} Nymph snippet, and ${supportedTargets.length} packaging targets.`,
);
