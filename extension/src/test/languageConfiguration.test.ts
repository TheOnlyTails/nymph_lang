import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { parse } from "jsonc-parser";

interface LanguageConfiguration {
	comments?: {
		lineComment?: string;
		blockComment?: [string, string];
	};
}

interface TextMateGrammar {
	repository: {
		comments: {
			patterns: Array<{ name: string; match?: string; begin?: string; end?: string }>;
		};
	};
}

interface ExtensionManifest {
	contributes?: {
		configuration?: {
			properties?: Record<
				string,
				{ type?: string; enum?: string[]; default?: unknown; scope?: string }
			>;
		};
	};
}

void test("comment editing delimiters match the TextMate grammar", async () => {
	const extensionRoot = join(__dirname, "..", "..");
	const configuration = parse(
		await readFile(join(extensionRoot, "language-configuration.json"), "utf8"),
	) as LanguageConfiguration;
	const grammar = JSON.parse(
		await readFile(join(extensionRoot, "syntaxes", "nymph.tmLanguage.json"), "utf8"),
	) as TextMateGrammar;
	const patterns = grammar.repository.comments.patterns;
	const lineComment = patterns.find(
		(pattern) => pattern.name === "comment.line.double-slash.nymph",
	);
	const blockComment = patterns.find((pattern) => pattern.name === "comment.block.nymph");

	assert.equal(configuration.comments?.lineComment, lineComment?.match?.replace(".*$", ""));
	assert.deepEqual(
		configuration.comments?.blockComment,
		[blockComment?.begin, blockComment?.end].map((delimiter) => delimiter?.replace("\\*", "*")),
	);
});

void test("workspace diagnostics expose development and release compiler profiles", async () => {
	const extensionRoot = join(__dirname, "..", "..");
	const manifest = JSON.parse(
		await readFile(join(extensionRoot, "package.json"), "utf8"),
	) as ExtensionManifest;
	const profile = manifest.contributes?.configuration?.properties?.["nymph.buildProfile"];

	assert.equal(profile?.type, "string");
	assert.deepEqual(profile?.enum, ["development", "release"]);
	assert.equal(profile?.default, "development");
	assert.equal(profile?.scope, "window");

	const clientSource = await readFile(join(extensionRoot, "src", "extension.ts"), "utf8");
	assert.match(clientSource, /configurationSection:\s*"nymph"/);
});
