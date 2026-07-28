use nymph_sema::{
	BuiltinDispatch, DeclarationKey, EntryMode, ModuleIdentity, ModuleOrigin, RuntimeExtractionError,
	RuntimePayload, SemanticEnvironment, StableDispatch, check_module_with_environment,
	declared_headers, extract_module_interface, runtime_definitions,
};

fn source_name(definition: &nymph_sema::DefinitionId) -> &str {
	match &definition.key {
		DeclarationKey::TopLevel { name, .. }
		| DeclarationKey::Member { name, .. }
		| DeclarationKey::MethodBody { name, .. } => name,
		DeclarationKey::MaterializedInterfaceMember {
			interface_member, ..
		} => source_name(interface_member),
		DeclarationKey::Implementation { .. } | DeclarationKey::RecoveredImplementation { .. } => {
			"implementation"
		}
	}
}

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
	runtime_definitions(&module, &checked.facts, &interface).unwrap()
}

fn collect_pattern_ids(
	pattern: &nymph_sema::StablePattern,
	ids: &mut Vec<nymph_sema::PatternNodeId>,
) {
	use nymph_sema::{
		StableListPatternEntry, StableMapPatternEntry, StablePatternKind, StablePatternRange,
		StableStructPatternField,
	};
	ids.push(pattern.id);
	match &pattern.kind {
		StablePatternKind::Binding { inner, .. } | StablePatternKind::Grouped(inner) => {
			collect_pattern_ids(inner, ids)
		}
		StablePatternKind::List(entries) | StablePatternKind::Tuple(entries) => {
			for entry in entries.iter() {
				if let StableListPatternEntry::Item(pattern) = entry {
					collect_pattern_ids(pattern, ids);
				}
			}
		}
		StablePatternKind::Map(entries) => {
			for entry in entries.iter() {
				if let StableMapPatternEntry::Entry(key, value) = entry {
					collect_pattern_ids(key, ids);
					collect_pattern_ids(value, ids);
				}
			}
		}
		StablePatternKind::Range(range) => match range {
			StablePatternRange::From(pattern)
			| StablePatternRange::To(pattern)
			| StablePatternRange::ToInclusive(pattern) => collect_pattern_ids(pattern, ids),
			StablePatternRange::Exclusive { min, max } | StablePatternRange::Inclusive { min, max } => {
				collect_pattern_ids(min, ids);
				collect_pattern_ids(max, ids);
			}
		},
		StablePatternKind::Struct { fields, .. } => {
			for field in fields.iter() {
				match field {
					StableStructPatternField::Value { value, .. } => collect_pattern_ids(value, ids),
					StableStructPatternField::Named { id, .. } => ids.push(*id),
					StableStructPatternField::Positional { id, pattern } => {
						ids.push(*id);
						collect_pattern_ids(pattern, ids);
					}
					StableStructPatternField::Rest => {}
				}
			}
		}
		StablePatternKind::Union(left, right) => {
			collect_pattern_ids(left, ids);
			collect_pattern_ids(right, ids);
		}
		StablePatternKind::Int(_)
		| StablePatternKind::UInt(_)
		| StablePatternKind::Float(_)
		| StablePatternKind::Char(_)
		| StablePatternKind::String(_)
		| StablePatternKind::Boolean(_)
		| StablePatternKind::Placeholder => {}
	}
}

