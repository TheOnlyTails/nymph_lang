const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { targetSpec } = require("./stage-server.cjs");

const extensionRoot = path.resolve(__dirname, "..");
const repositoryRoot = path.resolve(extensionRoot, "..");
const readmePath = path.join(extensionRoot, "README.md");
const readme = fs.readFileSync(readmePath, "utf8");
const ignoredDirectories = new Set([".vscode-test", "dist", "node_modules", "out", "server"]);
const markdownFiles = [];

function collectMarkdownFiles(directory, relativeDirectory = "") {
	for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
		const relativePath = path.join(relativeDirectory, entry.name);
		if (entry.isDirectory() && !ignoredDirectories.has(entry.name)) {
			collectMarkdownFiles(path.join(directory, entry.name), relativePath);
		} else if (entry.isFile() && entry.name.endsWith(".md")) {
			markdownFiles.push([relativePath, fs.readFileSync(path.join(directory, entry.name), "utf8")]);
		}
	}
}

collectMarkdownFiles(extensionRoot);

assert.deepEqual(
	markdownFiles.map(([name]) => name).sort((left, right) => left.localeCompare(right)),
	["CHANGELOG.md", "README.md", "test/fixtures/destination.md"],
	"README.md must be the extension's single maintained documentation surface apart from test fixtures",
);

const staleClaims = [
	["legacy source suffix", new RegExp(String.raw`\.nym` + "ph" + String.raw`\b`, "i")],
	["parent debug/release lookup", /target[\\/](?:debug|release)/i],
	["untargeted VSIX packaging", /vsce package(?![^\n]*--target)/i],
	["scaffold quickstart", /vsc-extension-quickstart/i],
];

for (const [file, contents] of markdownFiles) {
	for (const [description, pattern] of staleClaims)
		assert.doesNotMatch(contents, pattern, `${file} contains ${description}`);
}

const userReadme = readme.split(/^## Extension development\s*$/m, 1)[0];
for (const [file, contents] of markdownFiles) {
	const userContents = file === "README.md" ? userReadme : contents;
	assert.doesNotMatch(
		userContents,
		/cargo\s+build[\s\S]{0,200}nymph-lsp/i,
		`${file} tells end users to build a separate LSP`,
	);
}

function containsActivationDownloadClaim(contents) {
	const normalized = contents.replace(/\s+/g, " ");
	const mentionsActivation = /(?:activat(?:e|es|ed|ing|ion)|start(?:s|ed|ing|up))/i.test(
		normalized,
	);
	const mentionsDownload = /(?:download|fetch)(?:s|ed|ing)?/i.test(normalized);
	const deniesDownload =
		/\b(?:(?:does?|will|would|can|could|should|must) not|never) (?:download|fetch)(?:s|ed|ing)?/i.test(
			normalized,
		) ||
		/\bwithout (?:download|fetch)(?:ing)?/i.test(normalized) ||
		/\b(?:server|lsp) (?:is|are) not (?:download|fetch)(?:ed)?/i.test(normalized);
	return (
		mentionsActivation && mentionsDownload && /(?:server|lsp)/i.test(normalized) && !deniesDownload
	);
}

for (const allowed of [
	"The extension does not download the LSP at startup.",
	"The server is not fetched during activation.",
	"Startup works without downloading a server.",
]) {
	assert.equal(containsActivationDownloadClaim(allowed), false);
}
for (const forbidden of [
	"The extension downloads the LSP at startup.",
	"The server is downloaded during activation.",
	"At startup, the extension fetches the server.",
]) {
	assert.equal(containsActivationDownloadClaim(forbidden), true);
}

for (const [file, contents] of markdownFiles) {
	for (const paragraph of contents.split(/\n\s*\n/)) {
		if (containsActivationDownloadClaim(paragraph)) {
			assert.fail(`${file} contains an activation-time server download claim`);
		}
	}
}

const manifest = JSON.parse(fs.readFileSync(path.join(extensionRoot, "package.json"), "utf8"));
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

const vscodeFloorMatch = manifest.engines.vscode.match(/^(?:\^|>=)?(\d+)\.(\d+)(?:\.\d+)?$/);
assert.ok(vscodeFloorMatch, `unsupported VS Code engine range: ${manifest.engines.vscode}`);
const vscodeFloor = `${vscodeFloorMatch[1]}.${vscodeFloorMatch[2]}`;
assert.match(
	readme,
	new RegExp(String.raw`VS Code ${vscodeFloor.replace(".", String.raw`\.`)} or newer`, "i"),
	"README.md must match the VS Code compatibility floor in package.json",
);
assert.match(readme, /\.nym\b/);
assert.match(readme, /nymph\.server\.path/);
assert.match(readme, /Unsupported\s+Nymph LSP host/i);
assert.match(readme, /diagnostics[\s\S]{0,100}\*\*Problems\*\*/i);
assert.match(readme, /Nymph Language Server[\s\S]*?Output/i);
assert.match(readme, /Developer: Set Log Level[\s\S]{0,100}\*\*Trace\*\*/i);
assert.doesNotMatch(readme, /syntax errors[\s\S]{0,100}(?:Output|channel)/i);
assert.match(readme, /vsce package[^\n]*--target/i);

const functionReference = fs.readFileSync(
	path.join(repositoryRoot, "docs", "reference", "functions.md"),
	"utf8",
);
assert.match(functionReference, /`func name\(params\): ReturnType = body`/);
const snippets = markdownFiles
	.filter(([file]) => !file.startsWith(`test${path.sep}fixtures${path.sep}`))
	.flatMap(([file, contents]) =>
		[...contents.matchAll(/```(?:nym|nymph)(?:[ \t]+[^\n]*)?\r?\n([\s\S]*?)```/g)].map((match) => ({
			file,
			source: match[1],
		})),
	);
assert.ok(snippets.length > 0, "README.md must contain a Nymph example");
for (const { source } of snippets) {
	assert.doesNotMatch(source, /^\s*fn\b/m, "Nymph examples must not use obsolete fn syntax");
	assert.match(
		source,
		/^func\s+\w+\([^)]*\)(?::\s*[^=\n]+)?\s*=\s*.+/m,
		"Nymph examples must use func name(params): ReturnType = body syntax",
	);
}

