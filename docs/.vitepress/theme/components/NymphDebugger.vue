<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import AstTreeNode, { type AstTreeNode as AstNode } from "./AstTreeNode.vue";
import TypeTreeNode, { type TypeTreeNode as TypeNode } from "./TypeTreeNode.vue";

type StageStatus = "complete" | "failed" | "blocked";
type Stage = { name: string; status: StageStatus; detail: string };
type Token = {
	kind: string;
	text: string;
	start: number;
	end: number;
	line: number;
	col: number;
};
type Diagnostic = {
	severity: "error" | "warning" | "info" | "hint";
	message: string;
	code: string;
	start: number;
	end: number;
	start_line: number;
	start_col: number;
	end_line: number;
	end_col: number;
};
type TypeState = {
	node: number;
	parent: number | null;
	source: string;
	type_: string;
	dispatch: string | null;
	method: string | null;
	start: number;
	end: number;
	line: number;
	col: number;
};
type Inspection = {
	tokens: Token[];
	ast: string;
	types: TypeState[];
	stages: Stage[];
	js: string | null;
	run: { js: string; root_kind: "void" | "option" | "result"; task: boolean } | null;
	diagnostics: Diagnostic[];
};
type HighlightSegment = {
	text: string;
	classes: string[];
	diagnostics: Diagnostic[];
};
type DiagnosticPopup = {
	diagnostics: Diagnostic[];
	x: number;
	y: number;
	placement: "above" | "below";
};
type WorkerResponse =
	| { type: "ready" }
	| { type: "result"; id: number; result: Inspection }
	| { type: "error"; id?: number; message: string };
type RuntimeResponse =
	| { type: "output"; level: "log" | "info" | "warn" | "error"; text: string }
	| { type: "result"; text: string }
	| { type: "runtime-error"; text: string };
type ConsoleLine = {
	level: "log" | "info" | "warn" | "error" | "result" | "system";
	text: string;
};

const examples = {
	Functions: `func fibonacci(n: int): int = match (n) {
	..=1 -> n,
	_ -> fibonacci(n - 1) + fibonacci(n - 2),
}

func main(): void = { let value = echo fibonacci(10) }
`,
	Types: `enum Shape {
	Circle(radius: float),
	Rectangle(width: float, height: float),
}

func area(shape: Shape): float = match (shape) {
	Shape.Circle(radius) -> 3.14159 * radius ** 2,
	Shape.Rectangle(width, height) -> width * height,
}
`,
	Async: `async func main(): void = {
	let exact = echo 18446744073709551615u
}
`,
	"Type error": `func greet(name: string): string = "Hello, " + name

func broken(): int = greet("Nymph")
`,
};

const source = ref(examples.Functions);
const result = ref<Inspection | null>(null);
const loading = ref(true);
const compiling = ref(false);
const failure = ref("");
const activeTab = ref<"tokens" | "ast" | "types" | "diagnostics" | "console" | "javascript">(
	"tokens",
);
const editor = ref<HTMLTextAreaElement | null>(null);
const highlightedEditor = ref<HTMLElement | null>(null);
const diagnosticPopup = ref<DiagnosticPopup | null>(null);
const running = ref(false);
const consoleLines = ref<ConsoleLine[]>([]);
let compilerWorker: Worker | undefined;
let runtimeWorker: Worker | undefined;
let nextRequestId = 0;
let latestRequestId = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

const keywordKinds = new Set([
	"Public",
	"Internal",
	"Private",
	"Import",
	"With",
	"Type",
	"Struct",
	"Enum",
	"Let",
	"Mut",
	"External",
	"Func",
	"Interface",
	"Impl",
	"Namespace",
	"For",
	"While",
	"If",
	"Else",
	"Match",
	"Continue",
	"Break",
	"Return",
	"This",
	"In",
	"As",
	"Is",
	"Async",
	"Await",
]);

