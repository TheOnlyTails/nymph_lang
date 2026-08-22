use std::{collections::HashMap, sync::Arc, thread};

use ecow::EcoString;
use nymph_ast::{decl::Declaration, expr::Pattern};
use nymph_sema::{
	DeclarationCategory, DeclarationKey, DefinitionId, HeaderBinder, HeaderConstraint,
	HeaderParameterId, HeaderType, ImplementationHeader, ModuleIdentity, StableIdBuilder,
};
use nymph_syntax::parse_module;

fn module(path: &str) -> ModuleIdentity {
	ModuleIdentity {
		origin: nymph_sema::ModuleOrigin::Project("app".into()),
		project: EcoString::from("app"),
		path: EcoString::from(path),
	}
}

#[test]
fn exact_resolved_package_instance_is_part_of_stable_identity() {
	let key = DeclarationKey::top_level(DeclarationCategory::Struct, "Value");
	let package = |node| ModuleIdentity::resolved_project("workspace", node, "types");

	let first = DefinitionId::new(package(1), key.clone());
	let alias_of_first = DefinitionId::new(package(1), key.clone());
	let independent_copy = DefinitionId::new(package(2), key);

	assert_eq!(first, alias_of_first);
	assert_ne!(first, independent_copy);
	assert!(first.module.same_package_as(&alias_of_first.module));
	assert!(!first.module.same_package_as(&independent_copy.module));
}

#[test]
fn ordinary_identity_uses_only_owned_source_header_identity() {
	let key = DeclarationKey::top_level(DeclarationCategory::Function, "answer");
	let a = DefinitionId::new(module("main"), key.clone());
	let b = DefinitionId::new(module("main"), key);
	assert_eq!(
		a, b,
		"body, whitespace, spans, traversal, and unrelated declarations have no input"
	);
	assert_ne!(
		a,
		DefinitionId::new(
			module("other"),
			DeclarationKey::top_level(DeclarationCategory::Function, "answer")
		)
	);
	assert_ne!(
		a,
		DefinitionId::new(
			module("main"),
			DeclarationKey::top_level(DeclarationCategory::Function, "other")
		)
	);
	assert_ne!(
		a,
		DefinitionId::new(
			module("main"),
			DeclarationKey::top_level(DeclarationCategory::Let, "answer")
		)
	);
}

#[test]
fn member_identity_is_owner_category_and_source_name() {
	let owner = DefinitionId::new(
		module("main"),
		DeclarationKey::top_level(DeclarationCategory::Struct, "Box"),
	);
	let field = DefinitionId::new(
		module("main"),
		DeclarationKey::member(owner.clone(), DeclarationCategory::Field, "value"),
	);
	assert_ne!(
		field,
		DefinitionId::new(
			module("main"),
			DeclarationKey::member(owner.clone(), DeclarationCategory::Method, "value")
		)
	);
	assert_ne!(
		field,
		DefinitionId::new(
			module("main"),
			DeclarationKey::member(owner, DeclarationCategory::Field, "other")
		)
	);
}

#[test]
fn duplicate_counters_are_scoped_to_identical_keys() {
	let mut ids = StableIdBuilder::new(module("main"));
	let f = DeclarationKey::top_level(DeclarationCategory::Function, "f");
	let g = DeclarationKey::top_level(DeclarationCategory::Function, "g");
	assert_eq!(ids.allocate(f.clone()).key.duplicate(), 0);
	assert_eq!(ids.allocate(f.clone()).key.duplicate(), 1);
	assert_eq!(ids.allocate(g).key.duplicate(), 0);
	assert_eq!(ids.allocate(f).key.duplicate(), 2);
}

#[test]
fn parallel_construction_is_deterministic() {
	let expected = Arc::new(DefinitionId::new(
		module("lib"),
		DeclarationKey::top_level(DeclarationCategory::Enum, "Result"),
	));
	let handles: Vec<_> = (0..16)
		.map(|_| {
			let expected = Arc::clone(&expected);
			thread::spawn(move || {
				assert_eq!(
					*expected,
					DefinitionId::new(
						module("lib"),
						DeclarationKey::top_level(DeclarationCategory::Enum, "Result")
					)
				)
			})
		})
		.collect();
	for handle in handles {
		handle.join().unwrap();
	}
}

#[test]
fn permutation_does_not_change_ids() {
	let keys = [
		DeclarationKey::top_level(DeclarationCategory::Struct, "A"),
		DeclarationKey::top_level(DeclarationCategory::Enum, "B"),
	];
	let make = |order: &[usize]| {
		order
			.iter()
			.map(|&i| {
				let key = keys[i].clone();
				(key.clone(), DefinitionId::new(module("m"), key))
			})
			.collect::<HashMap<_, _>>()
	};
	assert_eq!(make(&[0, 1]), make(&[1, 0]));
}