fn collect_owned_ids(
	expr: &nymph_sema::StableExpr,
	body_ids: &mut Vec<nymph_sema::BodyNodeId>,
	pattern_ids: &mut Vec<nymph_sema::PatternNodeId>,
) {
	use nymph_sema::{
		StableExprKind, StableListItem, StableMapEntry, StableRange, StableStatement, StableStringPart,
	};
	body_ids.push(expr.id);
	macro_rules! visit {
		($child:expr) => {
			collect_owned_ids($child, body_ids, pattern_ids)
		};
	}
	match &expr.kind {
		StableExprKind::String(parts) => {
			for part in parts.iter() {
				if let StableStringPart::Expr(child) = part {
					visit!(child);
				}
			}
		}
		StableExprKind::List(items) | StableExprKind::Tuple(items) => {
			for item in items.iter() {
				match item {
					StableListItem::Expr(child) | StableListItem::Spread(child) => visit!(child),
				}
			}
		}
		StableExprKind::Map(entries) => {
			for entry in entries.iter() {
				match entry {
					StableMapEntry::Entry(key, value) => {
						visit!(key);
						visit!(value);
					}
					StableMapEntry::Spread(child) => visit!(child),
				}
			}
		}
		StableExprKind::Range(range) => match range {
			StableRange::From(child) | StableRange::To(child) | StableRange::ToInclusive(child) => {
				visit!(child)
			}
			StableRange::Exclusive { min, max } | StableRange::Inclusive { min, max } => {
				visit!(min);
				visit!(max);
			}
		},
		StableExprKind::Call { func, args } => {
			visit!(func);
			for argument in args.iter() {
				visit!(&argument.value);
			}
		}
		StableExprKind::MemberAccess { parent, .. } => visit!(parent),
		StableExprKind::IndexAccess { parent, index, .. } => {
			visit!(parent);
			visit!(index);
		}
		StableExprKind::Closure { params, body } => {
			for parameter in params.iter() {
				collect_pattern_ids(&parameter.pattern, pattern_ids);
			}
			visit!(body);
		}
		StableExprKind::PrefixOp { value, .. }
		| StableExprKind::PostfixOp { value, .. }
		| StableExprKind::Grouped(value) => visit!(value),
		StableExprKind::BinaryOp { lhs, rhs, .. } | StableExprKind::AssignOp { lhs, rhs, .. } => {
			visit!(lhs);
			visit!(rhs);
		}
		StableExprKind::TypeOp { lhs, .. } => visit!(lhs),
		StableExprKind::PatternOp { lhs, rhs, .. } => {
			visit!(lhs);
			collect_pattern_ids(rhs, pattern_ids);
		}
		StableExprKind::Return { value, .. } | StableExprKind::Break { value, .. } => {
			if let Some(value) = value {
				visit!(value);
			}
		}
		StableExprKind::While {
			condition, body, ..
		} => {
			visit!(condition);
			visit!(body);
		}
		StableExprKind::For {
			variable,
			iterable,
			body,
			..
		} => {
			collect_pattern_ids(variable, pattern_ids);
			visit!(iterable);
			visit!(body);
		}
		StableExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			visit!(condition);
			visit!(then);
			if let Some(otherwise) = otherwise {
				visit!(otherwise);
			}
		}
		StableExprKind::Match { value, arms } => {
			visit!(value);
			for arm in arms.iter() {
				collect_pattern_ids(&arm.pattern, pattern_ids);
				if let Some(guard) = &arm.guard {
					visit!(guard);
				}
				visit!(&arm.body);
			}
		}
		StableExprKind::Block { body, .. } => {
			for statement in body.iter() {
				match statement {
					StableStatement::Expr(child) => visit!(child),
					StableStatement::Let { pattern, value, .. } => {
						collect_pattern_ids(pattern, pattern_ids);
						visit!(value);
					}
				}
			}
		}
		StableExprKind::Int(_)
		| StableExprKind::UInt(_)
		| StableExprKind::Float(_)
		| StableExprKind::Char(_)
		| StableExprKind::Boolean(_)
		| StableExprKind::Identifier(_)
		| StableExprKind::AnonymousParam(_)
		| StableExprKind::Continue { .. }
		| StableExprKind::This => {}
	}
}