const errors = computed(
	() => result.value?.diagnostics.filter((item) => item.severity === "error").length ?? 0,
);
const warnings = computed(
	() => result.value?.diagnostics.filter((item) => item.severity === "warning").length ?? 0,
);
const astTree = computed<AstNode[]>(() => parseAstTree(result.value?.ast ?? ""));
const typeTree = computed<TypeNode[]>(() => {
	const entries = result.value?.types ?? [];
	const nodes = new Map<number, TypeNode>(
		entries.map((entry) => [entry.node, { ...entry, children: [] }]),
	);
	const roots: TypeNode[] = [];

	for (const entry of entries) {
		const node = nodes.get(entry.node);
		const parent = entry.parent === null ? undefined : nodes.get(entry.parent);
		if (!node) continue;
		if (parent) parent.children.push(node);
		else roots.push(node);
	}

	return roots;
});
const highlightedSource = computed<HighlightSegment[]>(() => {
	const bytes = new TextEncoder().encode(source.value);
	const tokens = result.value?.tokens ?? [];
	const diagnostics = result.value?.diagnostics ?? [];
	const boundaries = new Set([0, bytes.length]);

	for (const token of tokens) {
		boundaries.add(Math.min(token.start, bytes.length));
		boundaries.add(Math.min(token.end, bytes.length));
	}
	for (const diagnostic of diagnostics) {
		boundaries.add(Math.min(diagnostic.start, bytes.length));
		boundaries.add(Math.min(Math.max(diagnostic.end, diagnostic.start + 1), bytes.length));
	}

	const offsets = [...boundaries].sort((left, right) => left - right);
	return offsets.slice(0, -1).map((start, index) => {
		const end = offsets[index + 1];
		const token = tokens.find((item) => item.start <= start && item.end >= end);
		const overlappingDiagnostics = diagnostics.filter(
			(item) => item.start < end && Math.max(item.end, item.start + 1) > start,
		);
		const classes = token ? [syntaxClass(token.kind)] : [];
		classes.push(...overlappingDiagnostics.map((item) => `inline-${item.severity}`));

		return {
			text: new TextDecoder().decode(bytes.slice(start, end)),
			classes,
			diagnostics: overlappingDiagnostics,
		};
	});
});

