import { defineConfig } from "vitepress";
import grammar from "../../extension/syntaxes/nymph.tmLanguage.json" with { type: "json" };

// https://vitepress.dev/reference/site-config
export default defineConfig({
	title: "Nymph",
	description: "A simple language that gets out of your way.",
	cleanUrls: true,
	lastUpdated: true,
	markdown: {
		math: true,
		lineNumbers: true,
		languageAlias: { nym: "Nymph" },
		shikiSetup: async (shiki) => {
			await shiki.loadLanguage(grammar as any);
		},
	},
	sitemap: {
		hostname: "nymphlang.dev",
	},
	themeConfig: {
		// https://vitepress.dev/reference/default-theme-config
		nav: [
			{ text: "Home", link: "/" },
			{ text: "Guide", link: "/guide" },
		],
		search: { provider: "local" },

		sidebar: [
			{
				text: "Guide",
				items: [{ text: "Getting Started", link: "/guide" }],
			},
		],

		socialLinks: [{ icon: "github", link: "https://github.com/theonlytails/nymph_lang" }],
	},
});
