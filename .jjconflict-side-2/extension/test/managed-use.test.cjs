const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const grammar = JSON.parse(
	fs.readFileSync(path.join(__dirname, "..", "syntaxes", "nymph.tmLanguage.json"), "utf8"),
);

void test("TextMate grammar scopes use only as a let modifier", () => {
	const managedLet = grammar.repository.keywords.patterns[0];
	const pattern = new RegExp(managedLet.match, "u");

	assert.equal(managedLet.captures[1].name, "storage.type.nymph");
	assert.equal(managedLet.captures[2].name, "storage.modifier.use.nymph");
	assert.match("let use resource = acquire()", pattern);
	assert.doesNotMatch("use(resource)", pattern);
});