function syntaxClass(kind: string) {
	if (kind.startsWith("Identifier") || kind.startsWith("AnonymousParam"))
		return "syntax-identifier";
	if (kind.startsWith("Str(") || kind.startsWith("Char(")) return "syntax-string";
	if (/^(Int|UInt|Float)\(/.test(kind)) return "syntax-number";
	if (kind === "True" || kind === "False") return "syntax-boolean";
	if (kind.endsWith("Type")) return "syntax-type";
	if (keywordKinds.has(kind)) return "syntax-keyword";
	if (/^(L|R|HashL)(Paren|Bracket|Brace)$/.test(kind)) return "syntax-delimiter";
	return "syntax-operator";
}

function parseAstTree(ast: string): AstNode[] {
	const roots: AstNode[] = [];
	const parents: AstNode[][] = [roots];
	let id = 0;

	for (const rawLine of ast.split("\n")) {
		const line = rawLine.trim();
		if (!line) continue;

		if (/^[\]})],?$/.test(line)) {
			parents.pop();
			continue;
		}

		const opensChildren = /[({[]$/.test(line);
		const node: AstNode = {
			id: id++,
			label: opensChildren ? line.slice(0, -1).trimEnd() : line.replace(/,$/, ""),
			children: [],
		};
		parents.at(-1)?.push(node);
		if (opensChildren) parents.push(node.children);
	}

	return roots;
}

function runInspection() {
	if (!compilerWorker) return;
	latestRequestId = ++nextRequestId;
	compilerWorker.postMessage({ id: latestRequestId, source: source.value });
}

function scheduleInspection() {
	clearTimeout(timer);
	stopProgram(false);
	compiling.value = true;
	timer = setTimeout(runInspection, 180);
}

function runProgram() {
	stopProgram(false);
	activeTab.value = "console";
	consoleLines.value = [];
	if (compiling.value) {
		consoleLines.value.push({ level: "system", text: "Wait for compilation to finish." });
		return;
	}
	if (!result.value?.run) {
		consoleLines.value.push({
			level: "system",
			text: "Add a valid main returning void, Option<void>, Result<void, E>, or an async Task form.",
		});
		return;
	}

	running.value = true;
	runtimeWorker = new Worker(new URL("./runtime.worker.ts", import.meta.url), { type: "module" });
	runtimeWorker.addEventListener("message", (event: MessageEvent<RuntimeResponse>) => {
		const response = event.data;
		if (response.type === "output") {
			consoleLines.value.push({ level: response.level, text: response.text });
			return;
		}
		consoleLines.value.push({
			level: response.type === "result" ? "result" : "error",
			text: response.type === "result" ? `Result: ${response.text}` : response.text,
		});
		running.value = false;
		runtimeWorker?.terminate();
		runtimeWorker = undefined;
	});
	runtimeWorker.addEventListener("error", (event) => {
		consoleLines.value.push({ level: "error", text: event.message });
		running.value = false;
	});
	runtimeWorker.postMessage({
		js: result.value.run.js,
		root_kind: result.value.run.root_kind,
		task: result.value.run.task,
	});
}

function handleEditorKeydown(event: KeyboardEvent) {
	if (event.key !== "Enter" || (!event.ctrlKey && !event.metaKey)) return;
	event.preventDefault();
	runProgram();
}

function stopProgram(report = true) {
	if (!runtimeWorker) return;
	runtimeWorker.terminate();
	runtimeWorker = undefined;
	running.value = false;
	if (report) consoleLines.value.push({ level: "system", text: "Execution stopped." });
}

function selectRange(start: number, end: number) {
	const bytes = new TextEncoder().encode(source.value);
	const startIndex = new TextDecoder().decode(bytes.slice(0, start)).length;
	const endIndex = new TextDecoder().decode(bytes.slice(0, end)).length;
	nextTick(() => {
		editor.value?.focus();
		editor.value?.setSelectionRange(startIndex, Math.max(startIndex + 1, endIndex));
	});
}

function syncEditorScroll() {
	if (!editor.value || !highlightedEditor.value) return;
	highlightedEditor.value.scrollTop = editor.value.scrollTop;
	highlightedEditor.value.scrollLeft = editor.value.scrollLeft;
}

function showDiagnosticPopup(event: MouseEvent, diagnostics: Diagnostic[]) {
	if (!diagnostics.length) return;
	const bounds = (event.currentTarget as HTMLElement).getBoundingClientRect();
	const placement = bounds.top > 150 ? "above" : "below";
	diagnosticPopup.value = {
		diagnostics,
		x: Math.max(16, Math.min(bounds.left, window.innerWidth - 376)),
		y: placement === "above" ? bounds.top - 9 : bounds.bottom + 9,
		placement,
	};
}

function chooseExample(name: keyof typeof examples) {
	source.value = examples[name];
}

watch(source, scheduleInspection);

onMounted(() => {
	compilerWorker = new Worker(new URL("./compiler.worker.ts", import.meta.url), { type: "module" });
	compilerWorker.addEventListener("message", (event: MessageEvent<WorkerResponse>) => {
		const response = event.data;
		if (response.type === "ready") {
			if (latestRequestId === 0) runInspection();
			return;
		}
		if (response.type === "result") {
			if (response.id !== latestRequestId) return;
			result.value = response.result;
			failure.value = "";
			loading.value = false;
			compiling.value = false;
			return;
		}
		if (response.id === undefined || response.id === latestRequestId) {
			failure.value = `Could not run the compiler: ${response.message}`;
			loading.value = false;
			compiling.value = false;
		}
	});
	compilerWorker.addEventListener("error", (event) => {
		failure.value = `Could not load the compiler worker: ${event.message}`;
		loading.value = false;
		compiling.value = false;
	});
});

onBeforeUnmount(() => {
	clearTimeout(timer);
	stopProgram(false);
	compilerWorker?.terminate();
	compilerWorker = undefined;
});
</script>

<template>
	<div class="compiler-lab">
		<div class="lab-toolbar">
			<div class="example-buttons" aria-label="Example programs">
				<span>Examples</span>
				<button
					v-for="(_, name) in examples"
					:key="name"
					type="button"
					@click="chooseExample(name)"
				>
					{{ name }}
				</button>
			</div>
			<div class="lab-actions">
				<div class="compiler-state" aria-live="polite">
					<span v-if="loading" class="loading-dot"></span>
					{{ loading ? "Loading compiler…" : `${errors} errors · ${warnings} warnings` }}
				</div>
				<button v-if="running" type="button" class="stop-button" @click="stopProgram()">
					■ Stop
				</button>
				<button
					v-else
					type="button"
					class="run-button"
					:disabled="loading || compiling"
					title="Run (Ctrl/⌘ + Enter)"
					@click="runProgram"
				>
					▶ Run
				</button>
			</div>
		</div>

		<div v-if="failure" class="lab-failure" role="alert">{{ failure }}</div>

		<div class="pipeline" aria-label="Compilation stages">
			<template v-for="(stage, index) in result?.stages ?? []" :key="stage.name">
				<article class="stage" :class="`is-${stage.status}`">
					<div class="stage-number">{{ index + 1 }}</div>
					<div>
						<strong>{{ stage.name }}</strong>
						<small>{{ stage.detail }}</small>
					</div>
				</article>
				<span v-if="index < (result?.stages.length ?? 0) - 1" class="stage-arrow" aria-hidden="true"
					>→</span
				>
			</template>
		</div>

		<div class="lab-grid">
			<section class="lab-panel source-panel">
				<header><span>Nymph source</span><small>playground.nym</small></header>
				<div class="editor-shell">
					<pre ref="highlightedEditor" class="source-highlight" aria-hidden="true"><code><span
						v-for="(segment, index) in highlightedSource"
						:key="index"
						:class="segment.classes"
						@mouseenter="showDiagnosticPopup($event, segment.diagnostics)"
						@mouseleave="diagnosticPopup = null"
					>{{ segment.text }}</span></code></pre>
					<textarea
						ref="editor"
						v-model="source"
						aria-label="Nymph source code"
						spellcheck="false"
						@keydown="handleEditorKeydown"
						@scroll="syncEditorScroll"
					></textarea>
				</div>
			</section>

			<section class="lab-panel output-panel">
				<div class="tabs" role="tablist" aria-label="Compiler output">
					<button
						v-for="tab in [
							'tokens',
							'ast',
							'types',
							'diagnostics',
							'console',
							'javascript',
						] as const"
						:key="tab"
						type="button"
						role="tab"
						:aria-selected="activeTab === tab"
						@click="activeTab = tab"
					>
						{{ tab === "ast" ? "AST" : tab[0].toUpperCase() + tab.slice(1) }}
						<span v-if="tab === 'diagnostics'" class="tab-count">
							{{ result?.diagnostics.length ?? 0 }}
						</span>
						<i v-if="tab === 'console' && running" class="loading-dot" aria-label="Running"></i>
					</button>
				</div>

				<div v-if="activeTab === 'tokens'" class="tokens" role="tabpanel">
					<button
						v-for="(token, index) in result?.tokens ?? []"
						:key="`${token.start}-${index}`"
						type="button"
						class="token"
						:title="`${token.kind} · bytes ${token.start}–${token.end}`"
						@click="selectRange(token.start, token.end)"
					>
						<code>{{ token.text }}</code>
						<small>{{ token.line }}:{{ token.col }}</small>
					</button>
					<p v-if="!result?.tokens.length" class="empty-state">No tokens yet.</p>
				</div>
				<div v-else-if="activeTab === 'ast'" class="ast-tree" role="tabpanel">
					<AstTreeNode v-for="node in astTree" :key="node.id" :node="node" :depth="0" />
					<p v-if="!astTree.length" class="empty-state">No syntax tree is available.</p>
				</div>
				<div v-else-if="activeTab === 'types'" class="type-state" role="tabpanel">
					<ul v-if="typeTree.length" class="inference-roots">
						<TypeTreeNode
							v-for="node in typeTree"
							:key="node.node"
							:node="node"
							@select="selectRange"
						/>
					</ul>
					<p v-else class="empty-state">
						No inferred expression state is available for this module.
					</p>
				</div>
				<div v-else-if="activeTab === 'diagnostics'" class="diagnostic-output" role="tabpanel">
					<button
						v-for="diagnostic in result?.diagnostics ?? []"
						:key="`${diagnostic.code}-${diagnostic.start}-${diagnostic.message}`"
						type="button"
						class="diagnostic"
						:class="`is-${diagnostic.severity}`"
						@click="selectRange(diagnostic.start, diagnostic.end)"
					>
						<span class="severity">{{ diagnostic.severity }}</span>
						<span class="message">{{ diagnostic.message }}</span>
						<code>{{ diagnostic.code }}</code>
						<small>{{ diagnostic.start_line }}:{{ diagnostic.start_col }}</small>
					</button>
					<p v-if="!result?.diagnostics.length" class="clean-state">
						✓ No diagnostics. The module compiles cleanly.
					</p>
				</div>
				<div v-else-if="activeTab === 'console'" class="console-output" role="tabpanel">
					<button
						v-if="consoleLines.length"
						type="button"
						class="clear-console"
						@click="consoleLines = []"
					>
						Clear
					</button>
					<p v-for="(line, index) in consoleLines" :key="index" :class="`console-${line.level}`">
						<span aria-hidden="true">{{ line.level === "result" ? "←" : ">" }}</span
						>{{ line.text }}
					</p>
					<p v-if="!consoleLines.length" class="console-empty">
						Run a module with an exported <code>main</code> function to see its output.
					</p>
				</div>
				<pre
					v-else
					role="tabpanel"
				><code>{{ result?.js ?? "JavaScript is emitted after a clean analysis." }}</code></pre>
			</section>
		</div>

		<Teleport to="body">
			<div
				v-if="diagnosticPopup"
				class="diagnostic-popup"
				:class="`popup-${diagnosticPopup.placement}`"
				:style="{ left: `${diagnosticPopup.x}px`, top: `${diagnosticPopup.y}px` }"
				role="tooltip"
			>
				<span
					v-for="diagnostic in diagnosticPopup.diagnostics"
					:key="`${diagnostic.code}-${diagnostic.message}`"
					class="popup-diagnostic"
				>
					<strong :class="`popup-${diagnostic.severity}`">{{ diagnostic.severity }}</strong>
					<span>{{ diagnostic.message }}</span>
					<small
						><code>{{ diagnostic.code }}</code> · {{ diagnostic.start_line }}:{{
							diagnostic.start_col
						}}</small
					>
				</span>
			</div>
		</Teleport>
	</div>
