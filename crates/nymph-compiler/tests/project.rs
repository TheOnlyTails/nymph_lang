//! Integration tests for the multi-module project driver
//! (`nymph_compiler::project`): resolution, namespace/`with` binding,
//! visibility, cycles, and collisions — over a virtual, filesystem-free
//! project (an `FxHashMap<String, String>` keyed by canonical module path).
use nymph_ast::Span;
use nymph_compiler::{
	CompiledEntryRoot, CompilerOptions, check_project, check_project_with_embedded_std,
	compile_project, compile_project_library_with_embedded_std_and_options,
	compile_project_with_embedded_std_and_options, embedded_std_provider,
	project::compile_project_module_sources_with_std,
};
use rustc_hash::FxHashMap;
/// Build a `load` closure over a virtual project map.
fn loader(files: FxHashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
	move |key: &str| files.get(key).map(|s| (*s).to_string())
}
#[test]
fn resolves_at_import_against_the_source_root() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc main(): void = {}\nfunc used(): int = sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn embedded_std_project_check_resolves_project_and_std_graph() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/helper with (leaf)\nfunc main(): void = { let tree = leaf() }",
		),
		(
			"helper",
			"import std/collections/tree with (Tree)\npublic func leaf(): Tree<int> = Tree.Leaf(value = 1)",
		),
	]);
	let diags = check_project_with_embedded_std("main", &loader(files));

	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn const_token_expansion_runs_through_the_normal_project_pipeline() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as meta\n\
		 const func make_answer(value: int): meta.Tokens = \\
		(func answer(): int = $(value))\n\
		 $(make_answer(42))\n\
		 func main(): void = { let value: int = answer() }",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn aliased_meta_values_remain_compile_time_only() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as reflection\n\
		 const func only_at_compile_time(value: reflection.Tokens): reflection.Tokens = value\n\
		 func main(): void = {}",
	)]);
	let sources =
		compile_project_module_sources_with_std("main", &loader(files), &embedded_std_provider)
			.expect("project should compile");
	assert!(
		!sources["main"].contains("only_at_compile_time"),
		"meta-typed const functions must not leak into runtime output: {}",
		sources["main"]
	);
}

#[test]
fn runtime_declarations_cannot_expose_aliased_meta_values() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as reflection\n\
		 func leak(value: reflection.Tokens): reflection.Tokens = value\n\
		 func main(): void = {}",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.message == "std/meta values are compile-time-only"),
		"expected a compile-time-only diagnostic, got: {diagnostics:?}"
	);
}

#[test]
fn generated_source_expansions_work_in_type_and_expression_positions() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func make(): meta.Tokens = \
		   \\(func answer(value: $(\\(int))): $(\\(int)) = value + $(\\(2)))\n\
		 $(make())\n\
		 func result(): int = answer(40)\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn source_expansions_work_at_every_nested_grammar_destination() {
	let files = FxHashMap::from_iter([(
		"main",
		"const let type_tokens: meta.Tokens = \\(int)\n\
		 const let parameter: meta.Tokens = {\n\
		   let name = meta.Name.exposed(\"value\")\n\
		   \\($(name): int)\n\
		 }\n\
		 const let field: meta.Tokens = \\(value: int)\n\
		 const let variant: meta.Tokens = \\(Value(value: int))\n\
		 const let pattern: meta.Tokens = {\n\
		   let name = meta.Name.exposed(\"incremented\")\n\
		   \\($(name))\n\
		 }\n\
		 const let statement: meta.Tokens = {\n\
		   let value = meta.Name.exposed(\"value\")\n\
		   \\(let $(pattern): int = $(value) + 1)\n\
		 }\n\
		 const let arm: meta.Tokens = \\(41 -> 42, _ -> 0)\n\
		 struct Box($(field))\n\
		 enum Number { $(variant) }\n\
		 func answer($(parameter)): $(type_tokens) = {\n\
		   $(statement)\n\
		   match (incremented) { $(arm) }\n\
		 }\n\
		 func result(): int = answer(40)\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn shorthand_expansions_use_the_canonical_path_at_every_grammar_destination() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func type_tokens(): meta.Tokens = \\(int)\n\
		 const func parameter(): meta.Tokens = \\(value: int)\n\
		 const func field(): meta.Tokens = \\(value: int)\n\
		 const func variant(): meta.Tokens = \\(Value(value: int))\n\
		 const func pattern(): meta.Tokens = \\(_)\n\
		 const func expression(): meta.Tokens = \\(41)\n\
		 const func statement(): meta.Tokens = \\(let _: int = 40)\n\
		 const func arms(): meta.Tokens = \\(41 -> 42, _ -> 0)\n\
		 struct Box($field())\n\
		 enum Number { $variant() }\n\
		 func answer($parameter()): $type_tokens() = {\n\
		   $statement()\n\
		   let $pattern(): int = $expression()\n\
		   match (41) { $arms() }\n\
		 }\n\
		 func result(): int = answer(0)\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn shorthand_token_interpolation_splicing_and_labeled_arguments_match_long_form() {
	let shorthand = FxHashMap::from_iter([(
		"main",
		"const func value(base: int, extra: int): int = base + extra\n\
		 const func parameters(): #[meta.Tokens] = #[\\(left: int,), \\(right: int)]\n\
		 const func make(base: int, extra: int): meta.Tokens = \
		   \\(func generated(...$parameters()): int = $value(base, extra = extra))\n\
		 $make(40, extra = 2)\n\
		 func result(): int = generated(0, 0)\n\
		 func main(): void = {}",
	)]);
	let long_form = FxHashMap::from_iter([(
		"main",
		"const func value(base: int, extra: int): int = base + extra\n\
		 const func parameters(): #[meta.Tokens] = #[\\(left: int,), \\(right: int)]\n\
		 const func make(base: int, extra: int): meta.Tokens = \
		   \\(func generated(...$(parameters())): int = $(value(base, extra = extra)))\n\
		 $(make(40, extra = 2))\n\
		 func result(): int = generated(0, 0)\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(shorthand, "result", ""), "42");
	assert_eq!(run_project(long_form, "result", ""), "42");
}

#[test]
fn shorthand_generated_errors_retain_expansion_provenance() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func broken(): meta.Tokens = \\(func generated(: int) = 0)\n$broken()",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics.iter().any(|diagnostic| {
			diagnostic.diag.span.origin != nymph_ast::OriginId::SOURCE
				&& diagnostic
					.diag
					.labels
					.iter()
					.any(|label| label.message.contains("invoked here"))
		}),
		"missing shorthand expansion provenance: {diagnostics:?}"
	);
}

#[test]
fn expansion_generated_imports_participate_in_graph_discovery() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"const func generated_import(): meta.Tokens = \\
			(import @/generated with (answer))\n\
			 $(generated_import())\n\
			 func main(): void = { let value: int = answer() }",
		),
		("generated", "public func answer(): int = 42"),
	]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn generated_import_fixed_point_limit_is_labeled_and_actionable() {
	let mut files = FxHashMap::default();
	for index in 0..=64 {
		let key = if index == 0 {
			"main".to_string()
		} else {
			format!("generated_{index}")
		};
		let next = format!("generated_{}", index + 1);
		files.insert(
			key,
			format!(
				"const func generated_import(): meta.Tokens = \\(import @/{next})\n$(generated_import())"
			),
		);
	}
	files.insert("generated_65".to_string(), String::new());
	let load = |key: &str| files.get(key).cloned();
	let diagnostics = check_project("main", &load);
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| diagnostic.diag.code == "META-FIXED-POINT")
		.unwrap_or_else(|| panic!("missing fixed-point diagnostic: {diagnostics:?}"));

	assert_ne!(diagnostic.diag.span.origin, nymph_ast::OriginId::SOURCE);
	assert!(
		diagnostic
			.diag
			.help
			.as_deref()
			.is_some_and(|help| help.contains("cycle") || help.contains("stable output"))
	);
	assert!(
		diagnostic
			.diag
			.labels
			.iter()
			.any(|label| label.message.contains("macro was defined here"))
	);
}