for (const [file, contents] of markdownFiles) {
	for (const match of contents.matchAll(/\[[^\]]+\]\(([^)]+)\)/g)) {
		const destination = match[1];
		if (/^(?:https?:|mailto:)/.test(destination)) continue;
		const relativePath = decodeURIComponent(destination.split("#", 1)[0]);
		if (!relativePath) continue;
		const resolvedPath = path.resolve(extensionRoot, path.dirname(file), relativePath);
		const packagedPath = path.relative(extensionRoot, resolvedPath);
		assert.ok(
			packagedPath && !packagedPath.startsWith(`..${path.sep}`) && !path.isAbsolute(packagedPath),
			`${file} local link escapes the packaged extension: ${destination}`,
		);
		assert.ok(fs.existsSync(resolvedPath), `${file} has a broken local link: ${destination}`);
	}
}

const snippetDirectory = fs.mkdtempSync(path.join(os.tmpdir(), "nymph-extension-docs-"));
try {
	for (const [index, { file, source }] of snippets.entries()) {
		const snippetPath = path.join(snippetDirectory, `snippet-${index}.nym`);
		fs.writeFileSync(snippetPath, source);
		try {
			execFileSync("cargo", ["run", "--quiet", "-p", "nymph-cli", "--", "check", snippetPath], {
				cwd: repositoryRoot,
				stdio: "pipe",
			});
		} catch (error) {
			const details = Buffer.concat([
				error.stdout || Buffer.alloc(0),
				error.stderr || Buffer.alloc(0),
			])
				.toString()
				.trim();
			assert.fail(`${file} contains an invalid Nymph snippet${details ? `:\n${details}` : ""}`);
		}
	}
} finally {
	fs.rmSync(snippetDirectory, { recursive: true, force: true });
}

console.log(
	`Checked ${markdownFiles.length} Markdown files, ${snippets.length} Nymph snippet, and ${supportedTargets.length} packaging targets.`,
);