</template>

<style scoped>
.compiler-lab {
	--lab-border: color-mix(in srgb, var(--vp-c-divider) 82%, var(--vp-c-brand-1));
	--syntax-background: #121212;
	--syntax-foreground: #dbd7caee;
	--syntax-keyword: #4d9375;
	--syntax-type: #5da994;
	--syntax-string: #c98a7d;
	--syntax-number: #4c9a91;
	--syntax-identifier: #bd976a;
	--syntax-operator: #cb7676;
	--syntax-delimiter: #666666;
	width: min(1120px, calc(100vw - 40px));
	margin: 32px 0 48px 50%;
	transform: translateX(-50%);
	font-size: 14px;
}

.lab-toolbar,
.pipeline,
.tabs,
.example-buttons,
.lab-actions,
.compiler-state {
	display: flex;
	align-items: center;
}

.lab-toolbar {
	justify-content: space-between;
	gap: 16px;
	margin-bottom: 16px;
}
.example-buttons {
	flex-wrap: wrap;
	gap: 8px;
}
.example-buttons > span {
	margin-right: 4px;
	color: var(--vp-c-text-2);
}
button {
	font: inherit;
}
.example-buttons button {
	padding: 5px 11px;
	border: 1px solid var(--lab-border);
	border-radius: 999px;
	background: var(--vp-c-bg-soft);
	color: var(--vp-c-text-1);
	cursor: pointer;
}
.example-buttons button:hover {
	border-color: var(--vp-c-brand-1);
	color: var(--vp-c-brand-1);
}
.compiler-state {
	gap: 8px;
	color: var(--vp-c-text-2);
	white-space: nowrap;
}
.lab-actions {
	gap: 12px;
}
.run-button,
.stop-button {
	min-width: 78px;
	padding: 7px 13px;
	border: 1px solid transparent;
	border-radius: 7px;
	color: white;
	font-weight: 600;
	cursor: pointer;
}
.run-button {
	background: var(--vp-c-green-1);
}
.run-button:hover {
	background: var(--vp-c-green-2);
}
.run-button:disabled {
	opacity: 0.55;
	cursor: wait;
}
.stop-button {
	background: var(--vp-c-danger-1);
}
.loading-dot {
	width: 8px;
	height: 8px;
	border-radius: 50%;
	background: var(--vp-c-brand-1);
	animation: pulse 1s infinite;
}

