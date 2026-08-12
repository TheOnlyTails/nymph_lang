use nymph_sema::check_module;
use nymph_syntax::parse_module;

fn errors(source: &str) -> Vec<String> {
	let parsed = parse_module(source, "test");
	let parse_errors: Vec<_> = parsed
		.diagnostics
		.iter()
		.filter(|diagnostic| diagnostic.is_error())
		.map(|diagnostic| diagnostic.message.to_string())
		.collect();
	assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}");
	check_module(&parsed.tree)
		.diags
		.into_iter()
		.filter(|diagnostic| diagnostic.is_error())
		.map(|diagnostic| diagnostic.message.to_string())
		.collect()
}

fn assert_ok(source: &str) {
	let errors = errors(source);
	assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn fixed_point_views_accept_self_cycles_diamonds_and_repetition() {
	assert_ok(
		"enum Root { A, B }
		 enum Left { ...Root, ...Left, L }
		 enum Right { ...Root, R }
		 enum CycleA { ...CycleB, X }
		 enum CycleB { ...CycleA, Y }
		 enum Diamond { ...Left, ...Right, ...Root, ...Root, D }
		 func widen(value: Root): Diamond = value
		 func cycle(value: CycleA): CycleB = value",
	);
}

#[test]
fn selected_variants_are_regular_types_and_require_refinement() {
	assert_ok(
		"enum Source { A, B(value: int) }
		 enum Selected { Source.A, C }
		 func selected(value: Source.A): Selected = value
		 func direct(): Selected = Source.A",
	);
	let diagnostics = errors(
		"enum Source { A, B }
		 enum Selected { Source.A }
		 func invalid(value: Source): Selected = value",
	);
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.contains("mismatched types")),
		"expected a set-inclusion mismatch, got {diagnostics:?}"
	);
}

#[test]
fn single_variant_identity_projects_only_generics_used_by_fields() {
	assert_ok(
		"enum Generic<T> { Some(value: T), None }
		 func erase(value: Generic<int>.None): Generic<string>.None = value",
	);
	let diagnostics = errors(
		"enum Generic<T> { Some(value: T), None }
		 func retain(value: Generic<int>.Some): Generic<string>.Some = value",
	);
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.contains("mismatched types")),
		"expected projected generic identities to differ, got {diagnostics:?}"
	);
}

#[test]
fn qualified_patterns_cover_embedded_source_variants() {
	assert_ok(
		"enum Source { A, B(value: int) }
		 enum View { ...Source, C }
		 func inspect(value: View): int = match (value) {
		   Source.A -> 0,
		   Source.B(value) -> value,
		   View.C -> 2,
		 }",
	);
}

#[test]
fn direct_deep_propagation_uses_set_inclusion() {
	assert_ok(
		"enum Narrow { A }
		 enum Wide { ...Narrow, B }
		 enum Result<T, E> { Ok(value: T), Error(error: E) }
		 func inner(): Result<int, Narrow> = Error(Narrow.A)
		 func outer(): Result<int, Wide> = Ok(inner()?)",
	);
}

#[test]
fn deep_propagation_rejects_missing_ambiguous_effectful_and_fallible_fallbacks() {
	let result = "enum Result<T, E> { Ok(value: T), Error(error: E) }";
	let missing = errors(&format!(
		"{result}\nstruct A\nstruct B\nfunc inner(): Result<int, A> = Error(A())\nfunc outer(): Result<int, B> = Ok(inner()?)"
	));
	assert!(!missing.is_empty(), "missing fallback must be diagnosed");

	let ambiguous = errors(&format!(
		"{result}\ninterface Into<Other> {{ func into(): Other }}\nstruct A\nstruct B\nimpl Into<B> for A {{ func into(): B = B() }}\nimpl Into<B> for A {{ func into(): B = B() }}\nfunc inner(): Result<int, A> = Error(A())\nfunc outer(): Result<int, B> = Ok(inner()?)"
	));
	assert!(
		!ambiguous.is_empty(),
		"ambiguous fallback must be diagnosed"
	);

	let effectful = errors(&format!(
		"{result}\neffect Io\ninterface Into<Other> {{ func into(): Other }}\nstruct A\nstruct B\nimpl Into<B> for A {{ func into(): B + !Io = B() }}\nfunc inner(): Result<int, A> = Error(A())\nfunc outer(): Result<int, B> = Ok(inner()?)"
	));
	assert!(
		!effectful.is_empty(),
		"effectful fallback must be diagnosed"
	);

	let fallible = errors(&format!(
		"{result}\ninterface Into<Other> {{ func into(): Other }}\nstruct A\nstruct B\nimpl Into<B> for A {{ func into(): Result<B, A> = Ok(B()) }}\nfunc inner(): Result<int, A> = Error(A())\nfunc outer(): Result<int, B> = Ok(inner()?)"
	));
	assert!(!fallible.is_empty(), "fallible fallback must be diagnosed");
}

#[test]
fn dispatch_uses_the_static_view_and_overlapping_views_compare() {
	assert_ok(
		"enum Source {
		   A
		   func value(): int = 1
		 }
		 enum View {
		   ...Source,
		   B
		   func value(): int = 2
		 }
		 func source(): int = Source.A.value()
		 func viewed(value: View): int = value.value()
		 func compare(left: Source, right: View): boolean = left == right
		 func rebound(value: View): int = match (value) {
		   source = Source.A -> source.value(),
		   View.B -> value.value(),
		 }",
	);
}