#[test]
fn attached_macros_apply_to_imports_before_graph_discovery() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"const func inspect(target: meta.Import): meta.Tokens = \\()\n\
			 $[inspect()] import @/generated with (answer)\n\
			 func main(): void = { let value: int = answer() }",
		),
		("generated", "public func answer(): int = 42"),
	]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn typed_import_records_rebuild_before_graph_discovery() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"const let target: meta.Import = meta.Import.parse(\\(import @/generated with (answer)))\n\
			 $(meta.Import(\
			  root = target.root,\
			  path = target.path,\
			  alias = target.alias,\
			  names = target.names,\
			  span = target.span,\
			 ))\n\
			func main(): void = { let value: int = answer() }",
		),
		("generated", "public func answer(): int = 42"),
	]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn attached_macros_receive_the_typed_target_while_the_compiler_retains_it() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func inspect(target: meta.Struct): meta.Tokens = \\()\n\
		 $[inspect()] struct Point(x: int)\n\
		 func main(): void = { let point = Point(x = 1) }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn imported_const_functions_expand_with_their_definition_scope() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/macros with (make)\n\
			 $(make(41))\n\
			 func main(): void = {}",
		),
		(
			"macros",
			"const func plus_one(value: int): int = value + 1\n\
			 public const func make(value: int): meta.Tokens = \
			 \\(func answer(): int = $(plus_one(value)))",
		),
	]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn imported_const_macros_can_add_modules_to_the_project_graph() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/macros with (add_generated_import)\n\
			 $(add_generated_import())\n\
			 func main(): void = { generated() }",
		),
		(
			"macros",
			"public const func add_generated_import(): meta.Tokens = \
			 \\(import @/generated with (generated))",
		),
		("generated", "public func generated(): void = {}"),
	]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn imported_const_protocol_dispatch_uses_the_values_nominal_owner() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import std/meta as meta\n\
			 import @/macros with (make)\n\
			 struct Box\n\
			 impl Into<Other = meta.Tokens> for Box {\n\
			   func into(): meta.Tokens = \\(func generated(): int = 0)\n\
			 }\n\
			 $(make())\n\
			 func main(): void = { let value: int = generated() }",
		),
		(
			"macros",
			"import std/meta as meta\n\
			 public struct Box\n\
			 impl Into<Other = meta.Tokens> for Box {\n\
			   func into(): meta.Tokens = \\(func generated(): int = 42)\n\
			 }\n\
			 public const func make(): Box = Box()",
		),
	]);
	let compiled = compile_project_with_embedded_std_and_options(
		"main",
		&loader(files),
		&CompilerOptions::default(),
	)
	.expect("project should compile with the imported type's Into implementation");
	assert!(compiled.js.contains("42"));
}