#[test]
fn body_projection_is_structural_and_records_exact_dispatch_variant_pattern_marshal_and_generic_type()
 {
	let body = r#"
external(max_float) let host: float
interface Value {
	func value(): int = 7
	func plus(other: self): self = other
}
struct Box(value: int) { func own(): int = this.value }
impl Value for Box { }
enum Choice { Some(value: int), None }
func generic<T: Value>(item: T): T = { let x = 1 + 2
	let made = Choice.Some(x)
	let read = match (made) { Choice.Some(1) -> 1, Choice.Some(_) -> 2, Choice.None -> 0 }
	let direct = Box(value = read).own()
	let defaulted = Box(value = direct).value()
	let bounded = item.value()
	let sum = item + item
	let host_value = host
	sum
}
"#;
	let first = project(body);
	let expected_positional_field = first
		.iter()
		.find_map(|artifact| match &artifact.payload {
			RuntimePayload::Enum(shell) if source_name(&artifact.definition) == "Choice" => shell
				.variants
				.iter()
				.find(|variant| variant.name == "Some")
				.and_then(|variant| variant.fields.first())
				.map(|field| field.id.clone()),
			_ => None,
		})
		.expect("Choice.Some.value exact field ID");
	let materialized = first
		.iter()
		.find(|artifact| {
			matches!(
				artifact.payload,
				RuntimePayload::MaterializedInterfaceMember { .. }
			)
		})
		.expect("default implementation publishes a materialized artifact");
	let RuntimePayload::MaterializedInterfaceMember {
		body_definition,
		interface_member,
	} = &materialized.payload
	else {
		unreachable!()
	};
	assert_eq!(body_definition, interface_member);
	assert_eq!(materialized.source_owner, body_definition.module);
	assert!(matches!(
		materialized.definition.key,
		nymph_sema::DeclarationKey::MaterializedInterfaceMember { .. }
	));
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
			.dispatches
			.iter()
			.any(|(_, dispatch)| matches!(
				dispatch,
				StableDispatch::InterfaceDefault { member, .. }
					if matches!(member.key, nymph_sema::DeclarationKey::MaterializedInterfaceMember { .. })
			))
	);
	assert!(
		body.annotations.dispatches.iter().all(|(_, dispatch)| {
			let StableDispatch::InterfaceDefault { implementation, .. } = dispatch else {
				return true;
			};
			matches!(
				implementation.key,
				nymph_sema::DeclarationKey::Implementation { .. }
			)
		}),
		"a stable default dispatch must retain the exact selected implementation ID"
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
	assert!(
		body
			.annotations
			.positional_fields
			.iter()
			.any(|(_, field)| { field.name == "value" && field.definition == expected_positional_field })
	);
	assert!(!body.annotations.external_marshals.is_empty());
}

#[test]
fn body_projection_preserves_generic_bound_dispatch_inside_a_closure() {
	let artifacts = project(
		"interface Comparable<Other> { func compare_to(other: Other): int }\n\
		 impl<T> #[T] { func sort_by(compare: (T, T) -> int): int = 0 }\n\
		 impl<T: Comparable<Other = T>> #[T] {\n\
		   func comparator(): int = this.sort_by((left, right) -> left.compare_to(right))\n\
		 }",
	);
	let comparator = artifacts
		.iter()
		.find(|artifact| source_name(&artifact.definition) == "comparator")
		.expect("comparator runtime body");
	let RuntimePayload::NymphBody(body) = &comparator.payload else {
		panic!("comparator must retain its Nymph body")
	};
	assert!(
		body
			.annotations
			.dispatches
			.iter()
			.any(|(_, dispatch)| matches!(dispatch, StableDispatch::GenericBound { .. })),
		"closure call must retain its exact generic-bound dispatch: {:?}",
		body.annotations.dispatches
	);
}

#[test]
fn runtime_body_identity_ignores_whitespace_and_comments() {
	let compact = project(
		"func stable(value: int): int = {\n\
		 let incremented = value + 1\n\
		 incremented\n\
		 }",
	);
	let formatted = project(
		"func stable( value: int ): int = {\n\
		 // This comment is not part of runtime semantics.\n\
		 let incremented = value + 1\n\n\
		 incremented\n\
		 }",
	);
	let body = |artifacts: Vec<nymph_sema::RuntimeDefinition>| {
		artifacts
			.into_iter()
			.find_map(|artifact| match artifact.payload {
				RuntimePayload::NymphBody(body) if source_name(&artifact.definition) == "stable" => {
					Some(body)
				}
				_ => None,
			})
			.expect("stable runtime body")
	};

	assert_eq!(body(compact), body(formatted));
}

#[test]
fn runtime_body_identity_changes_with_semantic_structure() {
	let one = project("func stable(value: int): int = value + 1");
	let two = project("func stable(value: int): int = value + 2");
	let body = |artifacts: Vec<nymph_sema::RuntimeDefinition>| {
		artifacts
			.into_iter()
			.find_map(|artifact| match artifact.payload {
				RuntimePayload::NymphBody(body) if source_name(&artifact.definition) == "stable" => {
					Some(body)
				}
				_ => None,
			})
			.expect("stable runtime body")
	};

	assert_ne!(body(one), body(two));
}

