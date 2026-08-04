#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	AmbientCoreModuleKey, CompilerSession, ModulePath, ProjectId, SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{
	DeclarationCategory, DeclarationKey, DefinitionId, EntryMode, InterfaceType, ModuleIdentity,
	ModuleInterface, ModuleOrigin,
};

fn hir_contains(
	expr: &nymph_hir::hir::HirExpr,
	predicate: &impl Fn(&nymph_hir::hir::HirExpr) -> bool,
) -> bool {
	use nymph_hir::hir::{HirArrayElem, HirExpr, HirMapElem, HirStmt};

	if predicate(expr) {
		return true;
	}
	let contains = |expr| hir_contains(expr, predicate);
	match expr {
		HirExpr::InterpolatedString(items) | HirExpr::Array { items, .. } => items.iter().any(contains),
		HirExpr::Call { callee, args } => contains(callee) || args.iter().any(contains),
		HirExpr::ExternCall { args, .. } => args.iter().any(contains),
		HirExpr::BoundDispatch {
			receiver, argument, ..
		} => contains(receiver) || contains(argument),
		HirExpr::UnaryBoundDispatch { receiver, .. } => contains(receiver),
		HirExpr::ArraySpread { elems, .. } => elems.iter().any(|elem| match elem {
			HirArrayElem::Item(expr) | HirArrayElem::Spread(expr) => contains(expr),
		}),
		HirExpr::MapLit(entries) => entries
			.iter()
			.any(|(key, value)| contains(key) || contains(value)),
		HirExpr::MapSpread(entries) => entries.iter().any(|entry| match entry {
			HirMapElem::Entry(key, value) => contains(key) || contains(value),
			HirMapElem::Spread(expr) => contains(expr),
		}),
		HirExpr::Index { recv, index } => contains(recv) || contains(index),
		HirExpr::MapGet { recv, key } => contains(recv) || contains(key),
		HirExpr::New { fields, .. } | HirExpr::VariantNew { fields, .. } => {
			fields.iter().any(|(_, value)| contains(value))
		}
		HirExpr::Field { recv, .. } => contains(recv),
		HirExpr::Binary { lhs, rhs, .. } => contains(lhs) || contains(rhs),
		HirExpr::Unary { operand, .. } | HirExpr::ScalarCast { operand, .. } => contains(operand),
		HirExpr::Assign { target, value } => contains(target) || contains(value),
		HirExpr::Block { stmts, tail } => {
			stmts.iter().any(|stmt| match stmt {
				HirStmt::Let { value, .. } | HirStmt::Expr(value) => contains(value),
				HirStmt::Return(value) => value.as_ref().is_some_and(contains),
				HirStmt::Break => false,
			}) || tail.as_ref().is_some_and(|tail| contains(tail))
		}
		HirExpr::If {
			cond,
			then,
			otherwise,
		} => contains(cond) || contains(then) || otherwise.as_ref().is_some_and(|expr| contains(expr)),
		HirExpr::While { cond, body } => contains(cond) || contains(body),
		HirExpr::Match { scrutinee, arms } => {
			contains(scrutinee)
				|| arms
					.iter()
					.any(|arm| arm.guard.as_ref().is_some_and(contains) || contains(&arm.body))
		}
		HirExpr::Closure { body, .. } => contains(body),
		HirExpr::Num(..)
		| HirExpr::Str(_)
		| HirExpr::Bool(_)
		| HirExpr::Char(_)
		| HirExpr::Local(_)
		| HirExpr::This
		| HirExpr::ExternValue { .. }
		| HirExpr::VariantRef { .. } => false,
	}
}

fn id(project: &str, name: &str) -> DefinitionId {
	DefinitionId::new(
		ModuleIdentity {
			origin: ModuleOrigin::Project(project.into()),
			project: project.into(),
			path: "main".into(),
		},
		DeclarationKey::top_level(DeclarationCategory::Function, name),
	)
}

fn top_level(
	interface: &ModuleInterface,
	category: DeclarationCategory,
	name: &str,
) -> DefinitionId {
	DefinitionId::new(
		interface.module.clone(),
		DeclarationKey::top_level(category, name),
	)
}

fn member(owner: &DefinitionId, name: &str) -> DefinitionId {
	DefinitionId::new(
		owner.module.clone(),
		DeclarationKey::member(owner.clone(), DeclarationCategory::Method, name),
	)
}