#[test]
fn typed_meta_functions_are_constructible_and_convert_back_to_tokens() {
	let files = FxHashMap::from_iter([(
		"main",
		"const let target: meta.Function = meta.Function.parse(\\(func answer(): int = 0))\n\
		 $(meta.Function(\n\
		     visibility = target.visibility,\n\
		     name = target.name,\n\
		     is_const = false,\n\
		     is_async = target.is_async,\n\
		     parameters = target.parameters,\n\
		     return_type = target.return_type,\n\
		     body = meta.Expression.parse(\\(42)),\n\
		     span = target.span,\n\
		   ))\n\
		 func result(): int = answer()\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn public_meta_tokens_and_flat_token_variants_are_constructible() {
	let files = FxHashMap::from_iter([(
		"main",
		"const let answer: meta.Tokens = meta.Tokens(items = #[\
		   meta.Token.Func,\
		   meta.Token.Identifier(value = \"answer\"),\
		   meta.Token.LParen,\
		   meta.Token.RParen,\
		   meta.Token.Colon,\
		   meta.Token.IntType,\
		   meta.Token.Eq,\
		   meta.Token.Int(value = 42u),\
		 ])\n\
		 $(answer)\n\
		 func result(): int = answer()\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn public_meta_span_constructor_preserves_context_and_origin() {
	let source = "const func reject(target: meta.Struct): Result<meta.Tokens, meta.Diagnostic> = \
		 Error(error = meta.Diagnostic(\
		   message = \"constructed span\",\
		   span = meta.Span(\
		     start = 7u,\
		     end = 11u,\
		     context = meta.SyntaxContext.Fresh(origin = 3u, index = 5u),\
		     origin = 3u,\
		   ),\
		 ))\n\
		 $[reject()] struct Point";
	let files = FxHashMap::from_iter([("main", source)]);
	let diagnostics = check_project("main", &loader(files));
	let span = diagnostics
		.iter()
		.find(|diagnostic| diagnostic.diag.message == "constructed span")
		.unwrap_or_else(|| panic!("constructed diagnostic, got: {diagnostics:?}"))
		.diag
		.span;
	assert_eq!(span.start, 7);
	assert_eq!(span.end, 11);
	assert_eq!(span.origin.0, 3);
	assert_eq!(
		span.context,
		nymph_ast::SyntaxContext::Fresh {
			origin: nymph_ast::OriginId(3),
			index: 5,
		}
	);
}

#[test]
fn public_struct_and_declaration_records_rebuild_nested_members() {
	let files = FxHashMap::from_iter([(
		"main",
		"const let target: meta.Struct = meta.Struct.parse(\\(struct Point { namespace func answer(): int = 42 }))\n\
		 $(meta.Declaration.Struct(value = meta.Struct(\
		     visibility = target.visibility,\
		     name = target.name,\
		     fields = target.fields,\
		     members = target.members,\
		     span = target.span,\
		   )))\n\
		 func result(): int = Point.answer()\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn const_evaluation_supports_finite_collections_ranges_and_state_loops() {
	let files = FxHashMap::from_iter([(
		"main",
		"struct Pair(left: int, right: int)\n\
		 const func pair_value(): int = match (Pair(left = 40, right = 2)) {\
		   Pair(left = left, right = right) -> left + right,\
		 }\n\
		 const func sum(): int = loop (let index = 0, let total = 0) {\
		   if (index == 4) break total\n\
		   continue(index = index + 1, total = total + index)\
		 }\n\
		 const func last(): Option<int> = for (value in 40..=42) {\
		   if (value == 42) break value\
		 }\n\
		 const let base: int = #{ \"sum\": sum() + match (last()) {\
		   Some(value = value) -> value,\
		   None -> 0,\
		 } - 6 + pair_value() - 42 }[\"sum\"]\n\
		 const let answer: int = {\
		   let plus_one = value -> value + 1\n\
		   plus_one(base - 1)\
		 }\n\
		 $(\\(func result(): int = $(answer)))\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn broad_attached_macro_can_pattern_match_the_public_declaration_enum() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func inspect_struct(target: meta.Declaration): meta.Tokens = match (target) {\n\
		   meta.Declaration.Struct(value = value) -> \\(),\n\
		   _ -> \\(),\n\
		 }\n\
		 $[inspect_struct()] struct Point(x: int)\n\
		 func main(): void = { let point = Point(x = 1) }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn narrow_meta_records_expose_constructible_syntax_fields() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func derive_copy(target: meta.Struct): meta.Tokens = \
		\\(struct Copy(...$(target.fields,)))\n\
		 $[derive_copy()] struct Point(x: int, y: int)\n\
		 func main(): void = {\n\
		   let point = Point(x = 1, y = 2)\n\
		   let copy = Copy(x = 1, y = 2)\
		 }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn attached_macro_result_diagnostics_use_the_supplied_meta_span() {
	let source = "const func reject(target: meta.Struct): Result<meta.Tokens, meta.Diagnostic> = \
		 Error(error = meta.Diagnostic(message = \"derive failed\", span = target.span))\n\
		 $[reject()] struct Point(x: int)";
	let files = FxHashMap::from_iter([("main", source)]);
	let diagnostics = check_project("main", &loader(files));
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| diagnostic.diag.message == "derive failed")
		.expect("macro diagnostic");
	let start = source.find("struct Point").expect("target declaration");
	assert_eq!(
		diagnostic.diag.span,
		Span::new(start, start + "struct Point(x: int)".len())
	);
}

#[test]
fn attached_macro_reports_every_returned_diagnostic() {
	let source = "const func reject(target: meta.Struct): Result<meta.Tokens, #[meta.Diagnostic]> = \
		 Error(error = #[\
			meta.Diagnostic(message = \"first failure\", span = target.span), \
			meta.Diagnostic(message = \"second failure\", span = target.span)\
		 ])\n\
		 $[reject()] struct Point(x: int)";
	let files = FxHashMap::from_iter([("main", source)]);
	let diagnostics = check_project("main", &loader(files));
	let messages = diagnostics
		.iter()
		.map(|diagnostic| diagnostic.diag.message.as_str())
		.filter(|message| message.ends_with("failure"))
		.collect::<Vec<_>>();
	assert_eq!(messages, ["first failure", "second failure"]);
}

#[test]
fn attached_macro_accepts_result_ok_tokens() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func inspect(target: meta.Struct): Result<meta.Tokens, meta.Diagnostic> = \
		 Ok(value = \\())\n\
		 $[inspect()] struct Point(x: int)\n\
		 func main(): void = { let point = Point(x = 1) }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn direct_expansion_accepts_meta_syntax_values_and_result_wrappers() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func make(): Result<meta.Function, meta.Diagnostic> = \
		 Ok(value = meta.Function.parse(\\(func generated(): int = 42)))\n\
		 $(make())\n\
		 func main(): void = { let value: int = generated() }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn every_typed_meta_fragment_expands_directly_at_its_grammar_destination() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as meta\n\
		 const let expression: meta.Expression = meta.Expression.parse(\\(40 + 2))\n\
		 const let statement: meta.Statement = meta.Statement.parse(\\(let local: int = 1))\n\
		 const let pattern: meta.Pattern = meta.Pattern.parse(\\(answer))\n\
		 const let type_: meta.Type = meta.Type.parse(\\(int))\n\
		 const let parameter: meta.Parameter = meta.Parameter.parse(\\(input: int))\n\
		 const let field: meta.Field = meta.Field.parse(\\(value: int))\n\
		 const let variant: meta.Variant = meta.Variant.parse(\\(One))\n\
		 const let arm: meta.MatchArm = meta.MatchArm.parse(\\(_ -> 42))\n\
		 struct Box($(field))\n\
		 enum Choice { $(variant) }\n\
		 func generated($(parameter)): $(type_) = {\n\
		   $(statement)\n\
		   let $(pattern): int = $(expression)\n\
		   match (Choice.One) { $(arm) }\n\
		 }\n\
		 func main(): void = { let result: int = generated(0) }",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn direct_expansion_uses_user_defined_into_tokens() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as meta\n\
		 struct Answer(value: int)\n\
		 impl Into<Other = meta.Tokens> for Answer {\n\
		   func into(): meta.Tokens = \\(func generated(): int = $(this.value))\n\
		 }\n\
		 const func make(): Answer = Answer(value = 42)\n\
		 $(make())\n\
		 func main(): void = { let value: int = generated() }",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn direct_expansion_dispatches_user_into_for_enum_values() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as meta\n\
		 enum Answer { Value(amount: int) }\n\
		 impl Into<Other = meta.Tokens> for Answer {\n\
		   func into(): meta.Tokens = match (this) {\n\
		     Answer.Value(amount) -> \\(func generated(): int = $(amount)),\n\
		   }\n\
		 }\n\
		 const func make(): Answer = Answer.Value(amount = 42)\n\
		 $(make())\n\
		 func main(): void = { let value: int = generated() }",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn iterable_token_splicing_accepts_non_list_iterators_and_separators() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as meta\n\
		 struct ParameterTokens(index: int)\n\
		 struct Parameters\n\
		 impl Iterable<meta.Tokens> for Parameters {\n\
		   func iter(): ParameterTokens = ParameterTokens(index = 0)\n\
		 }\n\
		 impl Iterator<meta.Tokens> for ParameterTokens {\n\
		   func next(): Iteration<meta.Tokens, ParameterTokens> = if (this.index == 0) {\n\
		     Iteration.Yield(item = \\(left: int), next = ParameterTokens(index = 1))\n\
		   } else if (this.index == 1) {\n\
		     Iteration.Yield(item = \\(right: int), next = ParameterTokens(index = 2))\n\
		   } else Iteration.Done\n\
		 }\n\
		 const func make(name: meta.Name, parameters: ParameterTokens): meta.Tokens = \
		   \\(func $(name)(...$(parameters,)): int = 42)\n\
		 $(make(meta.Name.exposed(\"add\"), Parameters().iter()))\n\
		 $(make(meta.Name.exposed(\"one\"), ParameterTokens(index = 1)))\n\
		 $(make(meta.Name.exposed(\"zero\"), ParameterTokens(index = 2)))\n\
		 func main(): void = {\n\
		   let value: int = add(20, 22) + one(1) + zero()\n\
		 }",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn direct_macro_output_flattens_generic_iterators_in_order() {
	let files = FxHashMap::from_iter([(
		"main",
		"import std/meta as meta\n\
		 struct Functions(index: int)\n\
		 impl Iterator<meta.Function> for Functions {\n\
		   func next(): Iteration<meta.Function, Functions> = if (this.index == 0) {\n\
		     Iteration.Yield(\
		       item = meta.Function.parse(\\(func first(): int = 20)),\
		       next = Functions(index = 1),\
		     )\n\
		   } else if (this.index == 1) {\n\
		     Iteration.Yield(\
		       item = meta.Function.parse(\\(func second(): int = 22)),\
		       next = Functions(index = 2),\
		     )\n\
		   } else Iteration.Done\n\
		 }\n\
		 const func make(): Functions = Functions(index = 0)\n\
		 $(make())\n\
		 func main(): void = { let value: int = first() + second() }",
	)]);
	let diagnostics = check_project_with_embedded_std("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn stacked_attachments_are_additive_and_flatten_zero_or_many_outputs() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func emit_nothing(target: meta.Struct): meta.Tokens = \\()\n\
		 const func emit_many(target: meta.Struct): Result<#[meta.Function], #[meta.Diagnostic]> = \
		 Ok(value = #[\
		   meta.Function.parse(\\(func first(): int = 20)),\
		   meta.Function.parse(\\(func second(): int = 22)),\
		 ])\n\
		 $[emit_nothing()] $[emit_many()] struct Point(value: int)\n\
		 func main(): void = {\
		   let point = Point(value = first() + second())\
		 }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn every_stacked_attachment_is_checked_against_the_original_target() {
	let source = "const func emit_function(target: meta.Struct): meta.Tokens = \
		 \\(func generated(): int = 42)\n\
		 const func expects_function(target: meta.Function): meta.Tokens = \\()\n\
		 $[emit_function()] $[expects_function()] struct Point\n\
		 func main(): void = { let value: int = generated() }";
	let diagnostics = check_project("main", &loader(FxHashMap::from_iter([("main", source)])));
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| diagnostic.diag.message.contains("expects `meta.Function`"))
		.unwrap_or_else(|| panic!("missing original-target mismatch: {diagnostics:?}"));
	assert!(
		diagnostic
			.diag
			.labels
			.iter()
			.any(|label| label.message.contains("target is Struct")),
		"missing original target label: {diagnostic:?}"
	);
	assert!(
		!diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.message.contains("generated")
				&& diagnostic.diag.code != "META001"),
		"the successful sibling output was not retained: {diagnostics:?}"
	);
}

#[test]
fn attached_macros_accept_alias_narrow_targets() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func inspect_alias(target: meta.TypeAlias): meta.Tokens = \\()\n\
		 $[inspect_alias()] type Count = int\n\
		 func main(): void = {}",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn attached_macro_target_mismatch_is_reported_before_execution() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func only_struct(target: meta.Struct): meta.Tokens = \\($(target))\n\
		 $[only_struct()] func main(): void = {}",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.message.contains("expects `meta.Struct`")),
		"missing target mismatch: {diagnostics:?}"
	);
}

#[test]
fn token_splices_insert_the_separator_only_between_items() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func make_adder(parameters: #[meta.Tokens], body: meta.Tokens): meta.Tokens = {\n\
		   let name = meta.Name.exposed(\"add\")\n\
		   \\(func $(name)(...$(parameters,)): int = $(body))\n\
		 }\n\
		 $(make_adder(#[\\(x: int), \\(y: int)], \\(x + y)))\n\
		 func main(): void = { let result: int = add(20, 22) }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn const_let_is_evaluated_even_when_nothing_references_it() {
	let files = FxHashMap::from_iter([(
		"main",
		"const let broken: int = 1 / 0\nfunc main(): void = {}",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.message.contains("division by zero")),
		"missing const-evaluation diagnostic: {diagnostics:?}"
	);
}

#[test]
fn stacked_attachment_output_follows_source_order_after_the_original() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func first(target: meta.Struct): meta.Tokens = \
		\\(func attachment_source_order_first_marker(): int = 20)\n\
		 const func second(target: meta.Struct): meta.Tokens = \
		\\(func attachment_source_order_second_marker(): int = 22)\n\
		 $[first()] $[second()] struct Point(value: int)\n\
		 func main(): void = {\
		   let point = Point(value = attachment_source_order_first_marker() + attachment_source_order_second_marker())\
		 }",
	)]);
	let sources =
		compile_project_module_sources_with_std("main", &loader(files), &embedded_std_provider)
			.expect("additive attachments should compile");
	let source = &sources["main"];
	let point = source.find("Point").expect("original struct");
	let first = source
		.find("attachment_source_order_first_marker")
		.expect("first attachment output");
	let second = source
		.find("attachment_source_order_second_marker")
		.expect("second attachment output");
	assert!(
		point < first && first < second,
		"unexpected output order: {source}"
	);
}

#[test]
fn stacked_attachments_receive_byte_and_identity_equivalent_originals() {
	let source = "const func first(target: meta.Struct): meta.Struct = target\n\
		 const func second(target: meta.Struct): meta.Struct = target\n\
		 $[first()] $[second()] struct Point(value: int)\n\
		 func main(): void = {}";
	let diagnostics = check_project("main", &loader(FxHashMap::from_iter([("main", source)])));
	let collisions = diagnostics
		.iter()
		.filter(|diagnostic| {
			diagnostic
				.diag
				.message
				.contains("`Point` is defined more than once")
		})
		.collect::<Vec<_>>();
	let start = source.find("Point(value: int)").expect("target name");
	let end = start + "Point".len();
	assert_eq!(
		collisions.len(),
		2,
		"each attachment must receive and re-emit the original"
	);
	assert!(
		collisions.iter().all(|diagnostic| {
			diagnostic.diag.span.start == start
				&& diagnostic.diag.span.end == end
				&& diagnostic.diag.span.origin != nymph_ast::OriginId::SOURCE
		}),
		"{collisions:#?}"
	);
	assert!(
		collisions
			.iter()
			.any(|diagnostic| diagnostic.diag.labels.iter().any(|label| {
				label.message == "first defined here" && label.span == Span::new(start, end)
			}))
	);
}

#[test]
fn a_failing_attachment_does_not_hide_successful_sibling_output() {
	let source = "const func reject(target: meta.Struct): Result<meta.Tokens, meta.Diagnostic> = \
		 Error(error = meta.Diagnostic(message = \"independent failure\", span = target.span))\n\
		 const func emit(target: meta.Struct): meta.Tokens = \\(func generated(): int = 42)\n\
		 $[reject()] $[emit()] struct Point\n\
		 func main(): void = { let value: int = generated() }";
	let diagnostics = check_project("main", &loader(FxHashMap::from_iter([("main", source)])));
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.message == "independent failure"),
		"missing attachment failure: {diagnostics:?}"
	);
	assert!(
		!diagnostics
			.iter()
			.any(|diagnostic| diagnostic.diag.message.contains("generated")
				&& diagnostic.diag.message != "independent failure"),
		"successful sibling output was lost: {diagnostics:?}"
	);
}

#[test]
fn reemitting_an_attached_target_reports_a_provenance_labeled_collision() {
	let source = "const func duplicate(target: meta.Struct): meta.Struct = target\n\
		 $[duplicate()] struct Point\n\
		 func main(): void = {}";
	let diagnostics = check_project("main", &loader(FxHashMap::from_iter([("main", source)])));
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| {
			diagnostic
				.diag
				.message
				.contains("`Point` is defined more than once")
		})
		.unwrap_or_else(|| panic!("missing normal collision: {diagnostics:?}"));
	assert!(
		diagnostic
			.diag
			.labels
			.iter()
			.any(|label| label.message == "first defined here"
				&& label.span.origin == nymph_ast::OriginId::SOURCE)
	);
	assert!(
		diagnostic
			.diag
			.labels
			.iter()
			.any(|label| label.message == "macro was defined here"),
		"missing expansion provenance: {diagnostic:?}"
	);
}

#[test]
fn fresh_names_are_unique_and_exposed_names_bind_at_the_expansion_site() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func private_helper(): meta.Tokens = {\n\
		   let name = meta.Name.fresh(\"helper\")\n\
		   \\(func $(name)(): int = 1)\n\
		 }\n\
		 const func public_answer(): meta.Tokens = {\n\
		   let name = meta.Name.exposed(\"answer\")\n\
		   \\(func $(name)(): int = 42)\n\
		 }\n\
		 $(private_helper())\n\
		 $(private_helper())\n\
		 $(public_answer())\n\
		 func main(): void = { let value: int = answer() }",
	)]);
	let diagnostics = check_project("main", &loader(files));
	assert!(
		diagnostics.is_empty(),
		"unexpected diagnostics: {diagnostics:?}"
	);
}

