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
				// `nym` is the fence tag the doc-sample test harness
				// (crates/nymph-compiler/tests/docs_samples.rs) checks: every
				// ```nym fence must compile cleanly, unless one of its lines
				// carries a trailing `// [!code error]` comment, in which case
				// the harness asserts compilation fails with a diagnostic on
				// that line (VitePress renders the same comment as an inline
				// error highlight). Fragments not meant to be checked use the
				// grammar's own name, `nymph`, instead of `nym`, so the harness
				// skips them while they still get syntax highlighting.
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
			{ text: "Tour", link: "/tour/" },
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
				text: "Tour",
				base: "/tour",
				items: [{ text: "A tour of Nymph", link: "/" }],
			},
			{
				text: "Reference",
				base: "/reference",
				items: [
					{ text: "Introduction", link: "/" },
					{ text: "Formatting", link: "/formatting/" },
					{ text: "Literals", link: "/literals/" },
					{ text: "Expressions", link: "/expressions/" },
					{ text: "Declarations", link: "/declarations/" },
					{ text: "Generated Documentation", link: "/generated-documentation/" },
					{ text: "Types", link: "/types/" },
					{ text: "Functions", link: "/functions/" },
					{ text: "Structs and Enums", link: "/structs-and-enums/" },
					{ text: "Interfaces and Impls", link: "/interfaces-and-impls/" },
					{ text: "Pattern Matching", link: "/pattern-matching/" },
					{ text: "Operators", link: "/operators/" },
					{ text: "Error Handling", link: "/error-handling/" },
					{ text: "Mutability", link: "/mutability/" },
					{ text: "Iteration", link: "/iteration/" },
					{ text: "Standard Library", link: "/stdlib/" },
				],
			},
		],

		socialLinks: [{ icon: "github", link: "https://github.com/theonlytails/nymph_lang" }],
	},
});
