//! Milestone B: the minimal multi-module driver (`check_program`).
//!
//! Flattening several modules into one program lets an item defined in one file be
//! used from another, with `import` statements dropped (every item shares one global
//! namespace after flattening).

use nymph_sema::check_program;
use nymph_syntax::parse_module;

fn check(sources: &[&str]) -> Vec<String> {
	let modules: Vec<_> = sources
		.iter()
		.enumerate()
		.map(|(i, src)| {
			let parsed = parse_module(src, format!("m{i}"));
			let parse_errors: Vec<_> = parsed
				.diagnostics
				.iter()
				.filter(|d| d.is_error())
				.map(|d| d.message.to_string())
				.collect();
			assert!(
				parse_errors.is_empty(),
				"source failed to parse: {parse_errors:?}\n---\n{src}"
			);
			parsed.tree
		})
		.collect();
	check_program(&modules)
		.diags
		.iter()
		.filter(|d| d.is_error())
		.map(|d| d.message.to_string())
		.collect()
}

fn assert_ok(sources: &[&str]) {
	let errors = check(sources);
	assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

#[test]
fn interface_from_another_module_resolves() {
	// `Show` is defined in one module and implemented in another; the `import` line is
	// a no-op after flattening but documents the dependency.
	assert_ok(&[
		"public interface Show { func show(): string }",
		"import @/show with (Show)
		 struct P(x: int)
		 impl Show for P { func show(): string = \"p\" }
		 func render(p: P): string = p.show()",
	]);
}

#[test]
fn type_from_another_module_resolves() {
	assert_ok(&[
		"public enum Option<T> { Some(value: T), None }",
		"import @/option with (Option)
		 func first(o: Option<int>): int = match (o) {
		   Some(value) -> value,
		   None -> 0,
		 }",
	]);
}

#[test]
fn cross_module_type_error_is_still_reported() {
	// Flattening must not paper over real errors: `show` returns a `string`, not `int`.
	let errors = check(&[
		"public interface Show { func show(): string }",
		"struct P(x: int)
		 impl Show for P { func show(): string = \"p\" }
		 func bad(p: P): int = p.show()",
	]);
	assert!(
		errors.iter().any(|e| e.contains("mismatched types")),
		"expected a mismatch, got: {errors:?}"
	);
}
