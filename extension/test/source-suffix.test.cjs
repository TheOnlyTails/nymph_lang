const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const extensionRoot = path.resolve(__dirname, "..");
const repositoryRoot = path.resolve(extensionRoot, "..");

void test(".nym is the extension's sole source suffix", () => {
	const manifest = JSON.parse(fs.readFileSync(path.join(extensionRoot, "package.json"), "utf8"));
	const nymphLanguage = manifest.contributes.languages.find(({ id }) => id === "nymph");

	assert.ok(nymphLanguage, "the nymph language contribution must exist");
	assert.deepEqual(nymphLanguage.extensions, [".nym"]);
	assert.equal(manifest.contributes.grammars[0].scopeName, "source.nymph");
	assert.equal(manifest.contributes.grammars[1].scopeName, "markdown.nymph.codeblock");

	const clientSource = fs.readFileSync(path.join(extensionRoot, "src", "extension.ts"), "utf8");
	assert.match(clientSource, /language:\s*["']nymph["']/);
	assert.match(clientSource, /createFileSystemWatcher\(["']\*\*\/\*\.nym["']\)/);
	assert.ok(!clientSource.includes(`.nym${"ph"}`));
});

void test("maintained guidance rejects the legacy suffix without renaming nymph scopes", () => {
	const roots = ["extension", ".vscode", "examples", "docs", "reference"];
	const ignoredDirectories = new Set([".vscode-test", "node_modules", "out", "server"]);
	const scopeKeys = new Set(["contentName", "include", "name", "scopeName"]);
	const recognizedScopes = new Set();
	const manifest = JSON.parse(fs.readFileSync(path.join(extensionRoot, "package.json"), "utf8"));
	const recognizedLanguageIdentifiers = new Set(
		manifest.contributes.languages.flatMap(({ id }) => [`lspconfig.${id}.setup`, `.${id}.setup`]),
	);
	const offenders = [];
	const legacySuffix = `.nym${"ph"}`;
	const legacyToken = new RegExp(
		String.raw`[A-Za-z0-9_./:\\*-]*` +
			legacySuffix.replace(".", String.raw`\.`) +
			String.raw`(?:\.[A-Za-z][\w-]*)*(?![\w])`,
		"g",
	);

	function collectScopes(value, key) {
		if (typeof value === "string") {
			if (scopeKeys.has(key)) {
				for (const scope of value.split(/\s*,\s*/)) recognizedScopes.add(scope);
			}
		} else if (Array.isArray(value)) {
			for (const child of value) collectScopes(child);
		} else if (value && typeof value === "object") {
			for (const [childKey, child] of Object.entries(value)) collectScopes(child, childKey);
		}
	}

	for (const grammarFile of ["nymph.tmLanguage.json", "nymph.codeblock.json"]) {
		collectScopes(
			JSON.parse(fs.readFileSync(path.join(extensionRoot, "syntaxes", grammarFile), "utf8")),
		);
	}

	function visit(relativePath) {
		const absolutePath = path.join(repositoryRoot, relativePath);
		for (const entry of fs.readdirSync(absolutePath, { withFileTypes: true })) {
			const child = path.join(relativePath, entry.name);
			if (entry.isDirectory()) {
				if (!ignoredDirectories.has(entry.name)) visit(child);
			} else {
				const contents = fs.readFileSync(path.join(repositoryRoot, child), "utf8");
				for (const match of contents.matchAll(legacyToken)) {
					if (!recognizedScopes.has(match[0]) && !recognizedLanguageIdentifiers.has(match[0])) {
						offenders.push(`${child}: ${match[0]}`);
					}
				}
			}
		}
	}

	for (const root of roots) visit(root);
	assert.deepEqual(
		offenders,
		[],
		`forbidden legacy source suffix found in:\n${offenders.join("\n")}`,
	);
});
