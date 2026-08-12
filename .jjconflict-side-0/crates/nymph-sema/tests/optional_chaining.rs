use nymph_sema::check_module;
use nymph_syntax::parse_module;

fn errors(source: &str) -> Vec<String> {
	let parsed = parse_module(source, "optional_chaining");
	assert!(
		parsed.diagnostics.is_empty(),
		"parse diagnostics: {:?}",
		parsed.diagnostics
	);
	check_module(&parsed.tree)
		.diags
		.iter()
		.filter(|diagnostic| diagnostic.is_error())
		.map(|diagnostic| diagnostic.message.to_string())
		.collect()
}

const PRELUDE: &str = r#"
enum Option<T> { Some(value: T), None }
enum Result<T, E> { Ok(value: T), Error(error: E) }
enum Maybe<T> { Present(value: T), Absent }
struct Box<T>(value: T)
struct Item(name: string, child: Box<string>, maybe: Option<Box<string>>) {
  func label(suffix: string): string = this.name + suffix
}
"#;

#[test]
fn optional_chaining_maps_option_and_result_payloads_without_flattening() {
	let source = format!(
		"{PRELUDE}\n\
		 func field(value: Option<Item>): Option<string> = value?.name\n\
		 func method(value: Option<Item>): Option<string> = value?.label(\"!\")\n\
		 func index(value: Option<#[int]>, i: uint): Option<int> = value?.[i]\n\
		 func result(value: Result<Item, string>): Result<string, string> = value?.name\n\
		 func nested(value: Option<Item>): Option<Option<Box<string>>> = value?.maybe\n\
		 func chain(value: Option<Item>): Option<string> = value?.child?.value\n\
		 func generic<T>(value: Option<Box<T>>): Option<T> = value?.value"
	);
	assert!(errors(&source).is_empty(), "errors: {:?}", errors(&source));
}

#[test]
fn optional_chaining_rejects_noncanonical_receivers() {
	let source = format!("{PRELUDE}\nfunc bad(value: Maybe<Box<int>>) = value?.value");
	let diagnostics = errors(&source);
	assert!(
		diagnostics
			.iter()
			.any(|message| message.contains("requires canonical `Option` or `Result`")),
		"diagnostics: {diagnostics:?}"
	);
}