#[test]
fn definition_context_names_bind_without_capturing_same_text_at_the_call_site() {
	let files = FxHashMap::from_iter([(
		"main",
		"func helper(): int = 41\n\
		 const func generate(): meta.Tokens = {\n\
		   let exported = meta.Name.exposed(\"generated\")\n\
		   \\(func helper(): int = 1 func $(exported)(): int = helper())\n\
		 }\n\
		 $(generate())\n\
		 func result(): int = helper() + generated()\n\
		 func main(): void = {}",
	)]);
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn const_call_depth_limit_reports_an_expansion_trace() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func recurse(): meta.Tokens = recurse()\n\
		 $(recurse())\n\
		 func main(): void = {}",
	)]);
	let diagnostics = check_project("main", &loader(files));
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| diagnostic.diag.message.contains("call depth"))
		.unwrap_or_else(|| panic!("missing deterministic limit diagnostic: {diagnostics:?}"));
	assert!(
		diagnostic
			.diag
			.notes
			.iter()
			.any(|note| note.contains("while evaluating const function `recurse`")),
		"missing expansion trace: {diagnostic:?}"
	);
}

#[test]
fn external_declarations_expose_typed_function_and_let_variants() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func inspect(target: meta.ExternalDeclaration): Result<meta.Tokens, meta.Diagnostic> = match (target) {\
		   meta.ExternalDeclaration.Function(value = function) -> Error(error = meta.Diagnostic(message = \"saw external function\", span = function.span)),\
		   meta.ExternalDeclaration.Let(value = let_) -> Error(error = meta.Diagnostic(message = \"saw external let\", span = let_.span)),\
		 }\n\
		 $[inspect()] external(host_value) let value: int\n\
		 $[inspect()] external(host_call) func call(value: int): int\n\
		 func main(): void = {}",
	)]);
	let diagnostics = check_project("main", &loader(files));
	let messages = diagnostics
		.iter()
		.map(|diagnostic| diagnostic.diag.message.as_str())
		.collect::<Vec<_>>();
	assert!(
		messages.contains(&"saw external function"),
		"{diagnostics:?}"
	);
	assert!(messages.contains(&"saw external let"), "{diagnostics:?}");
}

#[test]
fn generated_parse_diagnostics_walk_embedded_provenance() {
	let source = "const func broken(): meta.Tokens = \\(func generated(): = 1)\n\
		 $(broken())\n\
		 func main(): void = {}";
	let files = FxHashMap::from_iter([("main", source)]);
	let diagnostics = check_project("main", &loader(files));
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| {
			diagnostic
				.diag
				.notes
				.iter()
				.any(|note| note.contains("while parsing compile-time expansion output"))
		})
		.unwrap_or_else(|| panic!("missing generated parse diagnostic: {diagnostics:?}"));
	assert!(
		diagnostic
			.diag
			.notes
			.iter()
			.any(|note| note.contains("expanded at") && note.contains("macro definition")),
		"missing provenance trace: {diagnostic:?}"
	);
	assert!(
		diagnostic.diag.labels.len() >= 2,
		"missing invocation/definition labels: {diagnostic:?}"
	);
	assert!(
		diagnostic.diag.help.is_some(),
		"missing generated-code help: {diagnostic:?}"
	);
	let rendered =
		nymph_diagnostics::render("main.nym", source, std::slice::from_ref(&diagnostic.diag));
	assert!(
		rendered.contains("fix the generated syntax"),
		"diagnostic was not pretty-rendered with help: {rendered}"
	);
	assert!(
		rendered.contains("macro was defined here"),
		"missing rendered provenance label: {rendered}"
	);
}

