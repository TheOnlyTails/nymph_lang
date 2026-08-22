use nymph_sema::{
	BinderScope, CanonicalizationContext, DeclarationCategory, DeclarationKey, DefinitionId,
	DefinitionShapeKind, EffectAtom, EffectRow, EffectSolver, ExportedDefinition, GenericParameterId,
	InterfaceType, ModuleEnvironment, ModuleIdentity, ModuleInterface, RecoveredEffectRow,
	RecoveredExportedDefinition, SelectedEffectContract, canonicalize_type,
	implementation_effects_are_valid,
};

fn definition(name: &str) -> DefinitionId {
	DefinitionId::new(
		ModuleIdentity::project("effects", "main"),
		DeclarationKey::top_level(DeclarationCategory::Function, name),
	)
}

fn row(names: &[&str]) -> EffectRow {
	EffectRow::new(
		names
			.iter()
			.map(|name| EffectAtom::Nominal(definition(name)))
			.collect(),
	)
}

fn exported(effects: EffectRow) -> ExportedDefinition {
	ExportedDefinition {
		id: definition("run"),
		name: "run".into(),
		visibility: None,
		kind: DefinitionShapeKind::Function,
		declaration_kind: None,
		binders: vec![],
		constraints: vec![],
		parameters: vec![],
		return_type: Some(InterfaceType::Void),
		effects,
		ty: None,
		fields: vec![],
		variants: vec![],
		enum_view_variants: vec![],
		members: vec![],
		super_interfaces: vec![],
		external: None,
		runtime_owner: None,
	}
}

fn interface(effects: EffectRow) -> ModuleInterface {
	let mut interface = ModuleInterface {
		module: ModuleIdentity::project("effects", "main"),
		exports: vec![exported(effects)],
		support_definitions: vec![],
		implementations: vec![],
		fingerprint: 0,
	};
	interface.fingerprint = interface.structural_fingerprint();
	interface
}

fn check_source(source: &str) -> nymph_sema::Checked {
	let parsed = nymph_syntax::parse_module(source, "effects.nym");
	assert!(
		parsed.diagnostics.is_empty(),
		"source failed to parse: {:?}",
		parsed.diagnostics
	);
	nymph_sema::check_module(&parsed.tree)
}

#[test]
fn managed_bindings_require_close_and_charge_its_effect() {
	let checked = check_source(
		"effect Io\n\
		 interface Close<!E> { func close(): void + !E }\n\
		 struct Resource\n\
		 impl Close<!Io> for Resource { func close(): !Io = {} }\n\
		 func managed(): !Io = { let use resource = Resource() }\n\
		 func generic<T: Close<!Io>>(resource: T): !Io = { let use managed = resource }\n\
		 func rejected(): void = { let use value = 1 }",
	);
	assert!(
		checked.diags.iter().any(|diagnostic| diagnostic
			.message
			.contains("requires a value implementing `Close`")),
		"{:?}",
		checked.diags
	);
	assert_eq!(callable_effects(&checked, "managed").atoms().len(), 1);
	assert_eq!(callable_effects(&checked, "generic").atoms().len(), 1);
}

#[test]
fn direct_managed_fields_warn_unless_the_owner_closes_them() {
	let checked = check_source(
		"interface Close<!E> { func close(): void + !E }\n\
		 struct Resource\n\
		 impl Close<!()> for Resource { func close(): void = {} }\n\
		 struct Borrowed(resource: Resource)\n\
		 struct Generic<T: Close<!()>>(resource: T)\n\
		 struct Owned(resource: Resource) {\n\
		   impl Close<!()> { func close(): void = this.resource.close() }\n\
		 }",
	);
	let warnings = checked
		.diags
		.iter()
		.filter(|diagnostic| diagnostic.code == "managed-field")
		.collect::<Vec<_>>();
	assert_eq!(warnings.len(), 2, "{:?}", checked.diags);
	assert!(
		warnings
			.iter()
			.all(|diagnostic| diagnostic.labels.len() == 2)
	);
}

