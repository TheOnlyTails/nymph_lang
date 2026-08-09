use std::collections::BTreeMap;

use nymph_compiler::{DocFragment, DocOptions, DocSignature, Severity, document_project_with_std};

fn project(
	files: BTreeMap<&'static str, &'static str>,
	private: bool,
) -> nymph_compiler::DocProject {
	document_project_with_std(
		"main",
		&|path| files.get(path).map(ToString::to_string),
		&|_| None,
		DocOptions {
			document_private_items: private,
		},
	)
	.unwrap_or_else(|diagnostics| panic!("unexpected diagnostics: {diagnostics:#?}"))
}

fn signature_text(signature: &DocSignature) -> String {
	signature
		.0
		.iter()
		.filter_map(|fragment| match fragment {
			DocFragment::Text(text) => Some(text.as_str()),
			DocFragment::Definition { .. } => None,
		})
		.collect()
}

#[test]
fn doc_public_private_signatures_paths_and_exact_cross_module_links() {
	let files = BTreeMap::from([
		(
			"main",
			"import @/types/model with (Token as Renamed)\npublic func echo<T: Default>(value: #(Renamed, T)): #(Renamed, T) = value\nprivate func hidden(value: int): int = value",
		),
		(
			"types/model",
			"public struct Token(private secret: int, public value: int) { private func concealed(): int = this.secret }\npublic type Alias = Token\npublic interface Parent<T> {}\npublic interface Child: Parent<int> {}\npublic impl Token { public func visible(): int = this.value private func concealed_impl(): int = this.secret }",
		),
	]);
	let public = project(files.clone(), false);
	assert_eq!(
		public
			.modules
			.keys()
			.map(|p| p.as_str())
			.collect::<Vec<_>>(),
		vec!["main", "types/model"]
	);
	assert_eq!(
		public.modules[&nymph_compiler::ModulePath::new("main").unwrap()].url,
		"modules/main.html"
	);
	let items = &public.modules[&nymph_compiler::ModulePath::new("main").unwrap()].items;
	assert_eq!(
		items
			.iter()
			.map(|item| item.name.as_str())
			.collect::<Vec<_>>(),
		vec!["echo"]
	);
	let links = items[0]
		.signature
		.0
		.iter()
		.filter_map(|fragment| match fragment {
			DocFragment::Definition { target, .. } => Some(target),
			DocFragment::Text(_) => None,
		})
		.collect::<Vec<_>>();
	assert_eq!(
		links
			.iter()
			.filter(|target| target.module.path == "types/model")
			.count(),
		2
	);
	let signature = signature_text(&items[0].signature);
	assert!(signature.starts_with("public func echo<T>"), "{signature}");
	assert!(signature.contains("echo<T>"));
	assert!(signature.matches('T').count() >= 3, "{signature}");
	let public_token = &public.modules[&nymph_compiler::ModulePath::new("types/model").unwrap()]
		.items
		.iter()
		.find(|item| item.name == "Token")
		.unwrap()
		.signature;
	let public_token = signature_text(public_token);
	assert!(!public_token.contains("secret"), "{public_token}");
	assert!(!public_token.contains("concealed"), "{public_token}");
	assert!(public_token.contains("public value"), "{public_token}");
	let public_implementations =
		&public.modules[&nymph_compiler::ModulePath::new("types/model").unwrap()].implementations;
	assert_eq!(public_implementations.len(), 1);
	assert!(format!("{public_implementations:?}").contains("visible"));
	assert!(!format!("{public_implementations:?}").contains("concealed_impl"));
	let public_model =
		&public.modules[&nymph_compiler::ModulePath::new("types/model").unwrap()].items;
	assert!(
		public_model
			.iter()
			.any(|item| { item.name == "Alias" && format!("{:?}", item.signature.0).contains(" = ") })
	);
	assert!(
		public_model
			.iter()
			.any(|item| { item.name == "Child" && format!("{:?}", item.signature.0).contains("Parent") })
	);
	let private = project(files, true);
	let items = &private.modules[&nymph_compiler::ModulePath::new("main").unwrap()].items;
	let hidden = items
		.iter()
		.find(|item| item.name == "hidden" && item.private)
		.expect("private declaration");
	assert!(
		signature_text(&hidden.signature).starts_with("private func hidden"),
		"{:?}",
		hidden.signature
	);
	let private_token = &private.modules[&nymph_compiler::ModulePath::new("types/model").unwrap()]
		.items
		.iter()
		.find(|item| item.name == "Token")
		.unwrap()
		.signature;
	let private_token = signature_text(private_token);
	assert!(private_token.contains("private secret"), "{private_token}");
	assert!(
		private_token.contains("private func concealed"),
		"{private_token}"
	);
	let private_implementations =
		&private.modules[&nymph_compiler::ModulePath::new("types/model").unwrap()].implementations;
	assert!(format!("{private_implementations:?}").contains("concealed_impl"));
}

