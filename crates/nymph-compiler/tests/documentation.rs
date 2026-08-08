use std::collections::BTreeMap;

use nymph_compiler::{DocFragment, DocOptions, document_project_with_std};

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
	let signature = items[0]
		.signature
		.0
		.iter()
		.filter_map(|fragment| match fragment {
			DocFragment::Text(text) => Some(text.as_str()),
			DocFragment::Definition { .. } => None,
		})
		.collect::<String>();
	assert!(signature.contains("echo<T>"));
	assert!(signature.matches('T').count() >= 3, "{signature}");
	let public_token = &public.modules[&nymph_compiler::ModulePath::new("types/model").unwrap()]
		.items
		.iter()
		.find(|item| item.name == "Token")
		.unwrap()
		.signature
		.0;
	assert!(!format!("{public_token:?}").contains("secret"));
	assert!(!format!("{public_token:?}").contains("concealed"));
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
	assert!(
		items
			.iter()
			.any(|item| item.name == "hidden" && item.private)
	);
	let private_token = &private.modules[&nymph_compiler::ModulePath::new("types/model").unwrap()]
		.items
		.iter()
		.find(|item| item.name == "Token")
		.unwrap()
		.signature
		.0;
	assert!(format!("{private_token:?}").contains("secret"));
	assert!(format!("{private_token:?}").contains("concealed"));
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
}

#[test]
fn implementations_nested_in_private_types_follow_effective_privacy() {
	let files = BTreeMap::from([(
		"main",
		"public interface Read { func read(): int }\nprivate struct Secret { impl Read { public func read(): int = 1 } }\nprivate enum Hidden { Value impl Read { public func read(): int = 2 } }\npublic struct Visible { impl Read { public func read(): int = 3 } }\npublic impl Read for int { public func read(): int = this }",
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
	assert_eq!(implementations.len(), 4, "{implementations:#?}");
	assert_eq!(
		implementations
			.iter()
			.filter(|implementation| implementation.private)
			.count(),
		2
	);
}

#[test]
fn interface_nested_implementations_keep_checked_semantic_links_and_render() {
	let files = BTreeMap::from([(
		"main",
		"public interface Parent<T> { func parent(value: T): T }\npublic interface Child<T>: Parent<T> { impl Parent<T> { public func parent(value: T): T = value } }",
	)]);
	let docs = project(files, false);
	let module = docs.modules.values().next().unwrap();
	assert_eq!(
		module.implementations.len(),
		1,
		"{:#?}",
		module.implementations
	);
	let implementation = &module.implementations[0];
	let parent = &module
		.items
		.iter()
		.find(|item| item.name == "Parent")
		.unwrap()
		.definition;
	assert!(implementation.signature.0.iter().any(|fragment| matches!(
		fragment,
		DocFragment::Definition { target, .. } if target == parent
	)));
	let html = docs.render_html();
	assert!(html[&module.url].contains("parent"));
	assert!(html[&module.url].contains("implementation-0"));
}