fn ambient_iteration_demands(session: &CompilerSession) -> [DefinitionId; 2] {
	let iterator_module = session
		.ambient_core_module_interface(AmbientCoreModuleKey::new("iter").unwrap())
		.expect("Iterator ambient interface exists");
	let iterator = top_level(&iterator_module, DeclarationCategory::Interface, "Iterator");
	let next = member(&iterator, "next");
	let iterable_module = session
		.ambient_core_module_interface(AmbientCoreModuleKey::new("iter/iterable").unwrap())
		.expect("Iterable ambient interface exists");
	let iterable = top_level(&iterable_module, DeclarationCategory::Interface, "Iterable");
	let iter = member(&iterable, "iter");
	let list_iter = top_level(&iterable_module, DeclarationCategory::Struct, "ListIter");
	let next_body = iterable_module
		.implementations
		.iter()
		.find(|implementation| {
			implementation.interface.as_ref() == Some(&iterator)
				&& matches!(
					&implementation.self_type,
					InterfaceType::Named { definition, .. } if definition == &list_iter
				)
		})
		.expect("exact ListIter Iterator implementation exists")
		.member_slots
		.iter()
		.find(|slot| slot.interface_member_id == next)
		.expect("exact Iterator.next slot exists")
		.member_id
		.clone();
	let iter_body = iterable_module
		.implementations
		.iter()
		.find(|implementation| {
			implementation.interface.as_ref() == Some(&iterable)
				&& matches!(&implementation.self_type, InterfaceType::List(_))
		})
		.expect("exact List Iterable implementation exists")
		.member_slots
		.iter()
		.find(|slot| slot.interface_member_id == iter)
		.expect("exact Iterable.iter slot exists")
		.member_id
		.clone();
	[next_body, iter_body]
}

#[test]
fn compiler_lowers_one_exact_runtime_definition() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("exact-lower");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"public func answer(): int = 42".into(),
		SourceVersion(1),
	);
	let definition = id("exact-lower", "answer");
	let lowered = session
		.lower_runtime_definition(project, main, definition.clone(), EntryMode::Library)
		.expect("exact definition lowers");
	assert_eq!(lowered.definition(), &definition);
}

#[test]
fn editing_one_definition_only_reexecutes_its_exact_lowering_consumer() {
	let events = Arc::new(Mutex::new(Vec::<SemanticQueryEvent>::new()));
	let sink = events.clone();
	let mut session = CompilerSession::with_detailed_event_callback_for_test(move |event| {
		sink.lock().unwrap().push(event)
	});
	let project = ProjectId::new("lower-invalidation");
	let main = ModulePath::new("main").unwrap();
	let a = id("lower-invalidation", "a");
	let b = id("lower-invalidation", "b");
	session.set_source(
		project.clone(),
		main.clone(),
		"func a(): int = 1\nfunc b(): int = 2".into(),
		SourceVersion(1),
	);
	let before_a = session
		.lower_runtime_definition(project.clone(), main.clone(), a.clone(), EntryMode::Library)
		.unwrap();
	let before_b = session
		.lower_runtime_definition(project.clone(), main.clone(), b.clone(), EntryMode::Library)
		.unwrap();
	events.lock().unwrap().clear();
	session.set_source(
		project.clone(),
		main.clone(),
		"func a(): int = 1\nfunc b(): int = 3".into(),
		SourceVersion(2),
	);
	let after_a = session
		.lower_runtime_definition(project.clone(), main.clone(), a.clone(), EntryMode::Library)
		.unwrap();
	let after_b = session
		.lower_runtime_definition(project, main, b.clone(), EntryMode::Library)
		.unwrap();
	assert_eq!(before_a, after_a);
	assert_ne!(before_b, after_b);
	let events = events.lock().unwrap();
	assert!(!events.iter().any(
		|event| event.query == "lower_runtime_definition" && event.definition.as_ref() == Some(&a)
	));
	assert!(events.iter().any(
		|event| event.query == "lower_runtime_definition" && event.definition.as_ref() == Some(&b)
	));
}