#[test]
fn managed_child_capture_warning_has_declaration_close_and_join_labels() {
	let checked = check_source(
		"interface Close<!E> { func close(): void + !E }\n\
		 struct Resource\n\
		 impl Close<!()> for Resource { func close(): void = {} }\n\
		 async func risky(): void = {\n\
		   {\n\
		     let use resource = Resource()\n\
		     let child = async { resource.close() }.spawn()\n\
		   }\n\
		 }\n\
		 async func joined(): void = {\n\
		   {\n\
		     let use resource = Resource()\n\
		     let child = async { resource.close() }.spawn()\n\
		     child.await\n\
		   }\n\
		 }\n\
		 async func risky_indirect(): void = {\n\
		   {\n\
		     let use resource = Resource()\n\
		     let task = async { resource.close() }\n\
		     let child = task.spawn()\n\
		   }\n\
		 }\n\
		 async func shadowed(): void = {\n\
		   {\n\
		     let use resource = Resource()\n\
		     let task = async { let resource = Resource()\n resource.close() }\n\
		     let child = task.spawn()\n\
		   }\n\
		 }",
	);
	let warnings = checked
		.diags
		.iter()
		.filter(|diagnostic| diagnostic.code == "managed-child-capture")
		.collect::<Vec<_>>();
	assert_eq!(warnings.len(), 2, "{:?}", checked.diags);
	assert!(warnings.iter().all(|warning| warning.labels.len() == 3));
}

fn callable_effects<'a>(checked: &'a nymph_sema::Checked, name: &str) -> &'a EffectRow {
	let id = (0..checked.facts.semantic.definition_count())
		.filter_map(|index| {
			checked
				.facts
				.semantic
				.stable_definition(nymph_sema::DefId(index as u32))
		})
		.find(|id| {
			matches!(
				&id.key,
				DeclarationKey::TopLevel {
					category: DeclarationCategory::Function,
					name: definition_name,
					..
				} if definition_name == name
			)
		})
		.unwrap_or_else(|| panic!("missing callable {name}"));
	checked.facts.semantic.effect_row(id).unwrap()
}

#[test]
fn echo_preserves_operand_type_and_effects_without_adding_io() {
	let checked = check_source(
		"effect Database\n\
		 func read(): int + !Database = 1\n\
		 func observed(): int + !Database = echo read()",
	);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	assert_eq!(
		callable_effects(&checked, "observed"),
		callable_effects(&checked, "read")
	);
}

#[test]
fn async_recipe_construction_is_pure_and_await_charges_latent_effects() {
	let checked = check_source(
		"effect Io\n\
		 func read(): int + !Io = 1\n\
		 async func recipe(): int + !Io = read()\n\
		 func construct() = recipe()\n\
		 func spawn_it() = recipe().spawn()\n\
		 async func drive(): int + !Io = recipe().await",
	);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	assert_eq!(
		nominal_effect_names(callable_effects(&checked, "recipe")),
		vec!["Io"]
	);
	assert!(nominal_effect_names(callable_effects(&checked, "construct")).is_empty());
	assert_eq!(
		nominal_effect_names(callable_effects(&checked, "spawn_it")),
		vec!["Io"]
	);
	assert_eq!(
		nominal_effect_names(callable_effects(&checked, "drive")),
		vec!["Io"]
	);
}

#[test]
fn await_is_legal_only_in_async_contexts_and_async_blocks_capture_effects() {
	let legal = check_source("async func legal(): int = async { 1 }.await");
	assert!(legal.diags.is_empty(), "{:?}", legal.diags);
	let captures =
		check_source("effect Io\nfunc read(): int + !Io = 1\nfunc recipe() = async { read() }");
	assert!(captures.diags.is_empty(), "{:?}", captures.diags);
	assert!(nominal_effect_names(callable_effects(&captures, "recipe")).is_empty());

	let illegal = check_source("async func recipe(): int = 1\nfunc bad() = recipe().await");
	assert!(
		illegal
			.diags
			.iter()
			.any(|diagnostic| diagnostic.message.contains("only valid inside an async")),
		"{:?}",
		illegal.diags
	);
}