.pipeline {
	gap: 10px;
	margin-bottom: 16px;
}
.stage {
	display: flex;
	align-items: center;
	gap: 10px;
	min-width: 0;
	flex: 1;
	padding: 11px 12px;
	border: 1px solid var(--lab-border);
	border-radius: 10px;
	background: var(--vp-c-bg-soft);
}
.stage-number {
	display: grid;
	place-items: center;
	width: 25px;
	height: 25px;
	flex: 0 0 auto;
	border-radius: 50%;
	background: var(--vp-c-default-soft);
	font-weight: 700;
}
.stage strong,
.stage small {
	display: block;
}
.stage small {
	overflow: hidden;
	color: var(--vp-c-text-2);
	text-overflow: ellipsis;
	white-space: nowrap;
}
.stage.is-complete {
	border-color: color-mix(in srgb, var(--vp-c-green-1) 55%, transparent);
}
.stage.is-complete .stage-number {
	color: var(--vp-c-green-1);
	background: var(--vp-c-green-soft);
}
.stage.is-failed {
	border-color: var(--vp-c-danger-1);
}
.stage.is-failed .stage-number {
	color: var(--vp-c-danger-1);
	background: var(--vp-c-danger-soft);
}
.stage.is-blocked {
	opacity: 0.62;
}
.stage-arrow {
	color: var(--vp-c-text-3);
	font-size: 18px;
}