#[test]
fn implementation_identity_changes_only_with_its_observable_header() {
	let header = |self_type| ImplementationHeader {
		interface: Some(DefinitionId::new(
			module("traits"),
			DeclarationKey::top_level(DeclarationCategory::Interface, "Display"),
		)),
		interface_arguments: vec![],
		self_type,
		binders: vec![],
		constraints: vec![],
	};
	let first = DefinitionId::new(
		module("main"),
		DeclarationKey::implementation(header(HeaderType::Int)),
	);
	let same = DefinitionId::new(
		module("main"),
		DeclarationKey::implementation(header(HeaderType::Int)),
	);
	let changed = DefinitionId::new(
		module("main"),
		DeclarationKey::implementation(header(HeaderType::String)),
	);
	assert_eq!(first, same);
	assert_ne!(first, changed);
}

#[test]
fn generic_implementation_headers_are_alpha_normalized_and_order_insensitive() {
	let iface = definition(DeclarationCategory::Interface, "Convert");
	let bound_a = definition(DeclarationCategory::Interface, "Clone");
	let bound_b = definition(DeclarationCategory::Interface, "Display");
	let make = |first: u32, second: u32, reverse: bool| {
		let a = HeaderParameterId(first);
		let b = HeaderParameterId(second);
		let mut constraints = vec![
			HeaderConstraint {
				parameter: a,
				interface: bound_a.clone(),
				positional: vec![HeaderType::List(Box::new(HeaderType::Generic(b)))],
				named: vec![],
			},
			HeaderConstraint {
				parameter: b,
				interface: bound_b.clone(),
				positional: vec![],
				named: vec![("Output".into(), HeaderType::Generic(a))],
			},
		];
		if reverse {
			constraints.reverse();
		}
		ImplementationHeader {
			interface: Some(iface.clone()),
			interface_arguments: vec![
				("Z".into(), HeaderType::Generic(b)),
				("A".into(), HeaderType::Generic(a)),
			],
			self_type: HeaderType::Named {
				definition: definition(DeclarationCategory::Struct, "Pair"),
				positional: vec![HeaderType::Generic(a), HeaderType::Generic(b)],
				named: vec![],
			},
			binders: vec![HeaderBinder { parameter: a }, HeaderBinder { parameter: b }],
			constraints,
		}
	};
	let id = |header| DefinitionId::new(module("main"), DeclarationKey::implementation(header));
	assert_eq!(id(make(41, 99, false)), id(make(7, 8, true)));

	let first = id(make(1, 2, false));
	let mut changed_header = make(1, 2, false);
	changed_header.constraints[0].interface = bound_b;
	assert_ne!(first, id(changed_header));
}

#[test]
fn implementation_header_intersections_match_interner_normalization_everywhere() {
	let a = HeaderParameterId(20);
	let canonical = ImplementationHeader {
		interface: None,
		interface_arguments: vec![("Named".into(), HeaderType::Generic(a))],
		self_type: HeaderType::Intersection(vec![HeaderType::Int, HeaderType::String]),
		binders: vec![HeaderBinder { parameter: a }],
		constraints: vec![HeaderConstraint {
			parameter: a,
			interface: definition(DeclarationCategory::Interface, "Bound"),
			positional: vec![HeaderType::Void],
			named: vec![("Empty".into(), HeaderType::Void)],
		}],
	};
	let equivalent = ImplementationHeader {
		interface: None,
		interface_arguments: vec![(
			"Named".into(),
			HeaderType::Intersection(vec![HeaderType::Generic(HeaderParameterId(7))]),
		)],
		self_type: HeaderType::Intersection(vec![
			HeaderType::String,
			HeaderType::Intersection(vec![HeaderType::Int, HeaderType::String]),
		]),
		binders: vec![HeaderBinder {
			parameter: HeaderParameterId(7),
		}],
		constraints: vec![HeaderConstraint {
			parameter: HeaderParameterId(7),
			interface: definition(DeclarationCategory::Interface, "Bound"),
			positional: vec![HeaderType::Intersection(vec![])],
			named: vec![("Empty".into(), HeaderType::Intersection(vec![]))],
		}],
	};
	assert_eq!(
		DeclarationKey::implementation(canonical),
		DeclarationKey::implementation(equivalent)
	);
}