#[test]
fn generated_type_diagnostics_walk_embedded_provenance() {
	let files = FxHashMap::from_iter([(
		"main",
		"const func broken(): meta.Tokens = \\(func generated(): int = true)\n\
		 $(broken())\n\
		 func main(): void = {}",
	)]);
	let diagnostics = check_project("main", &loader(files));
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| diagnostic.diag.span.origin != nymph_ast::OriginId::SOURCE)
		.unwrap_or_else(|| panic!("missing generated type diagnostic: {diagnostics:?}"));
	assert!(
		diagnostic
			.diag
			.notes
			.iter()
			.any(|note| note.contains("expanded at") && note.contains("macro definition")),
		"missing provenance trace: {diagnostic:?}"
	);
	assert!(
		diagnostic.diag.labels.len() >= 2,
		"missing invocation/definition labels: {diagnostic:?}"
	);
	assert!(
		diagnostic.diag.help.is_some(),
		"missing generated-code help: {diagnostic:?}"
	);
}

#[test]
fn meta_diagnostics_have_actionable_help_and_const_call_labels() {
	let source = "func runtime_only(): int = 1\n\
		 const func broken(): meta.Tokens = \\(func value(): int = $(runtime_only()))\n\
		 $(broken())";
	let files = FxHashMap::from_iter([("main", source)]);
	let diagnostics = check_project("main", &loader(files));
	let diagnostic = diagnostics
		.iter()
		.find(|diagnostic| diagnostic.diag.code == "META001")
		.unwrap_or_else(|| panic!("missing META001 diagnostic: {diagnostics:?}"));
	assert!(
		diagnostic
			.diag
			.help
			.as_deref()
			.is_some_and(|help| help.contains("const func")),
		"missing practical const correction: {diagnostic:?}"
	);
	assert!(
		diagnostic
			.diag
			.labels
			.iter()
			.any(|label| label.message.contains("`broken` was called here")),
		"missing const call label: {diagnostic:?}"
	);
}

#[test]
fn entry_compilation_propagates_all_six_static_root_adapters() {
	let cases = [
		("func main(): void = {}", "void"),
		("func main(): Option<void> = None", "option"),
		(
			"func main(): Result<void, string> = Ok(value = {})",
			"result",
		),
		("async func main(): void = {}", "task-void"),
		("async func main(): Option<void> = None", "task-option"),
		(
			"async func main(): Result<void, string> = Ok(value = {})",
			"task-result",
		),
	];
	for (source, expected) in cases {
		let load = |key: &str| (key == "main").then(|| source.to_string());
		let compiled =
			compile_project_with_embedded_std_and_options("main", &load, &CompilerOptions::default())
				.expect("root project should compile");
		let actual = match compiled.entry_root.as_ref().expect("entry adapter") {
			CompiledEntryRoot::Void => "void",
			CompiledEntryRoot::Option { binding } => {
				assert!(!binding.is_empty());
				"option"
			}
			CompiledEntryRoot::Result { binding } => {
				assert!(!binding.is_empty());
				"result"
			}
			CompiledEntryRoot::TaskVoid => "task-void",
			CompiledEntryRoot::TaskOption { binding } => {
				assert!(!binding.is_empty());
				"task-option"
			}
			CompiledEntryRoot::TaskResult { binding } => {
				assert!(!binding.is_empty());
				"task-result"
			}
		};
		assert_eq!(actual, expected);
	}
}

#[test]
fn ordinary_build_is_an_inert_importable_module_without_node_launcher_policy() {
	let load = |key: &str| (key == "main").then(|| "func main(): Option<void> = None".to_string());
	let compiled = compile_project_library_with_embedded_std_and_options(
		"main",
		&load,
		&CompilerOptions::default(),
	)
	.expect("library project should compile");
	assert_eq!(compiled.entry_root, None);
	for forbidden in [
		"nymphStartRoot",
		"execution cancelled",
		"main returned None",
		"main();",
	] {
		assert!(
			!compiled.js.contains(forbidden),
			"ordinary module contains {forbidden:?}"
		);
	}
	let output = std::process::Command::new("node")
		.args(["--input-type=module", "--eval", &compiled.js])
		.output()
		.expect("Node should import the ordinary module");
	assert!(
		output.status.success(),
		"{}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert!(output.stdout.is_empty());
	assert!(output.stderr.is_empty());
}

#[test]
fn project_modules_import_one_shared_box_runtime_instead_of_inlining_it() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/helper with (value)\nfunc main(): void = { let result = value() }",
		),
		("helper", "public func value(): int = 1"),
	]);

	let sources = compile_project_module_sources_with_std("main", &loader(files), &|_| None)
		.expect("project should compile");
	let helper = &sources["helper"];

	assert!(
		helper.lines().any(|line| line.starts_with("import { ")
			&& line.contains("NInt")
			&& line.ends_with("from \"std/box\";")),
		"boxed project values should import the canonical runtime: {helper}"
	);
	assert!(
		!helper.contains("class NBox"),
		"project modules must not inline a private box runtime copy: {helper}"
	);
}

#[test]
fn resolves_relative_current_and_parent_imports() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import ./geometry/vec with (make)\nfunc main(): void = {}\nfunc used(): int = make(1)",
		),
		(
			"geometry/vec",
			"import ../helpers with (id)\nfunc make(x: int): int = id(x)",
		),
		("helpers", "func id(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn missing_module_is_an_unresolved_import_diagnostic() {
	let files = FxHashMap::from_iter([("main", "import @/nope with (x)\nfunc main(): void = {}")]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("UNRESOLVED")));
}

#[test]
fn missing_module_diagnostic_is_attributed_to_the_importer_not_the_missing_target() {
	// The diagnostic must point at the module that WROTE the bad `import`
	// (whose source actually exists, and can be rendered), not at the
	// nonexistent target — otherwise the CLI falls back to an empty source
	// and the wrong filename when rendering it.
	let files = FxHashMap::from_iter([("main", "import @/nope with (x)\nfunc main(): void = {}")]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(
		diags.iter().any(|d| d.module == "main"),
		"expected the diagnostic attributed to `main`, got: {diags:?}"
	);
	assert!(
		diags.iter().all(|d| d.module != "nope"),
		"diagnostic must not be attributed to the nonexistent target module: {diags:?}"
	);
}

#[test]
fn parent_import_escaping_the_source_root_is_a_diagnostic() {
	let files = FxHashMap::from_iter([("main", "import ../nope with (x)\nfunc main(): void = {}")]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("ESCAPES-ROOT")));
}
#[test]
fn import_cycle_is_a_clean_diagnostic_not_a_hang() {
	let files = FxHashMap::from_iter([
		("main", "import @/a with (f)\nfunc main(): void = { f() }"),
		("a", "import @/b with (g)\nfunc f(): void = g()"),
		("b", "import @/a with (f)\nfunc g(): void = f()"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("CYCLE")));
}
#[test]
fn namespace_access_resolves_to_the_imported_function() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math\nfunc main(): void = {}\nfunc used(): int = math.sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn namespace_alias_binds_under_the_alias_not_the_original_name() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math as m\nfunc main(): void = {}\nfunc used(): int = m.sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn with_alias_binds_the_renamed_name_unqualified() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin as sine)\nfunc main(): void = {}\nfunc used(): int = sine(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn private_name_cannot_be_imported_unqualified() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (helper)\nfunc main(): void = { helper() }",
		),
		("math", "private func helper(): void = {}"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(
		diags.iter().any(|d| d.diag.code.contains("PRIVATE")),
		"{diags:?}"
	);
}
#[test]
fn private_name_cannot_be_imported_via_namespace_access() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (public_value)\nfunc main(): void = { math.helper() }",
		),
		(
			"math",
			"private func helper(): void = {}\npublic let public_value = 1",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert_eq!(diags.len(), 1, "{diags:?}");
	assert_eq!(diags[0].diag.code, "IMPORT-PRIVATE-NAME", "{diags:?}");
}

#[test]
fn missing_imported_namespace_member_has_a_structured_import_diagnostic() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (public_value)\nfunc read(): int = math.missing()\nfunc main(): void = { let ignored = read() }",
		),
		("math", "public let public_value: int = 1"),
	]);
	let diags = check_project("main", &loader(files));
	assert_eq!(diags.len(), 1, "{diags:?}");
	assert_eq!(diags[0].diag.code, "IMPORT-UNRESOLVED-NAME", "{diags:?}");
}
#[test]
fn private_helper_is_still_usable_within_its_own_module() {
	// A private decl stays fully usable *inside* its own module — only the
	// cross-module boundary excludes it.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc main(): void = {}\nfunc used(): int = sin(1)",
		),
		(
			"math",
			"private func helper(x: int): int = x\nfunc sin(x: int): int = helper(x)",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}