#[test]
fn native_list_and_bounded_ranges_lower_with_real_ambient_protocol_facts() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("ambient-iteration");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		r#"func list_sum(xs: mut #[int]): int = { xs[0] = xs[0] + 1 let mut total = 0 for (x in xs) { total = total + x } total }
func exclusive(): int = { let mut total = 0 for (x in 1..4) { total = total + x } total }
func inclusive(): int = { let mut total = 0 for (x in 1..=4) { total = total + x } total }"#.into(),
		SourceVersion(1),
	);
	let list = session
		.lower_runtime_definition(
			project.clone(),
			main.clone(),
			id("ambient-iteration", "list_sum"),
			EntryMode::Library,
		)
		.expect("native List update and iteration lower through exact ambient facts");
	let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = list.fragment() else {
		panic!("list_sum must lower to a top-level function");
	};
	let iter_body = ambient_iteration_demands(&session)[1].clone();
	let iter_binding = session
		.binding_name_for_test(project.clone(), main.clone(), iter_body, EntryMode::Library)
		.expect("selected List.iter body has an authoritative binding name");
	assert!(hir_contains(&function.body, &|expr| matches!(
		expr,
		nymph_hir::hir::HirExpr::Assign { target, .. }
			if matches!(target.as_ref(), nymph_hir::hir::HirExpr::Index { .. })
	)));
	assert!(hir_contains(&function.body, &|expr| matches!(
		expr,
		nymph_hir::hir::HirExpr::Call { callee, args }
			if args.len() == 1 && matches!(
				callee.as_ref(),
				nymph_hir::hir::HirExpr::Local(name) if name == iter_binding.as_str()
			)
	)));
	assert!(hir_contains(&function.body, &|expr| matches!(
		expr,
		nymph_hir::hir::HirExpr::While { .. }
	)));
	assert_eq!(list.demands(), ambient_iteration_demands(&session));
	for name in ["exclusive", "inclusive"] {
		let lowered = session
			.lower_runtime_definition(
				project.clone(),
				main.clone(),
				id("ambient-iteration", name),
				EntryMode::Library,
			)
			.unwrap_or_else(|error| panic!("{name} must lower through exact ambient facts: {error:?}"));
		let nymph_sema::LoweredHirFragment::TopLevelFunction(function) = lowered.fragment() else {
			panic!("{name} must lower to a top-level function");
		};
		assert!(hir_contains(&function.body, &|expr| matches!(
			expr,
			nymph_hir::hir::HirExpr::While { .. }
		)));
		assert_eq!(
			lowered.demands(),
			[],
			"{name} has no runtime definition demands"
		);
	}
}

#[test]
fn exact_ambient_binding_is_stable_when_an_unrelated_project_implementation_is_added() {
	let mut session = CompilerSession::new();
	let project = ProjectId::new("binding-stability");
	let main = ModulePath::new("main").unwrap();
	session.set_source(
		project.clone(),
		main.clone(),
		"func value(): int = 1".into(),
		SourceVersion(1),
	);
	let exact = ambient_iteration_demands(&session)[1].clone();
	let before = session
		.binding_name_for_test(
			project.clone(),
			main.clone(),
			exact.clone(),
			EntryMode::Library,
		)
		.expect("exact ambient binding before unrelated implementation");
	session.set_source(
		project.clone(),
		main.clone(),
		"interface Local { func value(): int }\nstruct Added()\nimpl Local for Added { func value(): int = 2 }"
			.into(),
		SourceVersion(2),
	);
	let after = session
		.binding_name_for_test(project, main, exact, EntryMode::Library)
		.expect("exact ambient binding after unrelated implementation");
	assert_eq!(before, after);
}

#[test]
fn lowering_an_import_consumer_never_executes_dependency_body_queries_after_warmup() {
	let mut session = CompilerSession::without_builtin_sources();
	let project = ProjectId::new("body-guard");
	let main = ModulePath::new("main").unwrap();
	let dep = ModulePath::new("dep").unwrap();
	session.set_source(
		project.clone(),
		dep.clone(),
		"public func supplied(): int = 41".into(),
		SourceVersion(1),
	);
	session.set_source(
		project.clone(),
		main.clone(),
		"import @/dep with (supplied)\nfunc consumer(): int = supplied() + 1".into(),
		SourceVersion(1),
	);
	let supplied = DefinitionId::new(
		ModuleIdentity {
			origin: ModuleOrigin::Project("body-guard".into()),
			project: "body-guard".into(),
			path: "dep".into(),
		},
		DeclarationKey::top_level(DeclarationCategory::Function, "supplied"),
	);
	session
		.runtime_definition(project.clone(), main.clone(), supplied, EntryMode::Library)
		.expect("warm exact dependency runtime fact");
	session.panic_on_dependency_body_access_for_test(project.clone(), main.clone());
	session
		.lower_runtime_definition(
			project,
			main,
			id("body-guard", "consumer"),
			EntryMode::Library,
		)
		.expect("consumer lowering uses exact warmed facts, not dependency analysis");
}