fn nominal_effect_names(row: &EffectRow) -> Vec<&str> {
	row
		.atoms()
		.iter()
		.filter_map(|atom| match atom {
			EffectAtom::Nominal(DefinitionId {
				key:
					DeclarationKey::TopLevel {
						category: DeclarationCategory::Effect,
						name,
						..
					},
				..
			}) => Some(name.as_str()),
			_ => None,
		})
		.collect()
}

#[test]
fn source_effect_rows_check_infer_compose_and_enforce_closed_annotations() {
	let checked = check_source(
		"effect Database\n\
		 effect Network\n\
		 external(read) func read(): !Database\n\
		 external(send) func send(): !Network\n\
		 func pure(): void + !() = {}\n\
		 func inferred(): !_ = { read() send() }\n\
		 func over_approximated(): !Database + !Network = read()",
	);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	assert!(nominal_effect_names(callable_effects(&checked, "pure")).is_empty());
	assert_eq!(
		nominal_effect_names(callable_effects(&checked, "inferred")),
		vec!["Database", "Network"],
		"{:?}",
		callable_effects(&checked, "inferred")
	);
	assert_eq!(
		nominal_effect_names(callable_effects(&checked, "over_approximated")),
		vec!["Database", "Network"]
	);

	let exceeds = check_source(
		"effect Database\nexternal(read) func read(): !Database\nfunc pure(): !() = read()",
	);
	assert!(exceeds.diags.iter().any(|diagnostic| {
		diagnostic
			.message
			.contains("requires effects outside its declared effect row")
	}));
}

#[test]
fn source_effect_diagnostics_distinguish_resolution_kind_and_inference_failures() {
	let cases = [
		(
			"func run(): !Missing = {}",
			"cannot find effect `Missing` in this scope",
		),
		(
			"struct Value\nfunc run(): !Value = {}",
			"`Value` is not an effect",
		),
		(
			"func run<!E>(value: E): void = {}",
			"generic parameter `E` is not a type parameter",
		),
		(
			"func run<T>(): !T = {}",
			"generic parameter `T` is not a effect parameter",
		),
		(
			"external(run) func run(): !_",
			"cannot infer this effect row without a callable body or initializer",
		),
	];
	for (source, expected) in cases {
		let checked = check_source(source);
		assert!(
			checked
				.diags
				.iter()
				.any(|diagnostic| diagnostic.message.contains(expected)),
			"missing {expected:?} for {source:?}: {:?}",
			checked.diags
		);
	}
}

#[test]
fn effect_generic_callable_arguments_instantiate_the_callers_charged_row() {
	let checked = check_source(
		"effect Io\n\
		 external(source) func source(): !Io\n\
		 func apply<!E>(callback: () -> void + !E): !E = callback()\n\
		 func caller(): !Io = apply(source)",
	);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	assert_eq!(
		nominal_effect_names(callable_effects(&checked, "caller")),
		vec!["Io"],
		"{:?}",
		callable_effects(&checked, "caller")
	);
}

#[test]
fn effect_generic_method_arguments_instantiate_the_callers_charged_row() {
	let checked = check_source(
		"effect Io\n\
		 external(source) func source(): !Io\n\
		 struct Runner\n\
		 impl Runner { func apply<!E>(callback: () -> void + !E): !E = callback() }\n\
		 func caller(runner: Runner): !Io = runner.apply(source)",
	);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
	assert_eq!(
		nominal_effect_names(callable_effects(&checked, "caller")),
		vec!["Io"],
		"{:?}",
		callable_effects(&checked, "caller")
	);
}

#[test]
fn effectful_callable_arguments_must_fit_the_declared_callback_contract() {
	let checked = check_source(
		"effect Io\n\
		 external(source) func source(): !Io\n\
		 func accepts_pure(callback: () -> void + !()): void = {}\n\
		 func caller(): void = accepts_pure(source)",
	);
	assert!(
		checked
			.diags
			.iter()
			.any(|diagnostic| diagnostic.message.contains("mismatched types")),
		"{:?}",
		checked.diags
	);
}

