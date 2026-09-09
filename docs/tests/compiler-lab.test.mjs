import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const componentUrl = new URL("../.vitepress/theme/components/NymphDebugger.vue", import.meta.url);
const component = await readFile(componentUrl, "utf8");

await test("compiler lab exposes macro expansion and token highlighting", () => {
	assert.match(component, /expanded: string \| null/);
	assert.match(component, /expanded_tokens: Token\[\]/);
	assert.match(component, /"Macro Expansion"/);
	assert.match(component, /result\?\.expanded/);
	assert.match(component, /highlightedExpansion/);
});

await test("macro expansion panel hides stale source while compiling or after failure", () => {
	assert.match(component, /v-if="compiling"[^>]*>Updating macro expansion…/);
	assert.match(component, /v-else-if="result\?\.expanded"/);
	assert.match(component, /result\.value = null;/);
	assert.match(component, /Macro expansion is available after a clean expansion and analysis\./);
});

await test("DOM tabs use the exact user-facing labels and order", () => {
	const tabs = component.match(/const tabs = \[(.*?)\] as const;/s);
	assert.ok(tabs, "expected the component's tab source of truth");
	assert.match(component, /v-for="tab in tabs"/);
	assert.match(component, /\{\{ tab\.label \}\}/);
	const labels = [...tabs[1].matchAll(/label: "([^"]+)"/g)].map((match) => match[1]);
	assert.deepEqual(labels, [
		"Console",
		"Diagnostics",
		"Tokens",
		"AST",
		"Types",
		"Macro Expansion",
		"JavaScript",
	]);
	assert.doesNotMatch(component, /Expanded Nymph/);
	assert.doesNotMatch(component, /Javascript/);
});

await test("narrow tabs wrap without clipping and diagnostics use the canonical pretty output", () => {
	assert.match(component, /\.tabs \{[^}]*height: auto;[^}]*flex-wrap: wrap;/s);
	assert.match(component, /\.tabs button \{[^}]*padding: 0 4px;/s);
	assert.match(component, /\.tabs button \{[^}]*flex: 0 0 auto;/s);
	assert.match(component, /\.pipeline \{[^}]*grid-template-columns: 1fr;/s);
	assert.match(component, /\.diagnostic-output \{[^}]*height: auto;[^}]*overflow: visible;/s);
	assert.match(component, /<pre><code>\{\{ diagnostic\.pretty \}\}<\/code><\/pre>/);
	assert.match(
		component,
		/\.diagnostic pre \{[^}]*max-width: 100%;[^}]*overflow-wrap: anywhere;[^}]*white-space: pre-wrap;/s,
	);
});
