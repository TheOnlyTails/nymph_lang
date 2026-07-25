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