.lab-grid {
	display: grid;
	grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
	gap: 16px;
}
.lab-panel {
	overflow: hidden;
	border: 1px solid var(--lab-border);
	border-radius: 12px;
	background: var(--vp-c-bg);
}
.lab-panel > header {
	display: flex;
	justify-content: space-between;
	padding: 10px 14px;
	border-bottom: 1px solid var(--lab-border);
	background: var(--vp-c-bg-soft);
	font-weight: 600;
}
.lab-panel header small {
	color: var(--vp-c-text-3);
	font-weight: 400;
}
textarea,
pre,
.tokens {
	box-sizing: border-box;
	width: 100%;
	height: 430px;
	margin: 0;
	padding: 16px;
	border: 0;
	border-radius: 0;
	background: var(--vp-code-block-bg);
	color: var(--vp-code-block-color);
	font: 13px/1.65 var(--vp-font-family-mono);
	tab-size: 2;
}
textarea {
	display: block;
	resize: none;
	outline: none;
}
textarea:focus {
	box-shadow: inset 0 0 0 2px var(--vp-c-brand-1);
}
.editor-shell {
	position: relative;
	height: 430px;
	background: var(--syntax-background);
}
.editor-shell textarea,
.source-highlight {
	position: absolute;
	inset: 0;
	height: 100%;
	overflow: auto;
	white-space: pre;
}
.source-highlight {
	z-index: 2;
	overflow: hidden;
	pointer-events: none;
	background: transparent;
	color: var(--syntax-foreground);
}
.editor-shell textarea {
	z-index: 1;
	background: transparent;
	color: transparent;
	caret-color: var(--syntax-foreground);
	-webkit-text-fill-color: transparent;
}
.editor-shell textarea::selection {
	background: color-mix(in srgb, var(--vp-c-brand-1) 32%, transparent);
}
.source-highlight .syntax-keyword {
	color: var(--syntax-keyword);
	font-weight: 600;
}
.source-highlight .syntax-type {
	color: var(--syntax-type);
}
.source-highlight .syntax-string {
	color: var(--syntax-string);
}
.source-highlight .syntax-number {
	color: var(--syntax-number);
}
.source-highlight .syntax-boolean {
	color: var(--syntax-keyword);
}
.source-highlight .syntax-identifier {
	color: var(--syntax-identifier);
}
.source-highlight .syntax-delimiter {
	color: var(--syntax-delimiter);
}
.source-highlight .syntax-operator {
	color: var(--syntax-operator);
}
.source-highlight .inline-error,
.source-highlight .inline-warning {
	position: relative;
	border-radius: 2px;
	pointer-events: auto;
	cursor: help;
	text-decoration-line: underline;
	text-decoration-style: wavy;
	text-decoration-thickness: 1.5px;
	text-underline-offset: 3px;
}
.source-highlight .inline-error {
	background: color-mix(in srgb, var(--vp-c-danger-1) 14%, transparent);
	text-decoration-color: var(--vp-c-danger-1);
}
.source-highlight .inline-warning {
	background: color-mix(in srgb, var(--vp-c-warning-1) 14%, transparent);
	text-decoration-color: var(--vp-c-warning-1);
}
.diagnostic-popup {
	position: fixed;
	z-index: 1000;
	display: grid;
	gap: 9px;
	width: max-content;
	max-width: min(360px, calc(100vw - 64px));
	padding: 10px 12px;
	border: 1px solid #2f2f2f;
	border-radius: 8px;
	background: #181818;
	box-shadow: 0 10px 30px #00000066;
	color: #dbd7caee;
	font: 12px/1.45 var(--vp-font-family-mono);
	pointer-events: none;
	white-space: normal;
}
.diagnostic-popup::after {
	position: absolute;
	left: 12px;
	width: 8px;
	height: 8px;
	border-right: 1px solid #2f2f2f;
	border-bottom: 1px solid #2f2f2f;
	background: #181818;
	content: "";
}
.diagnostic-popup.popup-above {
	transform: translateY(-100%);
}
.diagnostic-popup.popup-above::after {
	top: 100%;
	transform: translateY(-4px) rotate(45deg);
}
.diagnostic-popup.popup-below::after {
	bottom: 100%;
	transform: translateY(4px) rotate(225deg);
}
.popup-diagnostic {
	display: grid;
	grid-template-columns: auto 1fr;
	gap: 2px 8px;
	color: #dbd7caee;
}
.popup-diagnostic strong {
	font-size: 10px;
	letter-spacing: 0.06em;
	text-transform: uppercase;
}
.popup-diagnostic small {
	grid-column: 2;
	color: #dedcd590;
}
.popup-diagnostic code {
	color: #dedcd590;
}
.popup-error {
	color: #cb7676;
}
.popup-warning {
	color: #d4976c;
}
.popup-info {
	color: #6394bf;
}
.popup-hint {
	color: #4d9375;
}
pre {
	overflow: auto;
	white-space: pre;
}
pre code {
	padding: 0;
	background: transparent;
	color: inherit;
}
.tabs {
	height: 45px;
	padding: 0 8px;
	border-bottom: 1px solid var(--lab-border);
	background: var(--vp-c-bg-soft);
}
.tabs button {
	display: flex;
	align-items: center;
	gap: 5px;
	align-self: stretch;
	padding: 0 7px;
	border: 0;
	border-bottom: 2px solid transparent;
	background: transparent;
	color: var(--vp-c-text-2);
	cursor: pointer;
}
.tabs button[aria-selected="true"] {
	border-color: var(--vp-c-brand-1);
	color: var(--vp-c-brand-1);
}
.tabs .tab-count {
	display: grid;
	place-items: center;
	min-width: 18px;
	height: 18px;
	border-radius: 9px;
	background: var(--vp-c-default-soft);
	font-size: 11px;
}
.tabs .loading-dot {
	display: inline-block;
	width: 6px;
	height: 6px;
}
.tokens {
	display: flex;
	align-content: flex-start;
	flex-wrap: wrap;
	gap: 8px;
	overflow: auto;
}
.token {
	display: flex;
	align-items: baseline;
	gap: 7px;
	height: fit-content;
	padding: 5px 8px;
	border: 1px solid var(--lab-border);
	border-radius: 6px;
	background: var(--vp-c-bg);
	color: var(--vp-c-text-1);
	cursor: pointer;
}
.token:hover {
	border-color: var(--vp-c-brand-1);
}
.token code {
	max-width: 180px;
	overflow: hidden;
	padding: 0;
	background: transparent;
	text-overflow: ellipsis;
	white-space: pre;
}
.token small {
	color: var(--vp-c-text-3);
}
.ast-tree {
	box-sizing: border-box;
	height: 430px;
	overflow: auto;
	padding: 12px;
	background: #121212;
	color: #dbd7caee;
	font: 12px/1.45 var(--vp-font-family-mono);
}
.ast-tree :deep(.ast-branch),
.ast-tree :deep(.ast-leaf) {
	position: relative;
	margin: 0;
}
.ast-tree :deep(summary),
.ast-tree :deep(.ast-leaf) {
	display: flex;
	align-items: center;
	gap: 7px;
	min-height: 27px;
	padding: 2px 7px;
	border-radius: 5px;
}
.ast-tree :deep(summary) {
	cursor: pointer;
	list-style: none;
}
.ast-tree :deep(summary::-webkit-details-marker) {
	display: none;
}
.ast-tree :deep(summary:hover),
.ast-tree :deep(summary:focus-visible),
.ast-tree :deep(.ast-leaf:hover) {
	background: #1b1b1b;
	outline: none;
}
.ast-tree :deep(code) {
	overflow: hidden;
	padding: 0;
	background: transparent;
	color: #bd976a;
	text-overflow: ellipsis;
	white-space: nowrap;
}
.ast-tree :deep(summary small) {
	margin-left: auto;
	color: #dedcd570;
}
.ast-tree :deep(.ast-chevron) {
	color: #5da994;
	font-size: 18px;
	line-height: 1;
	transition: transform 120ms ease;
}
.ast-tree :deep(details[open] > summary .ast-chevron) {
	transform: rotate(90deg);
}
.ast-tree :deep(.ast-children) {
	position: relative;
	margin-left: 12px;
	padding-left: 13px;
}
.ast-tree :deep(.ast-children::before) {
	position: absolute;
	inset: 0 auto 7px 0;
	border-left: 1px solid #353535;
	content: "";
}
.ast-tree :deep(.ast-dot) {
	width: 5px;
	height: 5px;
	margin: 0 6px;
	flex: 0 0 auto;
	border-radius: 50%;
	background: #666666;
}
.type-state {
	box-sizing: border-box;
	height: 430px;
	overflow: auto;
	padding: 14px;
	background: #121212;
	color: #dbd7caee;
	font: 12px/1.45 var(--vp-font-family-mono);
}
.type-state :deep(ul) {
	margin: 0;
	padding: 0;
	list-style: none;
}
.type-state :deep(.inference-roots) {
	display: grid;
	gap: 18px;
}
.type-state :deep(.inference-node) {
	position: relative;
}
.type-state :deep(.inference-children) {
	position: relative;
	display: grid;
	gap: 10px;
	margin: 10px 0 0 18px;
	padding-left: 22px;
}
.type-state :deep(.inference-children::before) {
	position: absolute;
	inset: -10px auto 17px 0;
	border-left: 1px solid #3b665b;
	content: "";
}
.type-state :deep(.inference-children > .inference-node::before) {
	position: absolute;
	top: 17px;
	right: 100%;
	width: 22px;
	border-top: 1px solid #3b665b;
	content: "";
}
.type-state :deep(.type-card) {
	display: grid;
	gap: 3px;
	width: 100%;
	padding: 8px 10px;
	border: 1px solid #303030;
	border-radius: 7px;
	background: #181818;
	color: inherit;
	text-align: left;
	cursor: pointer;
}
.type-state :deep(.type-card:hover),
.type-state :deep(.type-card:focus-visible) {
	border-color: #5da994;
	background: #1d1d1d;
	outline: none;
}
.type-state :deep(.type-card-heading) {
	display: flex;
	align-items: baseline;
	justify-content: space-between;
	gap: 14px;
}
.type-state :deep(.type-card code) {
	overflow: hidden;
	padding: 0;
	background: transparent;
	color: #bd976a;
	text-overflow: ellipsis;
	white-space: nowrap;
}
.type-state :deep(.type-card small),
.type-state :deep(.type-dispatch) {
	color: #dedcd590;
}
.type-state :deep(.type-card strong) {
	flex: 0 0 auto;
	color: #5da994;
	font-weight: 500;
}