#[test]
fn implementation_effect_rows_may_narrow_but_not_widen_interface_contracts() {
	let narrower = check_source(
		"effect Io\n\
		 interface Runner { func run(): !Io }\n\
		 struct Worker\n\
		 impl Runner for Worker { func run(): !() = {} }",
	);
	assert!(narrower.diags.is_empty(), "{:?}", narrower.diags);

	let broader = check_source(
		"effect Io\n\
		 effect Network\n\
		 interface Runner { func run(): !Io }\n\
		 struct Worker\n\
		 impl Runner for Worker { func run(): !Network = {} }",
	);
	assert!(
		broader.diags.iter().any(|diagnostic| {
			diagnostic
				.message
				.contains("requires effects outside the interface contract")
		}),
		"{:?}",
		broader.diags
	);

	let generic = check_source(
		"effect Io\n\
		 effect Network\n\
		 interface Runner<!E> { func run(): !E }\n\
		 struct Good\n\
		 struct Bad\n\
		 impl Runner<!Io> for Good { func run(): !() = {} }\n\
		 impl Runner<!Io> for Bad { func run(): !Network = {} }",
	);
	assert_eq!(
		generic
			.diags
			.iter()
			.filter(|diagnostic| diagnostic
				.message
				.contains("requires effects outside the interface contract"))
			.count(),
		1,
		"{:?}",
		generic.diags
	);
}

#[test]
fn implementation_effect_contracts_preserve_forwarded_generic_rows() {
	let checked = check_source(
		"interface Runner<!E> { func run(): !E }\n\
		 struct Worker<!F> { impl Runner<!F> { func run(): !F = {} } }",
	);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);
}

#[test]
fn generic_bound_methods_charge_forwarded_effect_rows() {
	for source in [
		"effect Io\ninterface Source<Item + !E> { func next(): Item + !E }\nfunc one<!E, T: Source<int + !E>>(source: T): int + !E = source.next()",
		"interface Source2<Item + !E + !F> { func next(): Item + !E + !F }\nfunc two<!E, !F, T: Source2<int, E = !E, F = !F>>(source: T): int + !E + !F = source.next()",
	] {
		let checked = check_source(source);
		assert!(checked.diags.is_empty(), "{source}: {:?}", checked.diags);
	}
}

#[test]
fn generic_bound_methods_still_reject_true_excess_effects() {
	let checked = check_source(
		"effect Io\ninterface Source<Item + !E> { func next(): Item + !E }\nfunc excess<T: Source<int + !Io>>(source: T): int = source.next()",
	);
	assert!(
		checked.diags.iter().any(|diagnostic| diagnostic
			.message
			.contains("outside its declared effect row")),
		"{:?}",
		checked.diags
	);
}

#[test]
fn rows_are_stable_id_sorted_deduplicated_and_fingerprint_deterministic() {
	let owner = definition("owner");
	let parameter = GenericParameterId::new(owner.binder(BinderScope::Definition, 0), 0);
	let io = EffectAtom::Nominal(definition("io"));
	let database = EffectAtom::Nominal(definition("database"));
	let rigid = EffectAtom::Parameter(parameter);
	let first = EffectRow::new(vec![io.clone(), rigid.clone(), database.clone(), io]);
	let second = EffectRow::new(vec![database, rigid, EffectAtom::Nominal(definition("io"))]);
	assert_eq!(first, second);
	assert_eq!(interface(first).fingerprint, interface(second).fingerprint);
	assert_ne!(
		interface(row(&["io"])).fingerprint,
		interface(row(&["database"])).fingerprint
	);
}

#[test]
fn recursive_constraints_converge_to_the_least_solution() {
	let mut solver = EffectSolver::default();
	let first = solver.variable();
	let second = solver.variable();
	let third = solver.variable();
	solver.require_row(row(&["database"]), first);
	solver.require_row(row(&["io"]), third);
	solver.require_subset(first, second);
	solver.require_subset(second, first);
	solver.require_subset(second, third);
	solver.require_subset(third, second);
	let solution = solver.solve().unwrap();
	let expected = row(&["database", "io"]);
	assert_eq!(solution.row(first), &expected);
	assert_eq!(solution.row(second), &expected);
	assert_eq!(solution.row(third), &expected);
}

