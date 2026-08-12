<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";

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
type Inspection = {
	tokens: Token[];
	ast: string;
	stages: Stage[];
	js: string | null;
	diagnostics: Diagnostic[];
};

const examples = {
	Functions: `func fibonacci(n: int): int = match (n) {
	..=1 -> n,
	_ -> fibonacci(n - 1) + fibonacci(n - 2),
}

func answer(): int = fibonacci(10)
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
	"Type error": `func greet(name: string): string = "Hello, " + name

func broken(): int = greet("Nymph")
`,
};

const source = ref(examples.Functions);
const result = ref<Inspection | null>(null);
const loading = ref(true);
const failure = ref("");
const activeTab = ref<"tokens" | "ast" | "javascript">("tokens");
const editor = ref<HTMLTextAreaElement | null>(null);
let inspectSource: ((value: string) => Inspection) | undefined;
let timer: ReturnType<typeof setTimeout> | undefined;

const errors = computed(
	() => result.value?.diagnostics.filter((item) => item.severity === "error").length ?? 0,
);
const warnings = computed(
	() => result.value?.diagnostics.filter((item) => item.severity === "warning").length ?? 0,
);

function runInspection() {
	if (!inspectSource) return;
	try {
		result.value = inspectSource(source.value);
		failure.value = "";
	} catch (error) {
		failure.value = error instanceof Error ? error.message : String(error);
	}
}

function scheduleInspection() {
	clearTimeout(timer);
	timer = setTimeout(runInspection, 180);
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

function chooseExample(name: keyof typeof examples) {
	source.value = examples[name];
}

watch(source, scheduleInspection);

onMounted(async () => {
	try {
		const wasm = await import("../../wasm/nymph_wasm.js");
		await wasm.default();
		inspectSource = wasm.inspect as (value: string) => Inspection;
		runInspection();
	} catch (error) {
		failure.value = `Could not load the compiler: ${error instanceof Error ? error.message : String(error)}`;
	} finally {
		loading.value = false;
	}
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
			<div class="compiler-state" aria-live="polite">
				<span v-if="loading" class="loading-dot"></span>
				{{ loading ? "Loading compiler…" : `${errors} errors · ${warnings} warnings` }}
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
				<textarea
					ref="editor"
					v-model="source"
					aria-label="Nymph source code"
					spellcheck="false"
				></textarea>
			</section>

			<section class="lab-panel output-panel">
				<div class="tabs" role="tablist" aria-label="Compiler output">
					<button
						v-for="tab in ['tokens', 'ast', 'javascript'] as const"
						:key="tab"
						type="button"
						role="tab"
						:aria-selected="activeTab === tab"
						@click="activeTab = tab"
					>
						{{ tab === "ast" ? "AST" : tab[0].toUpperCase() + tab.slice(1) }}
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
				<pre v-else-if="activeTab === 'ast'" role="tabpanel"><code>{{ result?.ast }}</code></pre>
				<pre
					v-else
					role="tabpanel"
				><code>{{ result?.js ?? "JavaScript is emitted after a clean analysis." }}</code></pre>
			</section>
		</div>

		<section class="diagnostics" aria-live="polite">
			<header>
				<strong>Diagnostics</strong>
				<span>{{ result?.diagnostics.length ?? 0 }}</span>
			</header>
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
		</section>
	</div>
</template>

<style scoped>
.compiler-lab {
	--lab-border: color-mix(in srgb, var(--vp-c-divider) 82%, var(--vp-c-brand-1));
	width: min(1120px, calc(100vw - 40px));
	margin: 32px 0 48px 50%;
	transform: translateX(-50%);
	font-size: 14px;
}

.lab-toolbar,
.pipeline,
.tabs,
.diagnostics header,
.example-buttons,
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
.lab-panel,
.diagnostics {
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
	resize: vertical;
	outline: none;
}
textarea:focus {
	box-shadow: inset 0 0 0 2px var(--vp-c-brand-1);
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
	align-self: stretch;
	padding: 0 10px;
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

.diagnostics {
	margin-top: 16px;
}
.diagnostics header {
	gap: 8px;
	padding: 10px 14px;
	border-bottom: 1px solid var(--lab-border);
	background: var(--vp-c-bg-soft);
}
.diagnostics header span {
	display: grid;
	place-items: center;
	min-width: 21px;
	height: 21px;
	border-radius: 10px;
	background: var(--vp-c-default-soft);
	font-size: 12px;
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
	.diagnostic {
		grid-template-columns: 60px 1fr;
	}
	.diagnostic code,
	.diagnostic small {
		display: none;
	}
}
</style>
