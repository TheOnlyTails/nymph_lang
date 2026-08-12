import DefaultTheme from "vitepress/theme";
import type { Theme } from "vitepress";
import NymphDebugger from "./components/NymphDebugger.vue";
import "./theme.css";

export default {
	extends: DefaultTheme,
	enhanceApp({ app }) {
		app.component("NymphDebugger", NymphDebugger);
	},
} satisfies Theme;