#[test]
fn owned_pattern_nodes_have_unique_structural_ids() {
	let artifacts = project(
		"enum Choice { Some(value: int), None }\nfunc ids(parameter: int): int = { let local = Choice.Some(parameter)\n match (local) { Choice.Some(value) -> value, Choice.None -> 0 } }",
	);
	let body = artifacts
		.into_iter()
		.find_map(|artifact| match artifact.payload {
			RuntimePayload::NymphBody(body) if source_name(&artifact.definition) == "ids" => Some(body),
			_ => None,
		})
		.expect("ids runtime body");
	let mut pattern_ids = Vec::new();
	for parameter in body.stable.params.iter() {
		collect_pattern_ids(&parameter.pattern, &mut pattern_ids);
	}
	let mut body_ids = Vec::new();
	collect_owned_ids(&body.stable.root, &mut body_ids, &mut pattern_ids);
	let unique_body = body_ids
		.iter()
		.copied()
		.collect::<std::collections::HashSet<_>>();
	let unique_patterns = pattern_ids
		.iter()
		.copied()
		.collect::<std::collections::HashSet<_>>();
	assert_eq!(
		unique_body.len(),
		body_ids.len(),
		"every owned expression needs its own ID"
	);
	assert_eq!(
		unique_patterns.len(),
		pattern_ids.len(),
		"every owned pattern site needs its own ID: {pattern_ids:?}"
	);
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
		runtime_definitions(&module, &checked.facts, &interface),
		Err(RuntimeExtractionError::MissingStableId(
			"MissingShape".into()
		)),
	);
}

#[test]
fn implementation_projection_uses_checker_identity_not_export_shape_order() {
	let source = r#"
interface Show { func show(): int }
struct Left
struct Right
impl Show for Left { func show(): int = 1 }
impl Show for Right { func show(): int = 2 }
"#;
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
	let interface = extract_module_interface(identity, &module, &checked, &headers).unwrap();
	let expected = runtime_definitions(&module, &checked.facts, &interface).unwrap();
	let mut reordered = interface.clone();
	reordered.implementations.reverse();
	let actual = runtime_definitions(&module, &checked.facts, &reordered).unwrap();
	assert_eq!(actual, expected);
}

#[test]
fn missing_authoritative_implementation_identity_is_a_typed_error() {
	let source = "struct Box\nimpl Box { func answer(): int = 42 }";
	let parsed = nymph_syntax::parse_module(source, "fixture.nym");
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
	let mut facts = result.analysis.checked.as_ref().clone();
	let checked = nymph_sema::Checked {
		diags: vec![],
		facts: facts.clone(),
	};
	let headers = declared_headers(identity.clone(), &module);
	let interface = extract_module_interface(identity, &module, &checked, &headers).unwrap();
	facts.source_identities.implementations.clear();
	assert_eq!(
		runtime_definitions(&module, &facts, &interface),
		Err(RuntimeExtractionError::MissingSourceIdentity),
	);
}

#[test]
fn standalone_inherent_members_have_exact_authoritative_identities() {
	let source = r#"
struct Box
impl Box {
	func answer(): int = 42
	let value: int = 7
	external(host_value) let host: int
}
"#;
	let artifacts = project(source);
	assert_eq!(
		artifacts
			.iter()
			.filter(|artifact| matches!(
				artifact.placement,
				nymph_sema::RuntimePlacement::Attached { .. }
			))
			.count(),
		3,
	);
	let ids = artifacts
		.iter()
		.map(|artifact| artifact.definition.clone())
		.collect::<std::collections::HashSet<_>>();
	assert_eq!(ids.len(), artifacts.len());
}

#[test]
fn inherent_static_call_annotations_preserve_the_exact_member_identity() {
	let source = "struct Box(value: int)\nimpl Box { namespace func make(): Box = Box(value = 1) }\nfunc read(): Box = Box.make()";
	let artifacts = project(source);
	let read = artifacts
		.iter()
		.find(|artifact| source_name(&artifact.definition) == "read")
		.expect("read runtime definition");
	let make = artifacts
		.iter()
		.find(|artifact| source_name(&artifact.definition) == "make")
		.expect("make runtime definition");
	let nymph_sema::RuntimePayload::NymphBody(body) = &read.payload else {
		panic!("read must have a Nymph body")
	};
	assert_eq!(
		body
			.annotations
			.definition_targets
			.iter()
			.filter(|(_, target)| target == &make.definition)
			.count(),
		2,
		"the call and its static member receiver must retain the selected member ID"
	);
	assert!(matches!(make.definition.key, DeclarationKey::Member { .. }));
}