.clear-console {
	float: right;
	padding: 3px 8px;
	border: 0;
	background: transparent;
	color: #dedcd590;
	cursor: pointer;
}
.diagnostic-output,
.console-output {
	box-sizing: border-box;
	height: 430px;
	overflow: auto;
}
.console-output {
	padding: 10px 14px;
	background: #121212;
	color: #dbd7caee;
	font: 12px/1.6 var(--vp-font-family-mono);
}
.console-output p {
	display: flex;
	gap: 9px;
	margin: 0;
	white-space: pre-wrap;
}
.console-output p > span {
	color: #666666;
}
.console-result {
	color: #5da994;
}
.console-error {
	color: #cb7676;
}
.console-warn {
	color: #d4976c;
}
.console-system,
.console-empty {
	color: #dedcd590;
}
.console-empty code {
	padding: 0;
	background: transparent;
	color: #bd976a;
}
.diagnostic {
	display: grid;
	grid-template-columns: 66px 1fr auto 52px;
	gap: 12px;
	width: 100%;
	padding: 10px 14px;
	border: 0;
	border-bottom: 1px solid var(--lab-border);
	background: transparent;
	color: var(--vp-c-text-1);
	text-align: left;
	cursor: pointer;
}
.diagnostic:hover {
	background: var(--vp-c-bg-soft);
}
.severity {
	font-size: 11px;
	font-weight: 700;
	letter-spacing: 0.05em;
	text-transform: uppercase;
}
.is-error .severity {
	color: var(--vp-c-danger-1);
}
.is-warning .severity {
	color: var(--vp-c-warning-1);
}
.diagnostic code,
.diagnostic small {
	color: var(--vp-c-text-3);
}
.clean-state,
.empty-state {
	margin: 0;
	padding: 14px;
	color: var(--vp-c-green-1);
}
.lab-failure {
	margin-bottom: 16px;
	padding: 12px 14px;
	border: 1px solid var(--vp-c-danger-1);
	border-radius: 8px;
	background: var(--vp-c-danger-soft);
	color: var(--vp-c-danger-1);
}

@keyframes pulse {
	50% {
		opacity: 0.35;
	}
}

@media (max-width: 820px) {
	.compiler-lab {
		width: calc(100vw - 28px);
	}
	.lab-toolbar {
		align-items: flex-start;
		flex-direction: column;
	}
	.pipeline {
		display: grid;
		grid-template-columns: 1fr 1fr;
	}
	.stage-arrow {
		display: none;
	}
	.lab-grid {
		grid-template-columns: 1fr;
	}
	textarea,
	pre,
	.tokens {
		height: 340px;
	}
	.ast-tree {
		height: 340px;
	}
	.type-state {
		height: 340px;
	}
	.diagnostic-output,
	.console-output {
		height: 340px;
	}
	.editor-shell {
		height: 340px;
	}
	.diagnostic {
		grid-template-columns: 60px 1fr;
	}
	.diagnostic code,
	.diagnostic small {
		display: none;
	}
}
</style>
