#![cfg(feature = "test-support")]

use std::sync::{Arc, Mutex};

use nymph_compiler::project::{
	CompilerSession, ModulePath, ProjectId, SemanticPipeline, SemanticQueryEvent, SourceVersion,
};
use nymph_sema::{
	DeclarationCategory, DeclarationKey, DefinitionId, EntryMode, ModuleIdentity, ModuleOrigin,
};

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
	let mut session = CompilerSession::with_detailed_event_callback_for_test(
		SemanticPipeline::Interface,
		move |event| sink.lock().unwrap().push(event),
	);
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
	assert!(matches!(
		list.fragment(),
		nymph_sema::LoweredHirFragment::TopLevelFunction(function)
			if matches!(&function.body, nymph_hir::hir::HirExpr::Block { stmts, .. }
				if matches!(&stmts[0], nymph_hir::hir::HirStmt::Expr(nymph_hir::hir::HirExpr::Assign { target, .. })
					if matches!(target.as_ref(), nymph_hir::hir::HirExpr::Index { .. }))
				&& matches!(&stmts[2], nymph_hir::hir::HirStmt::Expr(nymph_hir::hir::HirExpr::Block { stmts, .. })
					if matches!(&stmts[0], nymph_hir::hir::HirStmt::Let { value: nymph_hir::hir::HirExpr::Call { callee, .. }, .. }
						if matches!(callee.as_ref(), nymph_hir::hir::HirExpr::Field { name, .. } if name == "iter"))
					&& matches!(&stmts[2], nymph_hir::hir::HirStmt::Expr(nymph_hir::hir::HirExpr::While { .. }))))
	));
	assert_eq!(list.demands(), []);
	for name in ["list_sum", "exclusive", "inclusive"] {
		session
			.lower_runtime_definition(
				project.clone(),
				main.clone(),
				id("ambient-iteration", name),
				EntryMode::Library,
			)
			.unwrap_or_else(|error| panic!("{name} must lower through exact ambient facts: {error:?}"));
	}
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
