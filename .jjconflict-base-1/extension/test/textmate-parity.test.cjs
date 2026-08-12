const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const extensionRoot = path.join(__dirname, "..");
const grammar = JSON.parse(
	fs.readFileSync(path.join(extensionRoot, "syntaxes", "nymph.tmLanguage.json"), "utf8"),
);
const injection = JSON.parse(
	fs.readFileSync(path.join(extensionRoot, "syntaxes", "nymph.codeblock.json"), "utf8"),
);
const fixture = (name) => fs.readFileSync(path.join(__dirname, "fixtures", name), "utf8");
const patternNamed = (repository, name) =>
	repository.patterns.find((pattern) => pattern.name === name);

void test("destination .nym fixture has TextMate fallbacks matching LSP token categories", () => {
	const source = fixture("destination.nym");
	const keyword = patternNamed(grammar.repository.keywords, "keyword.control.nymph");
	const spread = patternNamed(grammar.repository.operators, "keyword.operator.spread.nymph");
	const range = patternNamed(grammar.repository.operators, "keyword.operator.range.nymph");
	const pipe = patternNamed(grammar.repository.operators, "keyword.operator.pipe.nymph");
	const type = patternNamed(grammar.repository.keywords, "entity.name.type.nymph");
	const property = grammar.repository["struct-fields"].patterns[0];
	const member = grammar.repository["member-access"];

	// TextMate's conventional scope families are the lexical fallbacks for the
	// corresponding authoritative LSP semantic token categories.
	for (const [lexeme, rule, semanticCategory, scopeFamily] of [
		["loop", keyword, "keyword", "keyword."],
		["echo", keyword, "keyword", "keyword."],
		["...", spread, "operator", "keyword.operator."],
		["..=", range, "operator", "keyword.operator."],
		["|>", pipe, "operator", "keyword.operator."],
		["Point", type, "type", "entity.name.type."],
		["x", property, "property", "variable.other.property."],
		["origin", member, "method", "variable.other.member."],
	]) {
		assert.ok(source.includes(lexeme), `fixture must exercise ${lexeme}`);
		const sample = lexeme === "origin" ? ".origin" : lexeme === "x" ? "x =" : lexeme;
		assert.match(sample, new RegExp(rule.match, "u"));
		const scope = rule.name ?? rule.captures?.[2]?.name;
		assert.ok(scope.startsWith(scopeFamily), `${semanticCategory} fallback was ${scope}`);
	}
});

void test("Markdown injection embeds both nym and nymph fenced destination fixtures", () => {
	const markdown = fixture("destination.md");
	const block = injection.repository["nymph-code-block"];
	const javascriptPattern = block.begin
		.replace("(?i:", "(?:")
		.replaceAll("\\G", "^")
		.replaceAll("\\`", "`");
	const begin = new RegExp(javascriptPattern, "iu");
	assert.match("```nym", begin);
	assert.match('~~~nymph title="destination"', begin);
	assert.equal(block.patterns[0].contentName, "meta.embedded.block.nymph");
	assert.equal(block.patterns[0].patterns[0].include, "source.nymph");
	assert.match(markdown, /```nym[\s\S]*\n```/);
	assert.match(markdown, /~~~nymph[\s\S]*\n~~~/);
});
