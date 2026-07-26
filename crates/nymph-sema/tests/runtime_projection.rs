use nymph_sema::{
	BuiltinDispatch, EntryMode, InterfaceType, ModuleIdentity, ModuleOrigin, RuntimeExtractionError,
	RuntimePayload, SemanticEnvironment, StableDispatch, check_module_with_environment,
	declared_headers, extract_module_interface, runtime_definitions,
};

fn project(source: &str) -> Vec<nymph_sema::RuntimeDefinition> {
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = std::sync::Arc::new(parsed.tree);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("annotations".into()),
		project: "annotations".into(),
		path: "main".into(),
	};
	let environment = SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
	let result = check_module_with_environment(
		module.clone(),
		identity.clone(),
		&environment,
		EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let checked = nymph_sema::Checked {
		diags: Vec::new(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(identity.clone(), &module);
	let interface = extract_module_interface(identity, &module, &checked, &headers).unwrap();
	runtime_definitions(&module, source, &checked.facts, &interface).unwrap()
}

#[test]
fn body_projection_is_structural_and_records_exact_dispatch_variant_pattern_marshal_and_generic_type()
 {
	let body = r#"
external(max_float) let host: float
interface Value { func value(): int = 7 }
struct Box(value: int) { func own(): int = this.value }
impl Value for Box { }
enum Choice { Some(value: int), None }
func generic<T: Value>(item: T): T = { let x = 1 + 2
	let made = Choice.Some(x)
	let read = match (made) { Choice.Some(1) -> 1, Choice.Some(_) -> 2, Choice.None -> 0 }
	let direct = Box(value = read).own()
	let defaulted = Box(value = direct).value()
	let bounded = item.value()
	let host_value = host
	item
}
"#;
	let first = project(body);
	let shifted = project(&format!("func unrelated(): int = 99\n{body}"));
	let find = |items: &[nymph_sema::RuntimeDefinition]| {
		items
			.iter()
			.find(|item| format!("{:?}", item.definition).contains("generic"))
			.unwrap()
			.clone()
	};
	let first = find(&first);
	let shifted = find(&shifted);
	assert_eq!(
		first, shifted,
		"parser-global IDs and absolute spans cannot affect an artifact"
	);
	let RuntimePayload::NymphBody(body) = first.payload else {
		panic!("generic body")
	};
	assert!(
		body
			.annotations
			.types
			.iter()
			.any(|(_, ty)| matches!(ty, InterfaceType::Generic(_)))
	);
	assert!(
		body
			.annotations
			.dispatches
			.iter()
			.any(|(_, dispatch)| matches!(
				dispatch,
				StableDispatch::Builtin {
					category: BuiltinDispatch::Eager,
					..
				}
			))
	);
	assert!(
		body
			.annotations
			.dispatches
			.iter()
			.any(|(_, dispatch)| matches!(dispatch, StableDispatch::Direct { .. }))
	);
	assert!(
		body
			.annotations
			.dispatches
			.iter()
			.any(|(_, dispatch)| matches!(dispatch, StableDispatch::GenericBound { .. }))
	);
	assert_eq!(body.annotations.variants.len(), 1);
	assert_eq!(body.annotations.pattern_variants.len(), 3);
	assert!(!body.annotations.positional_fields.is_empty());
	assert!(!body.annotations.external_marshals.is_empty());
}

#[test]
fn inconsistent_checked_interface_reports_typed_missing_stable_identity() {
	let source = "public struct MissingShape(value: int)";
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = std::sync::Arc::new(parsed.tree);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("runtime-projection".into()),
		project: "runtime-projection".into(),
		path: "main".into(),
	};
	let environment = SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
	let result = check_module_with_environment(
		module.clone(),
		identity.clone(),
		&environment,
		EntryMode::Library,
	);
	assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
	let checked = nymph_sema::Checked {
		diags: Vec::new(),
		facts: result.analysis.checked.as_ref().clone(),
	};
	let headers = declared_headers(identity.clone(), &module);
	let mut interface = extract_module_interface(identity, &module, &checked, &headers)
		.expect("consistent fixture extracts");
	interface.exports.clear();
	assert_eq!(
		runtime_definitions(&module, source, &checked.facts, &interface),
		Err(RuntimeExtractionError::MissingStableId(
			"MissingShape".into()
		)),
	);
}