#[test]
fn intersections_normalize_in_nested_named_positional_and_named_arguments() {
	let named = |positional, args| HeaderType::Named {
		definition: definition(DeclarationCategory::Struct, "Container"),
		positional,
		named: args,
	};
	let header = |self_type| ImplementationHeader {
		interface: None,
		interface_arguments: vec![],
		self_type,
		binders: vec![],
		constraints: vec![],
	};
	assert_eq!(
		DeclarationKey::implementation(header(named(
			vec![HeaderType::Int],
			vec![
				("A".into(), HeaderType::String),
				("B".into(), HeaderType::Void)
			]
		))),
		DeclarationKey::implementation(header(named(
			vec![HeaderType::Intersection(vec![
				HeaderType::Int,
				HeaderType::Int
			])],
			vec![
				("B".into(), HeaderType::Intersection(vec![])),
				(
					"A".into(),
					HeaderType::Intersection(vec![HeaderType::String])
				),
			]
		)))
	);
}

#[test]
#[should_panic(expected = "implementation header contains duplicate binder parameter ID")]
fn duplicate_header_parameter_ids_are_an_invariant_violation() {
	let duplicate = HeaderParameterId(9);
	DeclarationKey::implementation(ImplementationHeader {
		interface: None,
		interface_arguments: vec![],
		self_type: HeaderType::Void,
		binders: vec![
			HeaderBinder {
				parameter: duplicate,
			},
			HeaderBinder {
				parameter: duplicate,
			},
		],
		constraints: vec![],
	});
}

#[test]
fn distinct_parameter_ids_remain_valid_even_when_source_names_could_duplicate() {
	let header = ImplementationHeader {
		interface: None,
		interface_arguments: vec![],
		self_type: HeaderType::Tuple(vec![
			HeaderType::Generic(HeaderParameterId(1)),
			HeaderType::Generic(HeaderParameterId(2)),
		]),
		binders: vec![
			HeaderBinder {
				parameter: HeaderParameterId(1),
			},
			HeaderBinder {
				parameter: HeaderParameterId(2),
			},
		],
		constraints: vec![],
	};
	assert_eq!(header.canonical().binders.len(), 2);
}

fn parsed_top_level_ids(
	source: &str,
	path: &str,
) -> HashMap<(DeclarationCategory, EcoString), DefinitionId> {
	let parsed = parse_module(source, path);
	assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
	let mut builder = StableIdBuilder::new(module(path));
	parsed
		.tree
		.members
		.iter()
		.filter_map(|declaration| {
			let (category, name) = match declaration {
				Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
					(DeclarationCategory::Function, meta.name.0.clone())
				}
				Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
					let Pattern::Binding { name, .. } = &meta.name.0 else {
						return None;
					};
					(DeclarationCategory::Let, name.0.clone())
				}
				Declaration::Struct { name, .. } => (DeclarationCategory::Struct, name.0.clone()),
				Declaration::Enum { name, .. } => (DeclarationCategory::Enum, name.0.clone()),
				Declaration::Interface { name, .. } => (DeclarationCategory::Interface, name.0.clone()),
				Declaration::Namespace { name, .. } => (DeclarationCategory::Namespace, name.0.clone()),
				Declaration::TypeAlias { meta, .. } => {
					(DeclarationCategory::TypeAlias, meta.name.0.clone())
				}
				_ => return None,
			};
			let id = builder.allocate(DeclarationKey::top_level(category, name.clone()));
			Some(((category, name), id))
		})
		.collect()
}

#[test]
fn parser_backed_ids_ignore_bodies_whitespace_spans_and_unrelated_declarations() {
	let before = parsed_top_level_ids("func answer(): int = 1\nstruct Box(value: int)", "main");
	let after = parsed_top_level_ids(
		"\n let unrelated = 0\n\nfunc answer(): int = 40 + 2\n\nstruct Box(value: int)\n",
		"main",
	);
	for key in before.keys() {
		assert_eq!(before[key], after[key]);
	}
}

#[test]
fn reversed_module_construction_and_independent_parallel_builders_are_deterministic() {
	let sources = [("main", "func main() = 1"), ("dep", "struct Value(n: int)")];
	let build = |order: [usize; 2]| order.map(|i| parsed_top_level_ids(sources[i].1, sources[i].0));
	let forward = build([0, 1]);
	let reverse = build([1, 0]);
	assert_eq!(forward[0], reverse[1]);
	assert_eq!(forward[1], reverse[0]);
	let handles: Vec<_> = (0..8)
		.map(|_| thread::spawn(|| parsed_top_level_ids("func main() = 1", "main")))
		.collect();
	for handle in handles {
		assert_eq!(forward[0], handle.join().unwrap());
	}
}

fn definition(category: DeclarationCategory, name: &str) -> DefinitionId {
	DefinitionId::new(module("types"), DeclarationKey::top_level(category, name))
}