#[test]
fn with_name_colliding_with_a_local_declaration_is_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc sin(x: int): int = x\nfunc main(): void = { sin(1) }",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("COLLISION")));
}
#[test]
fn two_import_namespaces_with_the_same_name_are_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math\nimport @/geometry as math\nfunc main(): void = {}",
		),
		("math", "func sin(x: int): int = x"),
		("geometry", "func area(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(diags.iter().any(|d| d.diag.code.contains("COLLISION")));
}
#[test]
fn unresolved_with_name_is_diagnosed() {
	let files = FxHashMap::from_iter([
		("main", "import @/math with (nope)\nfunc main(): void = {}"),
		("math", "func sin(x: int): int = x"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(!diags.is_empty());
	assert!(
		diags
			.iter()
			.any(|d| d.diag.code.contains("UNRESOLVED-NAME"))
	);
}
#[test]
fn compiles_a_multi_module_project() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math with (sin)\nfunc main(): void = {}\nfunc used(): int = sin(1)",
		),
		("math", "func sin(x: int): int = x"),
	]);
	let result = compile_project("main", &loader(files));
	let compiled = result.unwrap_or_else(|d| panic!("expected a clean compile, got: {d:?}"));
	assert!(compiled.js.contains("main"));
	assert_eq!(compiled.entry_main, "main");
}
/// Emit the compiled project, append a call to its (mangled) entry `main`,
/// then log the (mangled) entry-module function named `call_fn` invoked with
/// `args` verbatim, run under Node, and return trimmed stdout — mirrors
/// `crates/nymph-codegen/tests/run_node.rs`'s single-module `run` helper,
/// adapted for the fact every top-level name in a project build is mangled.
fn run_project(files: FxHashMap<&'static str, &'static str>, call_fn: &str, args: &str) -> String {
	use std::io::Write;
	use std::process::Command;
	use std::sync::atomic::{AtomicU64, Ordering};
	let result = compile_project("main", &loader(files));
	let compiled = result.unwrap_or_else(|d| panic!("expected a clean compile, got: {d:?}"));
	let call_symbol = compiled.entry_symbol(call_fn);
	let mut js = compiled.js;
	js.push_str(&format!("\n{}();\n", compiled.entry_main));
	js.push_str(&format!("console.log(String({call_symbol}({args}).v));\n"));
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!(
		"nymph_project_run_{}_{unique}.mjs",
		std::process::id()
	));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();
	let output = Command::new("node")
		.arg(&path)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("run node");
	let _ = std::fs::remove_file(&path);
	assert!(
		output.status.success(),
		"node failed:\n{}\n--- js ---\n{}",
		String::from_utf8_lossy(&output.stderr),
		js
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}
#[test]
fn three_module_project_runs_under_node() {
	// entry imports a helper module that imports another; the value threads
	// through all three.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/geometry with (area)\nfunc main(): void = {}\nfunc result(): int = area(3, 4)",
		),
		(
			"geometry",
			"import @/math with (mul)\nfunc area(w: int, h: int): int = mul(w, h)",
		),
		("math", "func mul(a: int, b: int): int = a * b"),
	]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "12");
}

#[test]
fn imported_generic_callable_alias_captures_its_hidden_type_object() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/seed with (direct)\nfunc main(): void = {}\nfunc answer(): int = { let alias = direct\n alias(0, 40) }",
		),
		(
			"seed",
			"interface Seed { func seed(value: int): int }\nimpl Seed for int { func seed(value: int): int = value + 1 }\npublic func direct<T: Seed>(marker: T, value: int): int = T.seed(value)",
		),
	]);
	assert_eq!(run_project(files, "answer", ""), "41");
}
#[test]
fn namespace_and_with_together_run_under_node() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/math as m with (sin)\nfunc main(): void = {}\nfunc result(): int = m.cos(sin(1))",
		),
		(
			"math",
			"func sin(x: int): int = x + 1\nfunc cos(x: int): int = x * 10",
		),
	]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "20");
}

#[test]
fn imported_direct_and_default_interface_methods_run_under_node() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/dep with (Cell)\nfunc main(): void = {}\nfunc result(): int = Cell(value = 2).read() + Cell(value = 3).twice()",
		),
		(
			"dep",
			"public interface Read<Output> { func read(): Output\nfunc twice(): Output = this.read() }\npublic struct Cell(value: int) { impl Read<Output = int> { func read(): int = this.value } }",
		),
	]);

	assert_eq!(run_project(files, "result", ""), "5");
}

#[test]
fn imported_struct_construction_lowers_to_new_not_a_plain_call() {
	// Cross-module values require stable runtime identity because
	// flagged: an imported struct constructor call must lower to `new`, not
	// a plain function call (which would just crash at runtime).
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/geometry with (Point)\nfunc main(): void = {}\nfunc result(): int = Point(x = 3, y = 4).x",
		),
		("geometry", "struct Point(x: int, y: int)"),
	]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "3");
}

#[test]
fn imported_struct_clone_preserves_private_fields_without_rerunning_defaults() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/vault with (Vault)\nfunc main(): void = {}\nfunc result(): int = { let source = Vault.make(1, 9)\nlet updated = Vault(...source, shown = 2)\nupdated.shown * 10 + updated.reveal() }",
		),
		(
			"vault",
			"public struct Vault(public shown: int, private secret: int = 7) { public namespace func make(shown: int, secret: int): Vault = Vault(shown = shown, secret = secret)\npublic func reveal(): int = this.secret }",
		),
	]);
	assert_eq!(run_project(files, "result", ""), "29");
}

#[test]
fn imported_struct_hidden_fields_block_fresh_construction_and_require_pattern_omission() {
	let fresh = FxHashMap::from_iter([
		(
			"main",
			"import @/vault with (Vault)\nfunc main(): void = {}\nfunc bad(): Vault = Vault(shown = 1)",
		),
		(
			"vault",
			"public struct Vault(public shown: int, private secret: int = 7)",
		),
	]);
	let diagnostics = check_project("main", &loader(fresh));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic
			.diag
			.message
			.contains("cannot be constructed fresh because it has hidden fields")
	}));

	let pattern = FxHashMap::from_iter([
		(
			"main",
			"import @/vault with (Vault)\nfunc main(): void = {}\nfunc bad(value: Vault): int = match (value) { Vault(shown) -> shown }",
		),
		(
			"vault",
			"public struct Vault(public shown: int, private secret: int)",
		),
	]);
	let diagnostics = check_project("main", &loader(pattern));
	assert!(diagnostics.iter().any(|diagnostic| {
		diagnostic
			.diag
			.message
			.contains("partial struct pattern must end with anonymous `...`")
	}));
}

