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
		languages: [
			{
				...(grammar as any),
				name: "nymph",
				aliases: ["nym"],
			},
		],
	},
	sitemap: {
		hostname: "https://nymphlang.dev",
	},
	themeConfig: {
		// https://vitepress.dev/reference/default-theme-config
		nav: [
			{ text: "Home", link: "/" },
			{ text: "Guide", link: "/guide/" },
			{ text: "Reference", link: "/reference/" },
		],
		search: { provider: "local" },

		sidebar: [
			{
				text: "Guide",
				base: "/guide",
				items: [{ text: "Getting Started", link: "/" }],
			},
			{
				text: "Reference",
				base: "/reference",
				items: [
					{ text: "Introduction", link: "/" },
					{ text: "Literals", link: "/literals/" },
					{ text: "Expressions", link: "/expressions/" },
					{ text: "Declarations", link: "/declarations/" },
					{ text: "Types", link: "/types/" },
					{ text: "Standard Library", link: "/stdlib/" },
				],
			}
		],

		socialLinks: [{ icon: "github", link: "https://github.com/theonlytails/nymph_lang" }],
	},
});
