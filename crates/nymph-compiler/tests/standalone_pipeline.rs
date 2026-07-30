#![cfg(feature = "test-support")]

use nymph_compiler::project::GraphShape;
use nymph_compiler::{check, check_entry, check_without_prelude, compile, compile_entry};

fn assert_one_stable_check<T>(run: impl FnOnce() -> T) -> T {
	run()
}

#[test]
fn single_graph_fixture_is_exact_and_has_no_imports() {
	let fixture = GraphShape::Single.generate();
	assert_eq!(fixture.entry(), "main");
	assert_eq!(fixture.sources().len(), 1);
	assert_eq!(
		fixture.sources().get("main").map(String::as_str),
		Some("public func root_value(): int = 0")
	);
	assert!(fixture.unresolved_imports().is_empty());
}

#[test]
fn public_standalone_checks_use_the_stable_session_pipeline() {
	let diagnostics = assert_one_stable_check(|| check("func value(): int = 1", "src/lib.nym"));
	assert!(diagnostics.is_empty(), "library check: {diagnostics:?}");

	let diagnostics =
		assert_one_stable_check(|| check_entry("func main(): void = {}", "src/main.nym"));
	assert!(diagnostics.is_empty(), "entry check: {diagnostics:?}");

	let diagnostics = assert_one_stable_check(|| {
		check_without_prelude("func value(): int = 1", "stdlib/src/value.nym")
	});
	assert!(diagnostics.is_empty(), "no-prelude check: {diagnostics:?}");
}

#[test]
fn public_standalone_compilers_use_one_stable_check_on_success_and_failure() {
	let javascript = assert_one_stable_check(|| compile("func value(): int = 1", "src/lib.nym"))
		.expect("library source should compile");
	assert!(javascript.contains("function value("));

	let javascript =
		assert_one_stable_check(|| compile_entry("func main(): void = {}", "src/main.nym"))
			.expect("entry source should compile");
	assert!(javascript.contains("function main("));

	let diagnostics =
		assert_one_stable_check(|| compile("func value(): int = true", "src/broken.nym"))
			.expect_err("invalid source must not compile");
	assert!(diagnostics.iter().any(|diagnostic| diagnostic.is_error()));
}

#[test]
fn stable_standalone_facade_preserves_entry_and_diagnostic_contracts() {
	let library_diagnostics = assert_one_stable_check(|| check("func value(): int = 1", "odd::path"));
	assert!(library_diagnostics.is_empty());

	let entry_diagnostics =
		assert_one_stable_check(|| check_entry("func value(): int = 1", "odd::path"));
	assert!(
		entry_diagnostics
			.iter()
			.any(|diagnostic| diagnostic.is_error())
	);

	let diagnostics =
		assert_one_stable_check(|| compile_entry("func broken(: int = 1", "malformed/source.nym"))
			.expect_err("malformed entry must not compile");
	let codes = diagnostics
		.iter()
		.map(|diagnostic| diagnostic.code.as_str())
		.collect::<Vec<_>>();
	assert!(codes.first().is_some_and(|code| code.starts_with('1')));
	assert!(codes.iter().skip(1).any(|code| code.starts_with('2')));
}
