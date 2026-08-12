<script setup lang="ts">
export type AstTreeNode = {
	id: number;
	label: string;
	children: AstTreeNode[];
};

defineProps<{
	node: AstTreeNode;
	depth: number;
}>();
</script>

<template>
	<details v-if="node.children.length" class="ast-branch" :open="depth < 2">
		<summary>
			<span class="ast-chevron" aria-hidden="true">›</span>
			<code>{{ node.label }}</code>
			<small>{{ node.children.length }}</small>
		</summary>
		<div class="ast-children">
			<AstTreeNode
				v-for="child in node.children"
				:key="child.id"
				:node="child"
				:depth="depth + 1"
			/>
		</div>
	</details>
	<div v-else class="ast-leaf">
		<span class="ast-dot" aria-hidden="true"></span>
		<code>{{ node.label }}</code>
	</div>
</template>
