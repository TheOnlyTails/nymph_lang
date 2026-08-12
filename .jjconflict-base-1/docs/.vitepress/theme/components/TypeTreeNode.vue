<script setup lang="ts">
export type TypeTreeNode = {
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
	children: TypeTreeNode[];
};

defineProps<{ node: TypeTreeNode }>();
const emit = defineEmits<{ select: [start: number, end: number] }>();

function forwardSelect(start: number, end: number) {
	emit("select", start, end);
}
</script>

<template>
	<li class="inference-node">
		<button type="button" class="type-card" @click="emit('select', node.start, node.end)">
			<span class="type-card-heading">
				<code>{{ node.source }}</code>
				<strong>{{ node.type_ }}</strong>
			</span>
			<small>node {{ node.node }} · {{ node.line }}:{{ node.col }}</small>
			<small v-if="node.dispatch" class="type-dispatch">
				{{ node.dispatch }}<template v-if="node.method"> · {{ node.method }}</template>
			</small>
		</button>
		<ul v-if="node.children.length" class="inference-children">
			<TypeTreeNode
				v-for="child in node.children"
				:key="child.node"
				:node="child"
				@select="forwardSelect"
			/>
		</ul>
	</li>
</template>