#[test]
fn doc_html_is_deterministic_escaped_and_diagnostics_block_output() {
	let files = BTreeMap::from([("main", "public func value(input: int): int = input")]);
	let first = project(files.clone(), false).render_html();
	let second = project(files, false).render_html();
	assert_eq!(first, second);
	assert!(first.contains_key("assets/style.css"));

	let escaped = document_project_with_std(
		"angle<root>",
		&|_| Some("public func value(): int = 1".into()),
		&|_| None,
		DocOptions::default(),
	)
	.unwrap()
	.render_html();
	assert!(escaped["index.html"].contains("angle&lt;root&gt;"));
	assert!(!escaped["index.html"].contains("angle<root>"));
	assert!(escaped.contains_key("modules/angle~3Croot~3E.html"));
	assert!(escaped["index.html"].contains("href=\"modules/angle~3Croot~3E.html\""));
	assert!(!escaped["index.html"].contains('%'));

	let broken = document_project_with_std(
		"main",
		&|_| Some("public func broken(: int".into()),
		&|_| None,
		DocOptions::default(),
	);
	assert!(broken.is_err());
}

#[test]
fn warnings_do_not_block_documentation() {
	let docs = project(
		BTreeMap::from([("main", "public func large(): int = 9007199254740992")]),
		false,
	);
	assert_eq!(docs.modules.values().next().unwrap().items[0].name, "large");
	assert_eq!(docs.diagnostics.len(), 1, "{:#?}", docs.diagnostics);
	assert_eq!(docs.diagnostics[0].diag.severity, Severity::Warning);
}

#[test]
fn declaration_external_and_mutable_implementation_modifiers_are_preserved() {
	let docs = project(
		BTreeMap::from([(
			"main",
			"public let mut counter: int = 0\npublic external(max_float) let external_count: float\npublic struct Cell(value: int)\npublic impl mut Cell { public external(display) func read(): string }",
		)]),
		false,
	);
	let module = docs.modules.values().next().unwrap();
	let counter = module
		.items
		.iter()
		.find(|item| item.name == "counter")
		.unwrap();
	assert_eq!(
		signature_text(&counter.signature),
		"public let mut counter: int"
	);
	let external_count = module
		.items
		.iter()
		.find(|item| item.name == "external_count")
		.unwrap();
	assert_eq!(
		signature_text(&external_count.signature),
		"public external(max_float) let external_count: float"
	);
	let implementation = module.implementations.first().unwrap();
	let signature = signature_text(&implementation.signature);
	assert!(signature.starts_with("public impl mut "), "{signature}");
	assert!(
		signature.contains("public external(display) func read(): string"),
		"{signature}"
	);
}

#[test]
fn namespace_members_use_checked_generics_constraints_and_inferred_types() {
	let docs = project(
		BTreeMap::from([(
			"main",
			"public interface Marker {}\npublic namespace Tools { public func id<T: Marker>(value: T) = value public let answer = 42 }",
		)]),
		false,
	);
	let module = docs.modules.values().next().unwrap();
	let tools = module
		.items
		.iter()
		.find(|item| item.name == "Tools")
		.unwrap();
	let signature = signature_text(&tools.signature);
	assert!(
		signature.contains("public func id<T>(value: T): T where T: "),
		"{signature}"
	);
	assert!(signature.contains("public let answer: int"), "{signature}");
}