#[test]
fn same_module_struct_pattern_matches_after_own_name_mangling() {
	// A single-file project (no imports at all) still goes through the
	// project driver's own-name mangling (every module gets a `$m{tag}$`
	// rename, entry `main` excepted). A bare struct-constructor pattern
	// matching a struct declared IN THE SAME MODULE must still resolve.
	let files = FxHashMap::from_iter([(
		"main",
		"struct Point(x: int, y: int)\nfunc main(): void = {}\nfunc result(): int = match (Point(x = 1, y = 2)) { Point(x = x, y = y) -> x + y }",
	)]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn same_module_struct_pattern_runs_under_node() {
	let files = FxHashMap::from_iter([(
		"main",
		"struct Point(x: int, y: int)\nfunc main(): void = {}\nfunc result(): int = match (Point(x = 1, y = 2)) { Point(x = x, y = y) -> x + y }",
	)]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "3");
}

#[test]
fn same_module_qualified_enum_variant_pattern_matches_after_own_name_mangling() {
	// A qualified enum-variant pattern (`Color.Red`) against an enum declared
	// in the SAME module (no imports) must still resolve after the module's
	// own `Color` name gets mangled.
	let files = FxHashMap::from_iter([(
		"main",
		"enum Color { Red, Green }\nfunc main(): void = {}\nfunc result(): int = match (Color.Red) { Color.Red -> 1, Color.Green -> 2 }",
	)]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn same_module_qualified_enum_variant_pattern_runs_under_node() {
	let files = FxHashMap::from_iter([(
		"main",
		"enum Color { Red, Green }\nfunc main(): void = {}\nfunc result(): int = match (Color.Red) { Color.Red -> 1, Color.Green -> 2 }",
	)]);
	let out = run_project(files, "result", "");
	assert_eq!(out, "1");
}

#[test]
fn dependency_with_a_genuine_type_error_is_reported_not_panicked() {
	// `geometry` has a plain, unrelated type error (no name shadowing
	// whatsoever). Checking `main` (which imports and calls it) must report
	// the diagnostic — not panic via the prelude-flattening machinery's
	// internal invariant, which assumed a prelude entry was always a
	// trusted, bug-free stdlib clone.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/geometry with (bad)\nfunc main(): void = {}\nfunc used(): int = bad()",
		),
		("geometry", "func bad(): int = true"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		!diags.is_empty(),
		"expected the dependency's genuine type error to be reported"
	);
}

#[test]
fn imported_enum_is_emitted_once_and_the_bundle_is_valid_js() {
	// A cross-module enum must be emitted only on its own module's turn.
	// `materialize_referenced_prelude_enums` must not treat every prelude entry
	// like the ambient stdlib), so the concatenated bundle declared it twice and
	// Node crashed at load with "Identifier ... has already been declared".
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/shapes with (Color)\nfunc pick(): Color = Color.Red\nfunc main(): void = {}",
		),
		("shapes", "public enum Color { Red, Green }"),
	]);
	let js = compile_project("main", &loader(files))
		.expect("project should compile")
		.js;

	// `node --check` catches a duplicate top-level declaration (a SyntaxError)
	// without executing anything.
	let path = std::env::temp_dir().join(format!("nymph_project_enum_{}.mjs", std::process::id()));
	std::fs::write(&path, &js).unwrap();
	let out = std::process::Command::new("node")
		.arg("--check")
		.arg(&path)
		.output()
		.expect("spawn node");
	let _ = std::fs::remove_file(&path);
	assert!(
		out.status.success(),
		"emitted bundle is not valid JS (imported enum likely duplicated):\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}

#[test]
fn a_namespace_name_colliding_with_a_with_name_is_a_diagnostic() {
	// The spec requires a namespace name and a `with`-bound name sharing an
	// identifier to be a diagnostic, not a silent double-bind. Both checks must
	// inspect the namespace and `with` tables or this clash slips through.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/a as foo\nimport @/b with (foo)\nfunc main(): void = {}",
		),
		("a", "public func x(): int = 1"),
		("b", "public func foo(): int = 2"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags.iter().any(|d| d.diag.code.contains("COLLISION")),
		"expected a namespace/with name-collision diagnostic, got: {diags:?}"
	);
}

#[test]
fn interface_only_dependency_module_bundles_successfully() {
	// A dependency whose only top-level declaration is a (default-visibility)
	// `interface` must not break bundling: stable lowering never emits
	// a JS binding for an `Interface` declaration, so the synthesized
	// `export`/`import` lines must not name it either.
	let files = FxHashMap::from_iter([
		("main", "import @/shapes\nfunc main(): void = {}"),
		("shapes", "interface Shape { func area(): int }"),
	]);
	let result = compile_project("main", &loader(files));
	assert!(
		result.is_ok(),
		"expected a clean compile, got: {:?}",
		result.err()
	);
}

#[test]
fn type_alias_only_dependency_module_bundles_successfully() {
	// Same as above for a `type alias`-only dependency: `TypeAlias` also
	// never lowers to a JS binding.
	let files = FxHashMap::from_iter([
		("main", "import @/aliases\nfunc main(): void = {}"),
		("aliases", "type MyInt = int"),
	]);
	let result = compile_project("main", &loader(files));
	assert!(
		result.is_ok(),
		"expected a clean compile, got: {:?}",
		result.err()
	);
}

#[test]
fn imported_type_alias_participates_in_consumer_type_checking() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/aliases with (MyInt)\nfunc main(): void = {}\nfunc identity(value: MyInt): MyInt = value",
		),
		("aliases", "public type MyInt = int"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(diags.is_empty(), "expected a clean project, got: {diags:?}");
}

#[test]
fn conflicting_implementations_from_distinct_dependencies_are_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/first\nimport @/second\nfunc main(): void = {}",
		),
		("protocol", "public interface Show { func show(): string }"),
		("model", "public struct Item(value: int)"),
		(
			"first",
			"import @/protocol with (Show)\nimport @/model with (Item)\nimpl Show for Item { func show(): string = \"first\" }",
		),
		(
			"second",
			"import @/protocol with (Show)\nimport @/model with (Item)\nimpl Show for Item { func show(): string = \"second\" }",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags.iter().any(|diagnostic| diagnostic
			.diag
			.message
			.contains("conflicting implementations")),
		"expected an imported coherence diagnostic, got: {diags:?}"
	);
}

#[test]
fn namespace_only_dependency_module_bundles_successfully() {
	// A top-level `namespace` also never lowers to a JS binding today — same
	// hazard as interface/type-alias.
	let files = FxHashMap::from_iter([
		("main", "import @/ns\nfunc main(): void = {}"),
		("ns", "namespace Foo { func bar(): int = 1 }"),
	]);
	let result = compile_project("main", &loader(files));
	assert!(
		result.is_ok(),
		"expected a clean compile, got: {:?}",
		result.err()
	);
}

#[test]
fn a_with_name_colliding_with_a_namespace_name_is_a_diagnostic() {
	// The reverse order (namespace declared after the `with`-name) must also
	// collide, so the with-name check must cross-reference the namespaces table.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/b with (foo)\nimport @/a as foo\nfunc main(): void = {}",
		),
		("a", "public func x(): int = 1"),
		("b", "public func foo(): int = 2"),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags.iter().any(|d| d.diag.code.contains("COLLISION")),
		"expected a with/namespace name-collision diagnostic, got: {diags:?}"
	);
}

#[test]
fn project_module_named_like_a_removed_runtime_shim_remains_distinct() {
	let files = FxHashMap::from_iter([
		("main", "import @/std/option\nfunc main(): void = {}"),
		("std/option", "public func project_value(): int = 1"),
	]);
	assert!(
		compile_project("main", &loader(files)).is_ok(),
		"the compiler runtime's exact owner must not collide with a project std/option module"
	);
}

#[test]
fn project_module_cannot_be_silently_replaced_by_an_intrinsic_module() {
	let files = FxHashMap::from_iter([
		("main", "import @/std/box\nfunc main(): void = {}"),
		("std/box", "public func local(): int = 1"),
	]);
	let diags = match compile_project("main", &loader(files)) {
		Ok(_) => panic!("an intrinsic runtime module must not overwrite a project dependency"),
		Err(diags) => diags,
	};
	assert!(
		diags
			.iter()
			.any(|d| d.diag.code == "STABLE-INTRINSIC-COLLISION"),
		"expected a runtime-module collision diagnostic, got: {diags:?}"
	);
}

#[test]
fn canonical_runtime_functions_are_emitted_once_for_multiple_consumers() {
	std::thread::Builder::new()
		.stack_size(8 * 1024 * 1024)
		.spawn(canonical_runtime_functions_are_emitted_once)
		.unwrap()
		.join()
		.unwrap();
}

fn canonical_runtime_functions_are_emitted_once() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/left with (left_values)\nimport @/right with (right_values)\nfunc main(): void = {}\nfunc result(): int = left_values()[0] + right_values()[0]",
		),
		("left", "public func left_values(): #[int] = #[2, 1].sort()"),
		(
			"right",
			"public func right_values(): #[int] = #[4, 3].sort()",
		),
	]);
	let sources = compile_project_module_sources_with_std("main", &loader(files.clone()), &|_| None)
		.unwrap_or_else(|diags| panic!("canonical graph should compile: {diags:?}"));
	let list = sources
		.get("@nymph/runtime/collections/list")
		.expect("canonical list owner");
	let sort_declarations = list
		.lines()
		.filter(|line| {
			line.starts_with("let ")
				&& line.contains(" = nymphCallable(function(")
				&& line.contains("$list$i")
				&& line
					.split_once(" =")
					.is_some_and(|(binding, _)| binding.ends_with("$sort"))
		})
		.collect::<Vec<_>>();
	assert_eq!(sort_declarations.len(), 1, "{list}");
	let sort_binding = sort_declarations[0]
		.trim_start_matches("let ")
		.split_once(" =")
		.unwrap()
		.0;
	let ops = &sources["@nymph/runtime/ops"];
	let compare_declarations = ops
		.lines()
		.filter(|line| {
			line.starts_with("let ")
				&& line.contains(" = nymphCallable(function(")
				&& line.contains("$int$i")
				&& line
					.split_once(" =")
					.is_some_and(|(binding, _)| binding.ends_with("$compare_to"))
		})
		.collect::<Vec<_>>();
	assert_eq!(compare_declarations.len(), 1, "{ops}");
	for consumer in ["left", "right"] {
		let list_import = sources[consumer].lines().find(|line| {
			line.ends_with("from \"@nymph/runtime/collections/list\";") && line.contains(sort_binding)
		});
		assert!(
			list_import.is_some(),
			"{consumer} must import exact {sort_binding} from its canonical owner:\n{}",
			sources[consumer]
		);
	}
	assert!(
		list.contains("from \"std/collections/list\""),
		"canonical Nymph owner must link its host-only leaf explicitly:\n{list}"
	);
	let out = run_project(
		FxHashMap::from_iter([
			(
				"main",
				"import @/left with (left_values)\nimport @/right with (right_values)\nfunc main(): void = {}\nfunc result(): int = left_values()[0] + right_values()[0]",
			),
			("left", "public func left_values(): #[int] = #[2, 1].sort()"),
			(
				"right",
				"public func right_values(): #[int] = #[4, 3].sort()",
			),
		]),
		"result",
		"",
	);
	assert_eq!(out, "4");
}