#[test]
fn closed_upper_bounds_report_only_the_excess() {
	let mut solver = EffectSolver::default();
	let variable = solver.variable();
	solver.require_row(row(&["database", "network"]), variable);
	solver.set_upper_bound(variable, row(&["database"]));
	let errors = solver.solve().unwrap_err();
	assert_eq!(errors.len(), 1);
	assert_eq!(errors[0].excess, row(&["network"]));
}

#[test]
fn narrower_implementations_are_valid_and_dispatch_charges_the_selected_contract() {
	let concrete = row(&["database"]);
	let interface = row(&["database", "telemetry"]);
	assert!(implementation_effects_are_valid(&concrete, &interface));
	assert!(!implementation_effects_are_valid(&interface, &concrete));
	assert_eq!(
		SelectedEffectContract::Concrete(&concrete).charged_row(),
		&concrete
	);
	assert_eq!(
		SelectedEffectContract::Interface(&interface).charged_row(),
		&interface
	);
	assert_eq!(
		SelectedEffectContract::Generic(&interface).charged_row(),
		&interface
	);
}

#[test]
fn callable_types_and_complete_recovered_interfaces_retain_resolved_rows() {
	let local_effect = nymph_sema::DefId(1);
	let stable_effect = definition("io");
	let mut interner = nymph_sema::Interner::new();
	let int = interner.int();
	let semantic = interner.mk_effectful_fn(
		vec![int],
		int,
		nymph_sema::ty::EffectRow::new(vec![nymph_sema::ty::EffectAtom::Nominal(local_effect)]),
	);
	let canonical = canonicalize_type(
		&interner,
		semantic,
		&CanonicalizationContext::new(
			[(local_effect, stable_effect.clone())].into(),
			Default::default(),
		),
	)
	.unwrap();
	let InterfaceType::Function { effects, .. } = canonical else {
		panic!("expected callable interface type")
	};
	assert_eq!(
		effects,
		EffectRow::new(vec![EffectAtom::Nominal(stable_effect)])
	);

	let complete = exported(effects.clone());
	let recovered = RecoveredExportedDefinition::from(complete);
	assert_eq!(recovered.effects, RecoveredEffectRow::Known(effects));
}

#[test]
fn imported_callable_types_allocate_nominal_effect_references_before_instantiation() {
	let effect = definition("io");
	let mut value = exported(EffectRow::pure());
	value.kind = DefinitionShapeKind::Let;
	value.return_type = None;
	value.ty = Some(InterfaceType::Function {
		parameters: vec![],
		return_type: Box::new(InterfaceType::Void),
		effects: EffectRow::new(vec![EffectAtom::Nominal(effect)]),
	});
	let mut dependency = interface(EffectRow::pure());
	dependency.exports = vec![value];
	dependency.fingerprint = dependency.structural_fingerprint();
	let _environment = nymph_sema::SemanticEnvironment::from_modules(
		ModuleIdentity::project("effects", "consumer"),
		&[std::sync::Arc::new(ModuleEnvironment::Complete(dependency))],
	)
	.unwrap();
}

#[test]
fn checked_publication_resolves_current_callable_rows_before_facts_escape() {
	let parsed = nymph_syntax::parse_module("func run(): int = 1", "test");
	let identity = ModuleIdentity::project("effects", "main");
	let environment = nymph_sema::SemanticEnvironment::from_modules(identity.clone(), &[]).unwrap();
	let checked = nymph_sema::check_module_with_environment(
		std::sync::Arc::new(parsed.tree),
		identity,
		&environment,
		nymph_sema::EntryMode::Library,
	);
	assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
	let id = (0..checked.analysis.checked.semantic.definition_count())
		.filter_map(|index| {
			checked
				.analysis
				.checked
				.semantic
				.stable_definition(nymph_sema::DefId(index as u32))
		})
		.find(|id| {
			matches!(
				id.key,
				DeclarationKey::TopLevel {
					category: DeclarationCategory::Function,
					..
				}
			)
		})
		.unwrap();
	assert_eq!(
		checked.analysis.checked.semantic.effect_row(id),
		Some(&EffectRow::pure())
	);
}