#[test]
fn implementations_nested_in_private_types_follow_effective_privacy() {
	let files = BTreeMap::from([(
		"main",
		"public interface Read { func read(): int }\nprivate interface Conceal { func conceal(): int }\nprivate struct Secret { impl Read { public func read(): int = 1 } }\nprivate enum Hidden { Value impl Read { public func read(): int = 2 } }\npublic struct Visible { impl Read { public func read(): int = 3 } }\npublic impl Read for int { public func read(): int = this }\npublic impl Read for #[Secret] { public func read(): int = 4 }\npublic impl Conceal for uint { public func conceal(): int = 5 }",
	)]);
	let public = project(files.clone(), false);
	let implementations = &public.modules.values().next().unwrap().implementations;
	assert_eq!(implementations.len(), 2, "{implementations:#?}");
	assert!(
		implementations
			.iter()
			.all(|implementation| !implementation.private)
	);

	let private = project(files, true);
	let implementations = &private.modules.values().next().unwrap().implementations;
	assert_eq!(implementations.len(), 6, "{implementations:#?}");
	assert_eq!(
		implementations
			.iter()
			.filter(|implementation| implementation.private)
			.count(),
		4
	);
}

#[test]
fn unchecked_interface_owned_implementations_do_not_corrupt_documentation_interfaces() {
	let docs = project(
		BTreeMap::from([(
			"main",
			"public interface Parent { func map<T>(value: T): T }\npublic interface Child { impl Parent { func map<T>(value: T): T = value } }",
		)]),
		false,
	);
	let module = docs.modules.values().next().unwrap();
	assert_eq!(
		module
			.items
			.iter()
			.map(|item| item.name.as_str())
			.collect::<Vec<_>>(),
		vec!["Child", "Parent"]
	);
	assert!(module.implementations.is_empty());
}

#[test]
fn implementation_anchors_are_stable_when_an_unrelated_implementation_is_added() {
	fn anchor_for(docs: &nymph_compiler::DocProject, name: &str) -> String {
		let module = docs.modules.values().next().unwrap();
		let target = &module
			.items
			.iter()
			.find(|item| item.name == name)
			.unwrap()
			.definition;
		module
			.implementations
			.iter()
			.find(|implementation| {
				implementation.signature.0.iter().any(|fragment| {
					matches!(
						fragment,
						DocFragment::Definition { target: candidate, .. } if candidate == target
					)
				})
			})
			.unwrap()
			.anchor
			.clone()
	}

	let baseline = project(
		BTreeMap::from([(
			"main",
			"public interface Read { func read(): int }\npublic struct B { impl Read { func read(): int = 2 } }",
		)]),
		false,
	);
	let expanded = project(
		BTreeMap::from([(
			"main",
			"public interface Read { func read(): int }\npublic struct A { impl Read { func read(): int = 1 } }\npublic struct B { impl Read { func read(): int = 2 } }",
		)]),
		false,
	);
	assert_eq!(anchor_for(&baseline, "B"), anchor_for(&expanded, "B"));
}

#[test]
fn module_urls_are_case_fold_unique_and_avoid_windows_device_names() {
	let docs = project(
		BTreeMap::from([
			(
				"main",
				"import @/Foo as Upper\nimport @/foo as Lower\nimport @/con as Reserved\npublic let answer: int = 42",
			),
			("Foo", ""),
			("foo", ""),
			("con", ""),
		]),
		false,
	);
	let urls = docs
		.modules
		.values()
		.map(|module| module.url.to_ascii_lowercase())
		.collect::<std::collections::BTreeSet<_>>();
	assert_eq!(urls.len(), docs.modules.len());
	assert_ne!(
		docs.modules[&nymph_compiler::ModulePath::new("con").unwrap()].url,
		"modules/con.html"
	);
}