#[test]
fn native_generic_dispatch_members_are_declared_once_by_exact_identity() {
	let files = FxHashMap::from_iter([(
		"main",
		"func add<T: Plus<Other = T, Output = T>>(left: T, right: T): T = if (1.plus(1) == 2) left + right else left\n\
		 func result(): int = add(20, 22)\nfunc main(): void = {}",
	)]);
	let sources = compile_project_module_sources_with_std("main", &loader(files.clone()), &|_| None)
		.unwrap_or_else(|diags| panic!("native generic dispatch should assemble: {diags:?}"));
	let plus_declarations = sources
		.iter()
		.flat_map(|(module, source)| {
			source
				.lines()
				.filter(|line| {
					line.starts_with("let ")
						&& line.contains(" = nymphCallable(function(")
						&& line
							.split_once(" =")
							.is_some_and(|(binding, _)| binding.ends_with("$plus"))
				})
				.map(move |line| (module.as_str(), line))
		})
		.collect::<Vec<_>>();
	let unique = plus_declarations
		.iter()
		.map(|(_, declaration)| *declaration)
		.collect::<std::collections::HashSet<_>>();
	assert!(!plus_declarations.is_empty(), "{sources:#?}");
	assert_eq!(
		plus_declarations.len(),
		unique.len(),
		"{plus_declarations:#?}"
	);
	let main = &sources["main"];
	let plus_imports = main
		.lines()
		.filter(|line| line.starts_with("import ") && line.contains("$plus"))
		.flat_map(|line| {
			line
				.split_once('{')
				.unwrap()
				.1
				.split_once('}')
				.unwrap()
				.0
				.split(',')
				.map(str::trim)
				.filter(|name| name.contains("$plus"))
		})
		.collect::<Vec<_>>();
	let unique_imports = plus_imports
		.iter()
		.copied()
		.collect::<std::collections::HashSet<_>>();
	assert!(!plus_imports.is_empty(), "{main}");
	assert_eq!(plus_imports.len(), unique_imports.len(), "{main}");
	assert_eq!(run_project(files, "result", ""), "42");
}

#[test]
fn cross_module_enum_match_runs_under_node() {
	// Matching an enum imported from another module must not
	// crash at RUNTIME with `ReferenceError: TAG is not defined` — the shared
	// `TAG` discriminant const is emitted only by the enum's DECLARING module,
	// but across rolldown's per-module ES scopes the MATCHING module referenced
	// it without a binding. `node --check` can't catch this (it's a runtime
	// error), so this actually executes the bundle. `main` recurses forever if
	// the match produced the wrong value, so a clean exit 0 proves both that
	// `TAG` is bound AND the cross-module match resolved correctly.
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/shapes with (Color, red)\n\
			 func label(c: Color): int = match (c) { Red -> 1, Green -> 2 }\n\
			 func spin(): void = spin()\n\
			 func main(): void = { if (label(red()) != 1) spin() }",
		),
		(
			"shapes",
			"public enum Color { Red, Green }\npublic func red(): Color = Color.Red",
		),
	]);
	let compiled = compile_project("main", &loader(files))
		.unwrap_or_else(|d| panic!("expected a clean compile, got: {d:?}"));
	let js = format!("{}\n{}();\n", compiled.js, compiled.entry_main);

	let path =
		std::env::temp_dir().join(format!("nymph_project_enum_run_{}.mjs", std::process::id()));
	std::fs::write(&path, &js).unwrap();
	let out = std::process::Command::new("node")
		.arg(&path)
		.output()
		.expect("spawn node");
	let _ = std::fs::remove_file(&path);
	assert!(
		out.status.success(),
		"cross-module enum match crashed under Node:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}

#[test]
fn cross_module_enum_views_check_deterministically() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/view with (View, widen)\nimport @/source with (Source, make)\nfunc direct(value: Source): View = value\nfunc use(): View = direct(make())\nfunc main(): void = { let value = widen(make()) }",
		),
		(
			"source",
			"public enum Source { A, B }\npublic func make(): Source = Source.A",
		),
		(
			"view",
			"import @/source with (Source)\npublic enum View { ...Source, C }\npublic func widen(value: Source): View = value",
		),
	]);
	let first = check_project("main", &loader(files.clone()));
	let second = check_project("main", &loader(files));
	assert!(
		first.is_empty(),
		"expected a clean fixed point, got: {first:?}"
	);
	assert_eq!(
		first, second,
		"fixed-point diagnostics must be deterministic"
	);
}

#[test]
fn unrelated_module_cannot_attach_a_static_to_an_imported_type() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/owner with (Token)\nimport @/extension\nfunc main(): void = {}",
		),
		("owner", "public struct Token(value: int)"),
		(
			"extension",
			"import @/owner with (Token)\nimpl Token { namespace func forge(): Token = Token(value = 0) }",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		!diags.is_empty(),
		"an unrelated module must not extend another module's type"
	);
}

#[test]
fn same_bare_type_name_in_different_modules_does_not_receive_another_owners_static() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/left with (left_value)\nimport @/right with (right_value)\nfunc main(): void = {}\nfunc result(): int = left_value() + right_value()",
		),
		(
			"left",
			"public struct Token(value: int)\nimpl Token { namespace func left(): Token = Token(value = 1) }\npublic func left_value(): int = Token.left().value",
		),
		(
			"right",
			"public struct Token(value: int)\npublic func right_value(): int = Token(value = 2).value",
		),
	]);
	assert_eq!(run_project(files, "result", ""), "3");
}

#[test]
fn duplicate_static_attachments_across_source_modules_are_diagnosed() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/owner with (Token)\nimport @/first\nimport @/second\nfunc main(): void = {}",
		),
		("owner", "public struct Token(value: int)"),
		(
			"first",
			"import @/owner with (Token)\nimpl Token { namespace func make(): Token = Token(value = 1) }",
		),
		(
			"second",
			"import @/owner with (Token)\nimpl Token { namespace func make(): Token = Token(value = 2) }",
		),
	]);
	let diags = check_project("main", &loader(files));
	assert!(
		diags
			.iter()
			.any(|diag| diag.diag.code == "INHERENT-IMPL-OWNER"),
		"malformed cross-module attachments must report exact owner errors: {diags:?}"
	);
}

#[test]
fn multiple_consumers_share_one_canonical_generic_enum_static_attachment() {
	let files = FxHashMap::from_iter([
		(
			"main",
			"import @/left with (left_value)\nimport @/right with (right_value)\nfunc main(): void = {}\nfunc result(): int = left_value() + right_value()",
		),
		(
			"owner",
			"public enum Boxed<T> { Value(value: T) }\nimpl<T> Boxed<T> { namespace func wrap(value: T): Boxed<T> = Boxed.Value(value = value) }",
		),
		(
			"left",
			"import @/owner with (Boxed)\npublic func left_value(): int = match (Boxed.wrap(2)) { Value(value) -> value }",
		),
		(
			"right",
			"import @/owner with (Boxed)\npublic func right_value(): int = match (Boxed.wrap(3)) { Value(value) -> value }",
		),
	]);
	let sources = compile_project_module_sources_with_std("main", &loader(files.clone()), &|_| None)
		.unwrap_or_else(|diags| panic!("canonical static graph should compile: {diags:?}"));
	assert!(sources.contains_key("owner"));
	assert_eq!(
		sources.keys().filter(|key| key.as_str() == "owner").count(),
		1
	);
	assert_eq!(run_project(files, "result", ""), "5");
}
