use std::sync::Arc;

use nymph_ast::{
	Ident, Span,
	decl::{Declaration, Module},
};
use nymph_diagnostics::Diagnostic;
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use super::{
	ProjectDiagnostic,
	resolve::{ResolvedImportTarget, resolve_import_target},
	session::{
		AmbientCoreRegistryInput, BuiltinModuleDomain, BuiltinModuleInput, LintLevel, ModuleInput,
		ModulePath, PackageId, ProjectInput, ProjectKey, ProjectPolicyInput, SemanticModuleDomain,
		SemanticModuleInput,
	},
};

#[salsa::tracked(returns(copy))]
pub(crate) fn effective_lint_level(
	db: &dyn Db,
	policy: ProjectPolicyInput,
	package: PackageId,
	lint: Arc<str>,
	default: LintLevel,
) -> LintLevel {
	if package != policy.root_package(db) {
		return default;
	}
	policy
		.lints(db)
		.iter()
		.find(|setting| setting.name == lint)
		.map_or(default, |setting| setting.level)
}

fn apply_semantic_lint(
	db: &dyn Db,
	key: ProjectKey<'_>,
	module: SemanticModuleInput,
	mut diagnostic: Diagnostic,
) -> Option<Diagnostic> {
	let lint = match diagnostic.code.as_str() {
		"managed-field" | "managed-child-capture" => diagnostic.code.clone(),
		_ => return Some(diagnostic),
	};
	let SemanticModuleInput::Project(module) = module else {
		return Some(diagnostic);
	};
	match effective_lint_level(
		db,
		key.policy_input(db),
		module.package(db),
		Arc::from(lint.as_str()),
		LintLevel::Warn,
	) {
		LintLevel::Allow => None,
		LintLevel::Warn => Some(diagnostic),
		LintLevel::Deny => {
			diagnostic.severity = nymph_diagnostics::Severity::Error;
			Some(diagnostic)
		}
	}
}

#[salsa::tracked(returns(clone))]
fn policy_project_diagnostics(
	db: &dyn Db,
	key: ProjectKey<'_>,
) -> super::session::ProjectDiagnostics {
	let policy = key.policy_input(db);
	if policy.profile(db) != super::session::BuildProfile::Release {
		return super::session::ProjectDiagnostics(Arc::new([]));
	}
	let root_package = policy.root_package(db);
	let level = effective_lint_level(
		db,
		policy,
		root_package.clone(),
		Arc::from("echo-in-release"),
		LintLevel::Warn,
	);
	if level == LintLevel::Allow {
		return super::session::ProjectDiagnostics(Arc::new([]));
	}
	let mut diagnostics = Vec::new();
	for module in key.project_input(db).active_modules(db).iter().copied() {
		if module.package(db) != root_package {
			continue;
		}
		let module_name = module.path(db).to_string();
		for span in nymph_sema::query::echo_sites(&parse(db, module).tree) {
			let message = "`echo` is erased from release builds";
			let diagnostic = match level {
				LintLevel::Allow => unreachable!(),
				LintLevel::Warn => Diagnostic::warning("echo-in-release".into(), message, span),
				LintLevel::Deny => Diagnostic::error("echo-in-release".into(), message, span),
			}
			.with_help("use `println` or telemetry for intentional release output");
			diagnostics.push(ProjectDiagnostic {
				module: module_name.clone(),
				diag: diagnostic,
			});
		}
	}
	super::session::ProjectDiagnostics(diagnostics.into())
}

#[salsa::db]
pub(crate) trait Db: salsa::Database {
	#[cfg(not(target_arch = "wasm32"))]
	fn parallel_clone(&self) -> Box<dyn Db>;

	#[cfg(feature = "test-support")]
	fn semantic_query_will_execute(&self, _query: &'static str, _module: SemanticModuleInput) {}
	#[cfg(feature = "test-support")]
	fn runtime_query_will_execute(
		&self,
		_query: &'static str,
		_definition: &nymph_sema::DefinitionId,
	) {
	}
}

#[salsa::tracked]
pub(crate) struct RuntimeDefinitionEntity<'db> {
	#[returns(ref)]
	pub definition: nymph_sema::DefinitionId,
	#[tracked]
	#[returns(clone)]
	pub value: Arc<nymph_sema::RuntimeDefinition>,
}

/// Source-ordered runtime inventory for one semantic module. Artifact payloads
/// remain tracked independently so consumers of an unchanged definition can
/// backdate even when another manifest entry changes.
#[derive(Clone, PartialEq, Eq, salsa::SalsaValue)]
pub(crate) struct RuntimeManifest<'db> {
	definitions: Arc<[RuntimeDefinitionEntity<'db>]>,
}

impl<'db> RuntimeManifest<'db> {
	fn new(
		db: &'db dyn Db,
		definitions: impl IntoIterator<Item = nymph_sema::RuntimeDefinition>,
	) -> Result<Self, nymph_sema::RuntimeExtractionError> {
		let mut seen = std::collections::BTreeSet::new();
		let definitions = definitions.into_iter().collect::<Vec<_>>();
		for definition in &definitions {
			if !seen.insert(definition.definition.clone()) {
				return Err(
					nymph_sema::RuntimeExtractionError::DuplicateRuntimeDefinition(
						definition.definition.clone(),
					),
				);
			}
		}
		let definitions = definitions
			.into_iter()
			.map(|value| RuntimeDefinitionEntity::new(db, value.definition.clone(), Arc::new(value)))
			.collect::<Vec<_>>()
			.into();
		Ok(Self { definitions })
	}

	pub(crate) fn definitions(&self) -> &[RuntimeDefinitionEntity<'db>] {
		&self.definitions
	}

	fn definition(
		&self,
		db: &'db dyn Db,
		definition: &nymph_sema::DefinitionId,
	) -> Option<RuntimeDefinitionEntity<'db>> {
		self
			.definitions
			.iter()
			.copied()
			.find(|entity| entity.definition(db) == definition)
	}
}

#[salsa::tracked(returns(clone))]
fn ambient_runtime_role_inventory(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<nymph_sema::CompilerRuntimeRoles> {
	use nymph_ast::decl::{Declaration, InterfaceElement, InterfaceMember};
	use nymph_ast::ty::Type;
	use nymph_sema::{DeclarationCategory as Category, DeclarationKey, DefinitionId};
	let parsed = parse_builtin(db, module);
	if !parsed.diagnostics.is_empty() {
		return Arc::new(Default::default());
	}
	let identity = ambient_identity(db, module);
	let headers = nymph_sema::declared_headers(identity.clone(), &parsed.tree);
	let exact_top = |name: &str, expected: Category| {
		let mut matches = headers
			.definitions
			.iter()
			.filter(|(candidate, _)| candidate == name);
		let result = matches.next()?.1.clone();
		(matches.next().is_none()
			&& matches!(
				result.key,
				DeclarationKey::TopLevel { category, .. } if category == expected
			))
		.then_some(result)
	};
	let interface = |name: &str, member_name: &str| {
		let owner = exact_top(name, Category::Interface)?;
		let mut declarations = parsed
			.tree
			.members
			.iter()
			.filter_map(|declaration| match declaration {
				Declaration::Interface {
					name: candidate,
					generics,
					members,
					..
				} if candidate.0 == name => Some((generics, members)),
				_ => None,
			});
		let (generics, members) = declarations.next()?;
		if declarations.next().is_some() {
			return None;
		}
		let mut methods = members.iter().filter_map(|member| {
			let InterfaceMember::Element(element) = &member.0 else {
				return None;
			};
			let InterfaceElement::Func { meta, .. } = &element.0 else {
				return None;
			};
			(meta.name.0 == member_name).then_some(meta)
		});
		let method = methods.next()?;
		if methods.next().is_some() {
			return None;
		}
		let valid_shape = method.params.is_empty()
			&& match name {
				"Display" | "Debug" => {
					generics.is_empty()
						&& matches!(
							method.return_type.as_ref().map(|ty| &ty.0),
							Some(Type::String)
						)
				}
				"Iterator" => {
					matches!(
						generics.as_slice(),
						[item, effect]
							if item.0.name.0 == "Item"
								&& effect.0.kind == nymph_ast::ty::GenericParamKind::Effect
					) && matches!(
						method.return_type.as_ref().map(|ty| &ty.0),
						Some(Type::Reference { name, generics: arguments })
							if name.0 == "Iteration"
								&& matches!(
									arguments.as_slice(),
									[item, next]
										if matches!(item.0.value.as_type().map(|ty| &ty.0), Some(Type::Reference { name, generics }) if name.0 == "Item" && generics.is_empty())
											&& matches!(next.0.value.as_type().map(|ty| &ty.0), Some(Type::SelfType))
								)
					)
				}
				"Iterable" => {
					matches!(
						generics.as_slice(),
						[item, effect]
							if item.0.name.0 == "Item"
								&& effect.0.kind == nymph_ast::ty::GenericParamKind::Effect
					) && matches!(
						method.return_type.as_ref().map(|ty| &ty.0),
						Some(Type::Reference { name, generics: arguments })
							if name.0 == "Iterator"
								&& matches!(arguments.as_slice(), [item, effect]
									if matches!(item.0.value.as_type().map(|ty| &ty.0), Some(Type::Reference { name, generics }) if name.0 == "Item" && generics.is_empty())
										&& effect.0.value.as_effect().is_some())
					)
				}
				"Close" => {
					matches!(
						generics.as_slice(),
						[generic]
							if generic.0.kind == nymph_ast::ty::GenericParamKind::Effect
								&& method.return_type.as_ref().is_some_and(|ty| ty.0 == Type::Void)
								&& method.effects.as_ref().is_some_and(|effects| {
									matches!(
										effects.0.effects.as_slice(),
										[effect]
											if matches!(
												&effect.0,
												nymph_ast::ty::Effect::Named(name)
													if name.0 == generic.0.name.0
											)
									)
								})
					)
				}
				_ => false,
			};
		if !valid_shape {
			return None;
		}
		Some(nymph_sema::InterfaceRuntimeRole {
			interface: owner.clone(),
			member: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(owner, Category::Method, member_name),
			),
		})
	};
	let option = (|| {
		let owner = exact_top("Option", Category::Enum)?;
		let mut enums = parsed
			.tree
			.members
			.iter()
			.filter_map(|declaration| match declaration {
				Declaration::Enum {
					name,
					generics,
					variants,
					..
				} if name.0 == "Option" => Some((generics, variants)),
				_ => None,
			});
		let (generics, variants) = enums.next()?;
		if enums.next().is_some() {
			return None;
		}
		let [generic] = generics.as_slice() else {
			return None;
		};
		let mut some = variants.iter().filter(|variant| variant.0.name.0 == "Some");
		let some_shape = some.next()?;
		if some.next().is_some()
			|| some_shape.0.fields.len() != 1
			|| some_shape.0.fields[0].0.name.0 != "value"
			|| !matches!(
				&some_shape.0.fields[0].0.type_.0,
				Type::Reference { name, generics }
					if name.0 == generic.0.name.0 && generics.is_empty()
			) {
			return None;
		}
		let mut none = variants.iter().filter(|variant| variant.0.name.0 == "None");
		let none_shape = none.next()?;
		if none.next().is_some() || !none_shape.0.fields.is_empty() {
			return None;
		}
		let some = DefinitionId::new(
			identity.clone(),
			DeclarationKey::member(owner.clone(), Category::Variant, "Some"),
		);
		Some(nymph_sema::OptionRuntimeRole {
			option: owner.clone(),
			some_value: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(some.clone(), Category::Field, "value"),
			),
			some,
			none: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(owner, Category::Variant, "None"),
			),
		})
	})();
	let iteration = (|| {
		let owner = exact_top("Iteration", Category::Enum)?;
		let mut enums = parsed
			.tree
			.members
			.iter()
			.filter_map(|declaration| match declaration {
				Declaration::Enum {
					name,
					generics,
					variants,
					..
				} if name.0 == "Iteration" => Some((generics, variants)),
				_ => None,
			});
		let (generics, variants) = enums.next()?;
		if enums.next().is_some() || generics.len() != 2 {
			return None;
		}
		let done = variants
			.iter()
			.find(|variant| variant.0.name.0 == "Done" && variant.0.fields.is_empty())?;
		let yield_ = variants.iter().find(|variant| {
			variant.0.name.0 == "Yield"
				&& matches!(variant.0.fields.as_slice(), [item, next]
					if item.0.name.0 == "item" && next.0.name.0 == "next")
		})?;
		let yield_id = DefinitionId::new(
			identity.clone(),
			DeclarationKey::member(owner.clone(), Category::Variant, "Yield"),
		);
		Some(nymph_sema::IterationRuntimeRole {
			iteration: owner.clone(),
			done: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(owner, Category::Variant, &done.0.name.0),
			),
			yield_item: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(
					yield_id.clone(),
					Category::Field,
					&yield_.0.fields[0].0.name.0,
				),
			),
			yield_next: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(
					yield_id.clone(),
					Category::Field,
					&yield_.0.fields[1].0.name.0,
				),
			),
			yield_: yield_id,
		})
	})();
	let result = (|| {
		let owner = exact_top("Result", Category::Enum)?;
		let mut enums = parsed
			.tree
			.members
			.iter()
			.filter_map(|declaration| match declaration {
				Declaration::Enum {
					name,
					generics,
					variants,
					..
				} if name.0 == "Result" => Some((generics, variants)),
				_ => None,
			});
		let (generics, variants) = enums.next()?;
		if enums.next().is_some() {
			return None;
		}
		let [ok_generic, error_generic] = generics.as_slice() else {
			return None;
		};
		let variant = |name: &str, field: &str, generic: &str| {
			let mut matches = variants.iter().filter(|variant| variant.0.name.0 == name);
			let shape = matches.next()?;
			if matches.next().is_some()
				|| shape.0.fields.len() != 1
				|| shape.0.fields[0].0.name.0 != field
				|| !matches!(
					&shape.0.fields[0].0.type_.0,
					Type::Reference { name, generics } if name.0 == generic && generics.is_empty()
				) {
				return None;
			}
			Some(())
		};
		variant("Ok", "value", &ok_generic.0.name.0)?;
		variant("Error", "error", &error_generic.0.name.0)?;
		let ok = DefinitionId::new(
			identity.clone(),
			DeclarationKey::member(owner.clone(), Category::Variant, "Ok"),
		);
		let error = DefinitionId::new(
			identity.clone(),
			DeclarationKey::member(owner.clone(), Category::Variant, "Error"),
		);
		Some(nymph_sema::ResultRuntimeRole {
			result: owner,
			ok_value: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(ok.clone(), Category::Field, "value"),
			),
			ok,
			error_value: DefinitionId::new(
				identity.clone(),
				DeclarationKey::member(error.clone(), Category::Field, "error"),
			),
			error,
		})
	})();
	Arc::new(nymph_sema::CompilerRuntimeRoles {
		display: interface("Display", "display"),
		debug: interface("Debug", "debug"),
		iterable: interface("Iterable", "iter"),
		iterator: interface("Iterator", "next"),
		iteration,
		close: interface("Close", "close"),
		option,
		result,
	})
}

#[salsa::tracked(returns(clone))]
pub(crate) fn compiler_runtime_roles(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
) -> Arc<nymph_sema::CompilerRuntimeRoles> {
	let module = |path: &str| {
		registry
			.modules(db)
			.iter()
			.copied()
			.find(|module| module.key(db).path.as_ref() == path)
	};
	let interface =
		|path: &str,
		 select: fn(&nymph_sema::CompilerRuntimeRoles) -> &Option<nymph_sema::InterfaceRuntimeRole>| {
			select(&ambient_runtime_role_inventory(db, module(path)?)).clone()
		};
	let option =
		module("option").and_then(|module| ambient_runtime_role_inventory(db, module).option.clone());
	let result =
		module("result").and_then(|module| ambient_runtime_role_inventory(db, module).result.clone());
	let iteration =
		module("iter").and_then(|module| ambient_runtime_role_inventory(db, module).iteration.clone());
	Arc::new(nymph_sema::CompilerRuntimeRoles {
		display: interface("ops", |roles| &roles.display),
		debug: interface("ops", |roles| &roles.debug),
		iterable: interface("iter/iterable", |roles| &roles.iterable),
		iterator: interface("iter", |roles| &roles.iterator),
		iteration,
		close: interface("ops", |roles| &roles.close),
		option,
		result,
	})
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedModule {
	pub tree: Module,
	pub diagnostics: Arc<[Diagnostic]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectImport {
	pub target: Result<ResolvedImportTarget, Diagnostic>,
	pub span: Span,
	pub namespace: Ident,
	pub has_with_list: bool,
	pub with_idents: Vec<(Ident, Option<Ident>)>,
}

pub type DirectImports = [DirectImport];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedModuleImports {
	pub bindings: FxHashMap<ecow::EcoString, nymph_sema::ResolvedImportBinding>,
	pub namespaces: Vec<(ecow::EcoString, nymph_sema::ModuleIdentity)>,
	pub references: Vec<(Span, nymph_sema::ImportReferenceTarget)>,
	pub diagnostics: Arc<[ProjectDiagnostic]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ImportDiagnosticCause {
	NameCollision {
		local: ecow::EcoString,
		span: Span,
	},
	PrivateName {
		declaration: nymph_sema::NamespaceDeclaration,
		span: Span,
	},
	MissingName {
		name: ecow::EcoString,
		span: Span,
	},
	PrivateNamespaceMember {
		declaration: nymph_sema::NamespaceDeclaration,
		span: Span,
	},
	MissingNamespaceMember {
		member: ecow::EcoString,
		span: Span,
	},
}

impl ImportDiagnosticCause {
	fn render(self, module: &str) -> ProjectDiagnostic {
		let (code, message, span) = match self {
			Self::NameCollision { local, span } => (
				"IMPORT-NAME-COLLISION",
				format!("import name `{local}` collides with another lexical binding"),
				span,
			),
			Self::PrivateName { declaration, span } => (
				"IMPORT-PRIVATE-NAME",
				format!("`{}` is private and cannot be imported", declaration.name),
				span,
			),
			Self::MissingName { name, span } => (
				"IMPORT-UNRESOLVED-NAME",
				format!("imported module has no exported name `{name}`"),
				span,
			),
			Self::PrivateNamespaceMember { declaration, span } => (
				"IMPORT-PRIVATE-NAME",
				format!(
					"private imported namespace member `{}` cannot be accessed",
					declaration.name
				),
				span,
			),
			Self::MissingNamespaceMember { member, span } => (
				"IMPORT-UNRESOLVED-NAME",
				format!("imported namespace has no member `{member}`"),
				span,
			),
		};
		ProjectDiagnostic {
			module: module.to_string(),
			diag: Diagnostic::error(code.into(), message, span),
		}
	}
}

#[salsa::tracked(returns(clone))]
fn namespace_summary<'db>(
	db: &'db dyn Db,
	_key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<nymph_sema::NamespaceSummary> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("namespace_summary", module);
	Arc::new(nymph_sema::namespace_summary(
		module.identity(db),
		&module.parsed(db).tree,
	))
}

fn local_declaration_name(declaration: &Declaration) -> Option<Ident> {
	match declaration {
		Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => match &meta.name.0 {
			nymph_ast::expr::Pattern::Binding { name, .. } => Some(name.clone()),
			_ => None,
		},
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
			Some(meta.name.clone())
		}
		Declaration::TypeAlias { meta, .. } => Some(meta.name.clone()),
		Declaration::Struct { name, .. }
		| Declaration::Enum { name, .. }
		| Declaration::Namespace { name, .. }
		| Declaration::Interface { name, .. } => Some(name.clone()),
		_ => None,
	}
}

fn insert_import_binding(
	bindings: &mut FxHashMap<ecow::EcoString, nymph_sema::ResolvedImportBinding>,
	locals: &FxHashMap<ecow::EcoString, Span>,
	diagnostic_causes: &mut Vec<ImportDiagnosticCause>,
	local: ecow::EcoString,
	span: Span,
	binding: nymph_sema::ResolvedImportBinding,
) -> bool {
	if bindings.contains_key(&local) || locals.contains_key(&local) {
		diagnostic_causes.push(ImportDiagnosticCause::NameCollision { local, span });
		return false;
	}
	bindings.insert(local, binding);
	true
}

fn resolved_import_module(
	db: &dyn Db,
	key: ProjectKey<'_>,
	owner: SemanticModuleInput,
	target: &ResolvedImportTarget,
) -> Option<SemanticModuleInput> {
	match target {
		ResolvedImportTarget::ImportableStd(path) => key
			.builtin_registry(db)
			.modules(db)
			.iter()
			.copied()
			.find(|module| module.key(db).path.as_ref() == path)
			.map(SemanticModuleInput::Builtin),
		ResolvedImportTarget::CurrentPackage(path) => match owner {
			SemanticModuleInput::Project(module) => key
				.project_input(db)
				.active_modules(db)
				.iter()
				.copied()
				.find(|candidate| {
					candidate.package(db) == module.package(db) && candidate.path(db).as_str() == path
				})
				.map(SemanticModuleInput::Project),
			SemanticModuleInput::Builtin(_) => key
				.builtin_registry(db)
				.modules(db)
				.iter()
				.copied()
				.find(|module| module.key(db).path.as_ref() == path)
				.map(SemanticModuleInput::Builtin),
		},
		ResolvedImportTarget::Package { alias, path } => {
			let SemanticModuleInput::Project(module) = owner else {
				return None;
			};
			let project = key.project_input(db);
			let package = project
				.package_aliases(db)
				.iter()
				.find(|edge| edge.owner == module.package(db) && edge.name.as_ref() == alias)?
				.target
				.clone();
			project
				.active_modules(db)
				.iter()
				.copied()
				.find(|candidate| candidate.package(db) == package && candidate.path(db).as_str() == path)
				.map(SemanticModuleInput::Project)
		}
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn resolved_module_imports<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<ResolvedModuleImports> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("resolved_module_imports", module);
	let graph = project_graph(db, key);
	let direct = graph
		.semantic_direct_dependencies(module)
		.iter()
		.copied()
		.filter(|dependency| {
			!key.isolated(db) || dependency.domain(db) != SemanticModuleDomain::Project
		})
		.collect::<Vec<_>>();
	let locals = module
		.parsed(db)
		.tree
		.members
		.iter()
		.filter_map(local_declaration_name)
		.map(|name| (name.0.clone(), name.1))
		.collect::<FxHashMap<_, _>>();
	let mut bindings = FxHashMap::default();
	let mut namespaces = Vec::new();
	let mut references = Vec::new();
	let mut diagnostic_causes = Vec::new();
	let owner = module.display_key(db);
	for import in graph.semantic_direct_imports(db, module).iter() {
		let Ok(target_key) = &import.target else {
			continue;
		};
		let Some(target) =
			resolved_import_module(db, key, module, target_key).filter(|target| direct.contains(target))
		else {
			continue;
		};
		let identity = target.identity(db);
		let current_identity = module.identity(db);
		let summary = namespace_summary(db, key, target);
		let namespace = import.namespace.0.clone();
		let namespace_matches_export = summary.declaration(&namespace).is_some_and(|declaration| {
			declaration
				.visibility
				.allows(&summary.module, &current_identity)
		}) && (!import.has_with_list
			|| import.with_idents.iter().any(|(source, alias)| {
				source.0 == namespace && alias.as_ref().unwrap_or(source).0 == namespace
			}));
		if !namespace_matches_export
			&& insert_import_binding(
				&mut bindings,
				&locals,
				&mut diagnostic_causes,
				namespace.clone(),
				import.namespace.1,
				nymph_sema::ResolvedImportBinding::Namespace(identity.clone()),
			) {
			references.push((
				import.namespace.1,
				nymph_sema::ImportReferenceTarget::Module(identity.clone()),
			));
			namespaces.push((namespace, identity));
		}
		let selected = if import.has_with_list {
			import
				.with_idents
				.iter()
				.map(|(source, alias)| {
					let mut spans = vec![source.1];
					if let Some(alias) = alias {
						spans.push(alias.1);
					}
					(
						source.0.clone(),
						alias.as_ref().unwrap_or(source).0.clone(),
						source.1,
						spans,
					)
				})
				.collect::<Vec<_>>()
		} else {
			summary
				.declarations
				.iter()
				.enumerate()
				.filter(|(index, declaration)| {
					declaration
						.visibility
						.allows(&summary.module, &current_identity)
						&& !summary.declarations[index + 1..]
							.iter()
							.any(|later| later.name == declaration.name)
				})
				.map(|(_, declaration)| {
					(
						declaration.name.clone(),
						declaration.name.clone(),
						import.namespace.1,
						Vec::new(),
					)
				})
				.collect::<Vec<_>>()
		};
		for (source, local, span, written_spans) in selected {
			match summary.declaration(&source) {
				Some(declaration)
					if declaration
						.visibility
						.allows(&summary.module, &current_identity) =>
				{
					if insert_import_binding(
						&mut bindings,
						&locals,
						&mut diagnostic_causes,
						local,
						span,
						nymph_sema::ResolvedImportBinding::Definition(declaration.definition.clone()),
					) {
						references.extend(written_spans.into_iter().map(|span| {
							(
								span,
								nymph_sema::ImportReferenceTarget::Definition(declaration.definition.clone()),
							)
						}));
					}
				}
				Some(declaration) => {
					diagnostic_causes.push(ImportDiagnosticCause::PrivateName {
						declaration: declaration.clone(),
						span,
					});
				}
				None => diagnostic_causes.push(ImportDiagnosticCause::MissingName { name: source, span }),
			}
		}
	}
	Arc::new(ResolvedModuleImports {
		bindings,
		namespaces,
		references,
		diagnostics: diagnostic_causes
			.into_iter()
			.map(|cause| cause.render(&owner))
			.collect::<Vec<_>>()
			.into(),
	})
}

impl SemanticModuleInput {
	pub(crate) fn domain(self, db: &dyn Db) -> SemanticModuleDomain {
		match self {
			Self::Project(_) => SemanticModuleDomain::Project,
			Self::Builtin(module) => match module.key(db).domain {
				BuiltinModuleDomain::ImportableStd => SemanticModuleDomain::ImportableStd,
				BuiltinModuleDomain::AmbientCore => SemanticModuleDomain::AmbientCore,
			},
		}
	}

	pub(crate) fn display_key(self, db: &dyn Db) -> String {
		match self {
			Self::Project(module) if module.package(db).is_root() => module.path(db).to_string(),
			Self::Project(module) => {
				format!(
					"package::{}::{}",
					module.package(db).node(),
					module.path(db)
				)
			}
			Self::Builtin(module) => format!("std::{}", module.key(db).path),
		}
	}

	pub(crate) fn identity(self, db: &dyn Db) -> nymph_sema::ModuleIdentity {
		match self {
			Self::Project(module) => nymph_sema::ModuleIdentity::resolved_project(
				module.project(db).as_str(),
				module.package(db).node(),
				module.path(db).as_str(),
			),
			Self::Builtin(module) => nymph_sema::ModuleIdentity {
				origin: match module.key(db).domain {
					BuiltinModuleDomain::ImportableStd => nymph_sema::ModuleOrigin::ImportableStd,
					BuiltinModuleDomain::AmbientCore => nymph_sema::ModuleOrigin::Compiler,
				},
				project: "compiler".into(),
				path: module.key(db).path.as_ref().into(),
			},
		}
	}

	pub(crate) fn parsed(self, db: &dyn Db) -> Arc<ParsedModule> {
		match self {
			Self::Project(module) => parse(db, module).clone(),
			Self::Builtin(module) => parse_builtin(db, module).clone(),
		}
	}

	pub(crate) fn imports(self, db: &dyn Db) -> Arc<DirectImports> {
		match self {
			Self::Project(module) => direct_imports(db, module).clone(),
			Self::Builtin(module) => builtin_direct_imports(db, module).clone(),
		}
	}

	#[cfg(test)]
	fn is_ambient_core(self, db: &dyn Db) -> bool {
		self.domain(db) == SemanticModuleDomain::AmbientCore
	}
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectGraph {
	pub order: Arc<[ModuleInput]>,
	#[allow(dead_code)]
	pub direct: Arc<[(ModuleInput, Arc<[ModuleInput]>)]>,
	pub(crate) semantic_order: Arc<[SemanticModuleInput]>,
	pub(crate) semantic_direct: Arc<[(SemanticModuleInput, Arc<[SemanticModuleInput]>)]>,
	pub diagnostics: Arc<[ProjectDiagnostic]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectDependencyGraph {
	direct: Arc<[(ModulePath, Arc<[ModulePath]>)]>,
}

impl ProjectDependencyGraph {
	pub(crate) fn reverse_importer_closure(&self, module: ModulePath) -> Vec<ModulePath> {
		use std::collections::{BTreeMap, BTreeSet, VecDeque};

		let mut seen = BTreeSet::from([module.clone()]);
		let mut pending = VecDeque::from([module.clone()]);
		while let Some(dependency) = pending.pop_front() {
			for (importer, imports) in self.direct.iter() {
				if imports.contains(&dependency) && seen.insert(importer.clone()) {
					pending.push_back(importer.clone());
				}
			}
		}

		let mut forward: BTreeMap<ModulePath, Vec<ModulePath>> = seen
			.iter()
			.cloned()
			.map(|module| (module, Vec::new()))
			.collect();
		let mut reverse = forward.clone();
		for (importer, imports) in self.direct.iter().filter(|(owner, _)| seen.contains(owner)) {
			for dependency in imports
				.iter()
				.filter(|dependency| seen.contains(*dependency))
			{
				forward.get_mut(dependency).unwrap().push(importer.clone());
				reverse.get_mut(importer).unwrap().push(dependency.clone());
			}
		}
		for edges in forward.values_mut().chain(reverse.values_mut()) {
			edges.sort();
		}

		fn finish_order(
			module: &ModulePath,
			graph: &BTreeMap<ModulePath, Vec<ModulePath>>,
			seen: &mut BTreeSet<ModulePath>,
			order: &mut Vec<ModulePath>,
		) {
			if !seen.insert(module.clone()) {
				return;
			}
			for next in &graph[module] {
				finish_order(next, graph, seen, order);
			}
			order.push(module.clone());
		}

		fn collect_component(
			module: &ModulePath,
			graph: &BTreeMap<ModulePath, Vec<ModulePath>>,
			seen: &mut BTreeSet<ModulePath>,
			component: &mut Vec<ModulePath>,
		) {
			if !seen.insert(module.clone()) {
				return;
			}
			component.push(module.clone());
			for next in &graph[module] {
				collect_component(next, graph, seen, component);
			}
		}

		let mut finished = Vec::new();
		let mut visited = BTreeSet::new();
		for current in forward.keys() {
			finish_order(current, &forward, &mut visited, &mut finished);
		}
		let mut components = Vec::<Vec<ModulePath>>::new();
		visited.clear();
		for current in finished.iter().rev() {
			if visited.contains(current) {
				continue;
			}
			let mut component = Vec::new();
			collect_component(current, &reverse, &mut visited, &mut component);
			component.sort();
			components.push(component);
		}
		let component_of: BTreeMap<ModulePath, usize> = components
			.iter()
			.enumerate()
			.flat_map(|(index, component)| component.iter().cloned().map(move |module| (module, index)))
			.collect();
		let mut outgoing = vec![BTreeSet::new(); components.len()];
		let mut indegree = vec![0usize; components.len()];
		for (dependency, importers) in &forward {
			let dependency = component_of[dependency];
			for importer in importers {
				let importer = component_of[importer];
				if dependency != importer && outgoing[dependency].insert(importer) {
					indegree[importer] += 1;
				}
			}
		}

		let mut available: BTreeSet<(ModulePath, usize)> = components
			.iter()
			.enumerate()
			.filter(|(index, _)| indegree[*index] == 0)
			.map(|(index, component)| (component[0].clone(), index))
			.collect();
		let mut depth = vec![0usize; components.len()];
		let mut topological = Vec::with_capacity(components.len());
		while let Some((_, component)) = available.pop_first() {
			topological.push(component);
			for importer in &outgoing[component] {
				depth[*importer] = depth[*importer].max(depth[component] + 1);
				indegree[*importer] -= 1;
				if indegree[*importer] == 0 {
					available.insert((components[*importer][0].clone(), *importer));
				}
			}
		}
		topological.sort_by(|left, right| {
			depth[*left]
				.cmp(&depth[*right])
				.then_with(|| components[*left][0].cmp(&components[*right][0]))
		});
		let mut ordered = Vec::with_capacity(seen.len());
		for component in topological {
			let mut members = components[component].clone();
			if let Some(changed) = members.iter().position(|member| member == &module) {
				members.swap(0, changed);
			}
			ordered.extend(members);
		}
		ordered
	}
}

/// Stable project-wide import relations derived from tracked parsed imports.
///
/// Unlike [`project_graph`], this index is not scoped to one entry module, so
/// long-lived tooling can discover reverse importers without rebuilding a
/// parallel graph outside the shared Salsa database.
#[salsa::tracked(returns(clone))]
pub(crate) fn project_dependency_graph(
	db: &dyn Db,
	project: ProjectInput,
) -> Arc<ProjectDependencyGraph> {
	use std::collections::BTreeMap;

	let active: BTreeMap<ModulePath, ModuleInput> = project
		.active_modules(db)
		.iter()
		.filter(|module| module.package(db).is_root())
		.map(|module| (module.path(db), *module))
		.collect();
	let direct = active
		.iter()
		.map(|(owner, module)| {
			let mut dependencies = Vec::new();
			for import in direct_imports(db, *module).iter() {
				let Ok(target) = &import.target else {
					continue;
				};
				let Some(target) = target.current_package_path() else {
					continue;
				};
				let Ok(target) = ModulePath::new(target) else {
					continue;
				};
				if active.contains_key(&target) && !dependencies.contains(&target) {
					dependencies.push(target);
				}
			}
			(owner.clone(), Arc::from(dependencies))
		})
		.collect::<Vec<_>>()
		.into();
	Arc::new(ProjectDependencyGraph { direct })
}

impl ProjectGraph {
	pub(crate) fn direct_dependencies(&self, module: ModuleInput) -> Arc<[ModuleInput]> {
		self
			.direct
			.iter()
			.find_map(|(owner, dependencies)| (*owner == module).then(|| dependencies.clone()))
			.unwrap_or_else(|| Arc::new([]))
	}

	pub(crate) fn reverse_importers(&self, module: ModuleInput) -> Arc<[ModuleInput]> {
		self
			.direct
			.iter()
			.filter_map(|(importer, dependencies)| dependencies.contains(&module).then_some(*importer))
			.collect::<Vec<_>>()
			.into()
	}

	/// Deterministic project symbol tags shared by semantic query roots.
	/// This is graph data rather than a semantic-analysis or symbol-map query.
	pub(crate) fn semantic_module_tags(
		&self,
		db: &dyn Db,
	) -> FxHashMap<nymph_sema::ModuleIdentity, usize> {
		self
			.semantic_order
			.iter()
			.copied()
			.enumerate()
			.map(|(tag, module)| (module.identity(db), tag))
			.collect()
	}

	pub(crate) fn semantic_direct_dependencies(
		&self,
		module: SemanticModuleInput,
	) -> Arc<[SemanticModuleInput]> {
		self
			.semantic_direct
			.iter()
			.find_map(|(owner, dependencies)| (*owner == module).then(|| dependencies.clone()))
			.unwrap_or_else(|| Arc::new([]))
	}

	pub(crate) fn semantic_direct_imports(
		&self,
		db: &dyn Db,
		module: SemanticModuleInput,
	) -> Arc<DirectImports> {
		// A validated graph guarantees each successful import has a matching
		// direct semantic edge. Keep the binding's namespace and `with` aliases
		// in source order for the interface environment query.
		if self
			.semantic_direct
			.iter()
			.any(|(owner, _)| *owner == module)
		{
			module.imports(db)
		} else {
			Arc::new([])
		}
	}

	pub(crate) fn semantic_closure(&self, root: SemanticModuleInput) -> Arc<[SemanticModuleInput]> {
		use std::collections::HashSet;
		fn visit(
			graph: &ProjectGraph,
			module: SemanticModuleInput,
			seen: &mut HashSet<SemanticModuleInput>,
		) {
			for dependency in graph.semantic_direct_dependencies(module).iter().copied() {
				if seen.insert(dependency) {
					visit(graph, dependency, seen);
				}
			}
		}
		let mut seen = HashSet::new();
		visit(self, root, &mut seen);
		self
			.semantic_order
			.iter()
			.copied()
			.filter(|module| seen.contains(module))
			.collect::<Vec<_>>()
			.into()
	}
}

#[salsa::tracked]
pub(crate) fn parse(db: &dyn Db, module: ModuleInput) -> Arc<ParsedModule> {
	parse_source(
		module.source(db).unwrap_or_default(),
		format!("{}.nym", module.path(db)),
	)
}

#[salsa::tracked]
pub(crate) fn parse_builtin(db: &dyn Db, module: BuiltinModuleInput) -> Arc<ParsedModule> {
	let key = module.key(db);
	let source = module.source(db);
	match key.domain {
		BuiltinModuleDomain::ImportableStd => parse_source(source, format!("std::{}.nym", key.path)),
		BuiltinModuleDomain::AmbientCore => {
			let path = format!("core::{}.nym", key.path);
			if let Some((index, canonical)) = crate::prelude::core_source(&key.path)
				&& source.as_ref() == canonical
			{
				static PARSED: [std::sync::OnceLock<Arc<ParsedModule>>; crate::prelude::CORE_SOURCE_COUNT] =
					[const { std::sync::OnceLock::new() }; crate::prelude::CORE_SOURCE_COUNT];
				return PARSED[index]
					.get_or_init(|| parse_source(Arc::from(canonical), path))
					.clone();
			}
			parse_source(source, path)
		}
	}
}

fn parse_source(source: Arc<str>, path: String) -> Arc<ParsedModule> {
	#[cfg(feature = "test-support")]
	let _timing = super::benchmark_support::phase(super::benchmark_support::Phase::Parse);
	let parsed = nymph_syntax::parse_module(&source, path);
	Arc::new(ParsedModule {
		tree: parsed.tree,
		diagnostics: parsed.diagnostics.into(),
	})
}

#[salsa::tracked(returns(clone))]
pub(crate) fn tooling_top_level_declarations(
	db: &dyn Db,
	module: ModuleInput,
) -> Arc<[nymph_sema::TopLevelDeclaration]> {
	nymph_sema::top_level_declarations(
		SemanticModuleInput::Project(module).identity(db),
		&parse(db, module).tree,
	)
	.into()
}

#[salsa::tracked]
pub(crate) fn direct_imports(db: &dyn Db, module: ModuleInput) -> Arc<DirectImports> {
	collect_imports(parse(db, module), module.path(db).as_str())
}

#[salsa::tracked]
pub(crate) fn builtin_direct_imports(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<DirectImports> {
	collect_imports(
		parse_builtin(db, module),
		&format!("std::{}", module.key(db).path),
	)
}

#[salsa::tracked]
pub(crate) fn ambient_core_direct_imports(
	db: &dyn Db,
	module: BuiltinModuleInput,
) -> Arc<DirectImports> {
	collect_imports(parse_builtin(db, module), &module.key(db).path)
}

#[salsa::tracked(returns(clone))]
fn ambient_core_graph(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	root: BuiltinModuleInput,
) -> Arc<AmbientCoreGraph> {
	#[derive(Clone, Copy, PartialEq, Eq)]
	enum Mark {
		Visiting,
		Visited,
	}
	fn visit(
		db: &dyn Db,
		module: BuiltinModuleInput,
		modules: &std::collections::BTreeMap<Arc<str>, BuiltinModuleInput>,
		marks: &mut std::collections::BTreeMap<Arc<str>, Mark>,
		stack: &mut Vec<Arc<str>>,
		order: &mut Vec<BuiltinModuleInput>,
		diagnostics: &mut Vec<Diagnostic>,
	) -> bool {
		let path = module.key(db).path;
		match marks.get(&path) {
			Some(Mark::Visited) => return true,
			Some(Mark::Visiting) => {
				let start = stack.iter().position(|item| item == &path).unwrap_or(0);
				let mut cycle = stack[start..].to_vec();
				cycle.push(path.clone());
				diagnostics.push(Diagnostic::error(
					"CORE-IMPORT-CYCLE".into(),
					format!("ambient core import cycle: {}", cycle.join(" -> ")),
					Span::new(0, 0),
				));
				return false;
			}
			None => {}
		}
		marks.insert(path.clone(), Mark::Visiting);
		stack.push(path.clone());
		let mut valid = true;
		for import in ambient_core_direct_imports(db, module).iter() {
			if let Ok(target) = &import.target {
				if let Some(child) = target
					.current_package_path()
					.and_then(|path| modules.get(path))
				{
					valid &= visit(db, *child, modules, marks, stack, order, diagnostics);
				} else {
					diagnostics.push(Diagnostic::error(
						"CORE-IMPORT-UNRESOLVED".into(),
						format!("ambient core `{path}` imports missing module `{target}`"),
						import.span,
					));
					valid = false;
				}
			}
		}
		stack.pop();
		marks.insert(path, Mark::Visited);
		if valid {
			order.push(module);
		}
		valid
	}
	let modules = registry
		.modules(db)
		.iter()
		.map(|module| (module.key(db).path, *module))
		.collect();
	let mut marks = std::collections::BTreeMap::new();
	let mut stack = Vec::new();
	let mut order = Vec::new();
	let mut diagnostics = Vec::new();
	visit(
		db,
		root,
		&modules,
		&mut marks,
		&mut stack,
		&mut order,
		&mut diagnostics,
	);
	Arc::new(AmbientCoreGraph {
		order: order.into(),
		diagnostics: diagnostics.into(),
	})
}

#[derive(Clone, Debug, PartialEq)]
struct AmbientCoreGraph {
	order: Arc<[BuiltinModuleInput]>,
	diagnostics: Arc<[Diagnostic]>,
}

#[salsa::tracked(no_eq)]
pub(crate) fn ambient_core_analysis(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<super::session::ModuleAnalysis> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute(
		"ambient_core_analysis",
		SemanticModuleInput::Builtin(module),
	);
	let graph = ambient_core_graph(db, registry, module);
	let dependencies = graph
		.order
		.iter()
		.copied()
		.filter(|input| *input != module)
		.map(|input| ambient_core_environment(db, registry, input))
		.collect::<Vec<_>>();
	let parsed = parse_builtin(db, module);
	let semantic_module = Arc::new(parsed.tree.clone());
	let result = if graph.diagnostics.is_empty() {
		let mut environment = nymph_sema::SemanticEnvironment::from_modules_with_runtime_roles(
			ambient_identity(db, module),
			&dependencies,
			(*compiler_runtime_roles(db, registry)).clone(),
		)
		.expect("ambient dependency interfaces have deterministic identities");
		let mut bindings = FxHashMap::default();
		for import in ambient_core_direct_imports(db, module).iter() {
			let Ok(target) = &import.target else { continue };
			let Some(dependency) = graph.order.iter().copied().find(|input| {
				target
					.current_package_path()
					.is_some_and(|path| input.key(db).path.as_ref() == path)
			}) else {
				continue;
			};
			let identity = ambient_identity(db, dependency);
			bindings.insert(
				import.namespace.0.clone(),
				nymph_sema::ResolvedImportBinding::Namespace(identity.clone()),
			);
			if let Some(exports) = environment.module_exports.get(&identity) {
				for (source, alias) in &import.with_idents {
					let local = alias.as_ref().unwrap_or(source).0.clone();
					let binding = exports
						.by_name
						.get(&source.0)
						.cloned()
						.map(nymph_sema::ResolvedImportBinding::Definition)
						.unwrap_or(nymph_sema::ResolvedImportBinding::Poison);
					bindings.insert(local, binding);
				}
			}
		}
		environment.set_resolved_imports(bindings);
		nymph_sema::check_module_with_owned_environment(
			semantic_module.clone(),
			environment,
			nymph_sema::EntryMode::Library,
		)
	} else {
		let environment = nymph_sema::SemanticEnvironment::from_modules_with_runtime_roles(
			ambient_identity(db, module),
			&[],
			(*compiler_runtime_roles(db, registry)).clone(),
		)
		.expect("empty environment is valid");
		let mut result = nymph_sema::check_module_with_owned_environment(
			semantic_module.clone(),
			environment,
			nymph_sema::EntryMode::Library,
		);
		let mut diagnostics = graph.diagnostics.to_vec();
		diagnostics.extend(result.diagnostics.iter().cloned());
		result.diagnostics = diagnostics.into();
		result
	};
	Arc::new(super::session::ModuleAnalysis {
		source: module.source(db),
		semantic: result.analysis,
		diagnostics: super::session::ProjectDiagnostics(
			result
				.diagnostics
				.iter()
				.cloned()
				.map(|diag| ProjectDiagnostic {
					module: module.key(db).path.to_string(),
					diag,
				})
				.collect::<Vec<_>>()
				.into(),
		),
	})
}

fn ambient_identity(db: &dyn Db, module: BuiltinModuleInput) -> nymph_sema::ModuleIdentity {
	nymph_sema::ModuleIdentity {
		origin: nymph_sema::ModuleOrigin::Compiler,
		project: "compiler".into(),
		path: module.key(db).path.as_ref().into(),
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_headers(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<nymph_sema::DeclaredHeaders> {
	let own = nymph_sema::declared_headers(
		ambient_identity(db, module),
		&parse_builtin(db, module).tree,
	);
	let dependency_definitions = ambient_core_graph(db, registry, module)
		.order
		.iter()
		.copied()
		.filter(|dependency| *dependency != module)
		.flat_map(|dependency| {
			nymph_sema::declared_headers(
				ambient_identity(db, dependency),
				&parse_builtin(db, dependency).tree,
			)
			.definitions
		})
		.collect();
	Arc::new(own.with_checked_definitions(dependency_definitions))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_environment(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<nymph_sema::ModuleEnvironment> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute(
		"ambient_core_environment",
		SemanticModuleInput::Builtin(module),
	);
	let analysis = ambient_core_analysis(db, registry, module);
	let checked = checked_from_analysis(analysis, []);
	let facts =
		nymph_sema::ExtractionFactSelection::current_module(&analysis.semantic.module, &checked);
	Arc::new(nymph_sema::recover_module_environment_with_facts(
		ambient_identity(db, module),
		&analysis.semantic.module,
		&checked,
		&ambient_core_headers(db, registry, module),
		&facts,
	))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_interface(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<nymph_sema::ModuleInterface> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute(
		"ambient_core_interface",
		SemanticModuleInput::Builtin(module),
	);
	match &*ambient_core_environment(db, registry, module) {
		nymph_sema::ModuleEnvironment::Complete(interface) => Arc::new(interface.clone()),
		nymph_sema::ModuleEnvironment::Recovered(_) => panic!(
			"embedded ambient core `{}` did not produce a complete interface",
			module.key(db).path
		),
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_core_diagnostics(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Arc<[nymph_diagnostics::Diagnostic]> {
	let analysis = ambient_core_analysis(db, registry, module);
	let mut diagnostics = analysis
		.diagnostics
		.0
		.iter()
		.map(|item| item.diag.clone())
		.collect::<Vec<_>>();
	if diagnostics.is_empty() {
		let facts = nymph_sema::ExtractionFactSelection::current_module_from_facts(
			&analysis.semantic.module,
			&analysis.semantic.checked,
		);
		if let Err(error) = nymph_sema::extract_module_interface_from_facts_with_selection(
			ambient_identity(db, module),
			&analysis.semantic.module,
			&analysis.semantic.checked,
			&ambient_core_headers(db, registry, module),
			&facts,
		) {
			diagnostics.push(nymph_diagnostics::Diagnostic::error(
				"INTERNAL-INTERFACE-CONVERSION".into(),
				format!("internal interface conversion failed: {error:?}"),
				Span::new(0, 0),
			));
		}
	}
	diagnostics.into()
}

#[salsa::tracked(returns(clone))]
pub(crate) fn ambient_runtime_owner_artifacts(
	db: &dyn Db,
	registry: AmbientCoreRegistryInput,
) -> Arc<[super::session::BuiltinRuntimeOwnerArtifact]> {
	use super::session::{
		AmbientCoreModuleKey, BuiltinRuntimeOwnerArtifact, BuiltinRuntimeOwnerShape,
	};
	let mut artifacts = std::collections::BTreeMap::new();
	for module in registry.modules(db).iter().copied() {
		let environment = ambient_core_environment(db, registry, module);
		let nymph_sema::ModuleEnvironment::Complete(interface) = &*environment else {
			continue;
		};
		let key = AmbientCoreModuleKey::new(module.key(db).path.as_ref())
			.expect("embedded core paths are canonical");
		for definition in interface.exports.iter().chain(
			interface
				.support_definitions
				.iter()
				.map(|item| &item.definition),
		) {
			if let Some(owner) = &definition.runtime_owner {
				artifacts.insert(
					owner.clone(),
					BuiltinRuntimeOwnerArtifact {
						definition: owner.clone(),
						module: key.clone(),
						shape: BuiltinRuntimeOwnerShape::Definition(definition.clone()),
					},
				);
			}
		}
		for implementation in &interface.implementations {
			if let Some(owner) = &implementation.runtime_owner {
				artifacts.insert(
					owner.clone(),
					BuiltinRuntimeOwnerArtifact {
						definition: owner.clone(),
						module: key.clone(),
						shape: BuiltinRuntimeOwnerShape::Implementation(implementation.clone()),
					},
				);
			}
		}
	}
	artifacts.into_values().collect::<Vec<_>>().into()
}

fn collect_imports(parsed: &ParsedModule, importer: &str) -> Arc<DirectImports> {
	let mut imports = Vec::new();
	for declaration in &parsed.tree.members {
		if let Declaration::Import {
			root,
			path,
			alias,
			idents,
		} = declaration
		{
			let span = alias
				.as_ref()
				.map(|item| item.1)
				.or_else(|| path.last().map(|item| item.1))
				.unwrap_or(Span::new(0, 0));
			let target = resolve_import_target(root, path, importer, span);
			let namespace = alias
				.clone()
				.or_else(|| path.last().cloned())
				.unwrap_or_else(|| nymph_ast::Spanned("".into(), span));
			imports.push(DirectImport {
				target,
				span,
				namespace,
				has_with_list: idents.is_some(),
				with_idents: idents.clone().unwrap_or_default(),
			});
		}
	}
	imports.into()
}

fn checked_from_analysis(
	analysis: &super::session::ModuleAnalysis,
	diagnostics: impl IntoIterator<Item = Diagnostic>,
) -> nymph_sema::Checked {
	nymph_sema::Checked {
		diags: diagnostics.into_iter().collect(),
		facts: analysis.semantic.checked.as_ref().clone(),
	}
}

#[salsa::tracked(returns(clone))]
fn ambient_runtime_manifest<'db>(
	db: &'db dyn Db,
	registry: AmbientCoreRegistryInput,
	module: BuiltinModuleInput,
) -> Result<RuntimeManifest<'db>, super::session::RuntimeDefinitionError> {
	let environment = ambient_core_environment(db, registry, module);
	let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		return Err(super::session::RuntimeDefinitionError::Recovered);
	};
	let analysis = ambient_core_analysis(db, registry, module);
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute(
		"runtime_definition_extraction",
		SemanticModuleInput::Builtin(module),
	);
	let definitions = nymph_sema::runtime_definitions(
		&analysis.semantic.module,
		&analysis.semantic.checked,
		interface,
	)
	.map_err(super::session::RuntimeDefinitionError::Extraction)?;
	RuntimeManifest::new(db, definitions).map_err(super::session::RuntimeDefinitionError::Extraction)
}

fn exact_module_environment<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<nymph_sema::ModuleEnvironment> {
	match module {
		SemanticModuleInput::Builtin(input)
			if input.key(db).domain == BuiltinModuleDomain::AmbientCore =>
		{
			ambient_core_environment(db, key.ambient_core_registry(db), input)
		}
		_ => interface_module_environment(db, key, module),
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn runtime_manifest<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<RuntimeManifest<'db>, super::session::RuntimeDefinitionError> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("runtime_manifest", module);
	if let SemanticModuleInput::Builtin(input) = module
		&& input.key(db).domain == BuiltinModuleDomain::AmbientCore
	{
		return ambient_runtime_manifest(db, key.ambient_core_registry(db), input);
	}
	let environment = interface_module_environment(db, key, module);
	let nymph_sema::ModuleEnvironment::Complete(interface) = environment.as_ref() else {
		return Err(super::session::RuntimeDefinitionError::Recovered);
	};
	let analysis = interface_module_analysis(db, key, module);
	match module {
		SemanticModuleInput::Project(input) => match input.source(db) {
			Some(_) => {}
			None => return Err(super::session::RuntimeDefinitionError::OwnerNotFound),
		},
		SemanticModuleInput::Builtin(_) => {}
	};
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("runtime_definition_extraction", module);
	let definitions = nymph_sema::runtime_definitions(
		&analysis.semantic.module,
		&analysis.semantic.checked,
		interface,
	)
	.map_err(super::session::RuntimeDefinitionError::Extraction)?;
	RuntimeManifest::new(db, definitions).map_err(super::session::RuntimeDefinitionError::Extraction)
}

/// Runtime-bearing identities owned by `module`, in language output order.
/// This is intentionally distinct from lookup: module assembly depends on the
/// identities, then requests each body through its exact per-definition query.
#[salsa::tracked(returns(clone))]
pub(crate) fn runtime_definition_ids<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<Arc<[nymph_sema::DefinitionId]>, super::session::RuntimeDefinitionError> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("runtime_definition_ids", module);
	Ok(
		runtime_manifest(db, key, module)?
			.definitions()
			.iter()
			.map(|entity| entity.definition(db).clone())
			.collect::<Vec<_>>()
			.into(),
	)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn runtime_definition<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<Arc<nymph_sema::RuntimeDefinition>, super::session::RuntimeDefinitionError> {
	#[cfg(feature = "test-support")]
	db.runtime_query_will_execute("runtime_definition", &definition);
	let module = runtime_owner(db, key, &definition)?;
	let entity = runtime_manifest(db, key, module)?
		.definition(db, &definition)
		.ok_or(super::session::RuntimeDefinitionError::DefinitionNotFound)?;
	Ok(entity.value(db))
}

#[cfg(feature = "test-support")]
#[salsa::tracked(returns(clone))]
pub(crate) fn runtime_definition_consumer<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<Arc<nymph_sema::RuntimeDefinition>, super::session::RuntimeDefinitionError> {
	db.runtime_query_will_execute("runtime_definition_consumer", &definition);
	runtime_definition(db, key, definition)
}

fn runtime_owner<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: &nymph_sema::DefinitionId,
) -> Result<SemanticModuleInput, super::session::RuntimeDefinitionError> {
	let graph = project_graph(db, key);
	let importable = key.builtin_registry(db).modules(db);
	let ambient = key.ambient_core_registry(db).modules(db);
	let owners = graph
		.semantic_order
		.iter()
		.copied()
		.chain(importable.iter().copied().map(SemanticModuleInput::Builtin))
		.chain(ambient.iter().copied().map(SemanticModuleInput::Builtin))
		.filter(|module| module.identity(db) == definition.module)
		.collect::<std::collections::HashSet<_>>();
	let mut owners = owners.into_iter();
	let Some(owner) = owners.next() else {
		return Err(super::session::RuntimeDefinitionError::OwnerNotFound);
	};
	if owners.next().is_some() {
		return Err(super::session::RuntimeDefinitionError::DuplicateOwner);
	}
	Ok(owner)
}

#[salsa::tracked(returns(clone))]
fn stable_shape_interface<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<Arc<nymph_sema::ModuleInterface>, Arc<nymph_sema::InterfaceConversionError>> {
	let (analysis, headers) = match module {
		SemanticModuleInput::Builtin(input)
			if input.key(db).domain == BuiltinModuleDomain::AmbientCore =>
		{
			(
				ambient_core_analysis(db, key.ambient_core_registry(db), input).clone(),
				ambient_core_headers(db, key.ambient_core_registry(db), input),
			)
		}
		_ => (
			interface_module_analysis(db, key, module),
			interface_declared_headers(db, key, module),
		),
	};
	let facts = nymph_sema::ExtractionFactSelection::current_module_from_facts(
		&analysis.semantic.module,
		&analysis.semantic.checked,
	)
	.including_private_definitions();
	nymph_sema::extract_module_interface_from_facts_with_selection(
		module.identity(db),
		&analysis.semantic.module,
		&analysis.semantic.checked,
		&headers,
		&facts,
	)
	.map(Arc::new)
	.map_err(Arc::new)
}

fn complete_interface<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	request: &nymph_sema::StableShapeRequest,
) -> Result<Arc<nymph_sema::ModuleInterface>, nymph_sema::StableShapeLookupError> {
	let definition = request.definition();
	let module = runtime_owner(db, key, definition).map_err(|_| {
		nymph_sema::StableShapeLookupError::Missing {
			request: request.clone(),
		}
	})?;
	let environment = exact_module_environment(db, key, module);
	if matches!(
		environment.as_ref(),
		nymph_sema::ModuleEnvironment::Recovered(_)
	) {
		return Err(nymph_sema::StableShapeLookupError::Recovered {
			definition: definition.clone(),
		});
	}
	stable_shape_interface(db, key, module).map_err(|_| {
		nymph_sema::StableShapeLookupError::Recovered {
			definition: definition.clone(),
		}
	})
}

#[salsa::tracked(returns(clone))]
pub(crate) fn stable_shape<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	request: nymph_sema::StableShapeRequest,
) -> Result<nymph_sema::StableShapeFact, nymph_sema::StableShapeLookupError> {
	#[cfg(feature = "test-support")]
	db.runtime_query_will_execute("stable_shape", request.definition());
	use nymph_sema::{StableShapeFact as Fact, StableShapeRequest as Request};
	if let Request::TypeShell(definition) = &request {
		let artifact = runtime_definition(db, key, definition.clone()).map_err(|_| {
			nymph_sema::StableShapeLookupError::Missing {
				request: request.clone(),
			}
		})?;
		return match &artifact.payload {
			nymph_sema::RuntimePayload::Struct(shell) => Ok(Fact::TypeShell(
				nymph_sema::StableTypeShell::Struct(shell.clone()),
			)),
			nymph_sema::RuntimePayload::Enum(shell) => Ok(Fact::TypeShell(
				nymph_sema::StableTypeShell::Enum(shell.clone()),
			)),
			_ => Err(nymph_sema::StableShapeLookupError::Missing { request }),
		};
	}
	let interface = complete_interface(db, key, &request)?;
	let definitions = interface.exports.iter().chain(
		interface
			.support_definitions
			.iter()
			.map(|item| &item.definition),
	);
	let result = match &request {
		Request::Definition(id) => definitions
			.clone()
			.find(|definition| definition.id == *id)
			.cloned()
			.map(Fact::Definition),
		Request::Member(id) => definitions
			.clone()
			.flat_map(|definition| &definition.members)
			.chain(
				interface
					.implementations
					.iter()
					.flat_map(|implementation| &implementation.members),
			)
			.find(|member| member.id == *id)
			.cloned()
			.map(Fact::Member),
		Request::Implementation(id) => interface
			.implementations
			.iter()
			.find(|implementation| implementation.id == *id)
			.cloned()
			.map(Fact::Implementation),
		Request::ImplementationsForInterface(id) => {
			let graph = project_graph(db, key);
			let builtins = key.builtin_registry(db).modules(db);
			let ambient = key.ambient_core_registry(db).modules(db);
			let modules = graph
				.semantic_order
				.iter()
				.copied()
				.chain(builtins.iter().copied().map(SemanticModuleInput::Builtin))
				.chain(ambient.iter().copied().map(SemanticModuleInput::Builtin));
			let mut implementations = Vec::new();
			for module in modules {
				let environment = exact_module_environment(db, key, module);
				let nymph_sema::ModuleEnvironment::Complete(candidate) = environment.as_ref() else {
					continue;
				};
				implementations.extend(
					candidate
						.implementations
						.iter()
						.filter(|implementation| implementation.interface.as_ref() == Some(id))
						.cloned(),
				);
			}
			implementations.sort_by(|left, right| left.id.cmp(&right.id));
			implementations.dedup_by(|left, right| left.id == right.id);
			Some(Fact::Implementations(implementations))
		}
		Request::InterfaceShell(id) => definitions
			.clone()
			.find(|definition| definition.id == *id)
			.cloned()
			.map(Fact::InterfaceShell),
		Request::ExternalAbi(id) => definitions
			.clone()
			.find(|definition| definition.id == *id)
			.and_then(|definition| definition.external.clone())
			.or_else(|| {
				definitions
					.clone()
					.flat_map(|definition| &definition.members)
					.chain(
						interface
							.implementations
							.iter()
							.flat_map(|implementation| &implementation.members),
					)
					.find(|member| member.id == *id)
					.and_then(|member| member.external.clone())
			})
			.map(Fact::ExternalAbi),
		Request::TypeShell(_) => unreachable!(),
	};
	result.ok_or(nymph_sema::StableShapeLookupError::Missing { request })
}

fn declaration_name(definition: &nymph_sema::DefinitionId) -> Option<&str> {
	match &definition.key {
		nymph_sema::DeclarationKey::TopLevel { name, .. }
		| nymph_sema::DeclarationKey::Member { name, .. }
		| nymph_sema::DeclarationKey::MethodBody { name, .. } => Some(name),
		nymph_sema::DeclarationKey::MaterializedInterfaceMember {
			interface_member, ..
		} => declaration_name(interface_member),
		_ => None,
	}
}

pub(crate) fn repl_module_tag(module: &nymph_sema::ModuleIdentity) -> String {
	let key = match &module.origin {
		nymph_sema::ModuleOrigin::Project(_) => module.path.to_string(),
		nymph_sema::ModuleOrigin::ResolvedPackage { node } => {
			format!("package::{node}::{}", module.path)
		}
		nymph_sema::ModuleOrigin::ImportableStd => format!("std::{}", module.path),
		nymph_sema::ModuleOrigin::Compiler => format!("compiler::{}", module.path),
	};
	key
		.as_bytes()
		.iter()
		.map(|byte| format!("{byte:02x}"))
		.collect()
}

#[salsa::tracked(returns(clone))]
pub(crate) fn binding_name<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<nymph_sema::EmittedBindingName, nymph_sema::StableNameLookupError> {
	let name = declaration_name(&definition).ok_or_else(|| {
		nymph_sema::StableNameLookupError::MissingBinding {
			definition: definition.clone(),
		}
	})?;
	let graph = project_graph(db, key);
	let project_tag = graph
		.semantic_order
		.iter()
		.position(|module| module.identity(db) == definition.module);
	let importable = key.builtin_registry(db).modules(db);
	let importable_tag = importable
		.iter()
		.position(|module| SemanticModuleInput::Builtin(*module).identity(db) == definition.module)
		.map(|index| graph.semantic_order.len() + index);
	let ambient = key.ambient_core_registry(db).modules(db);
	let ambient_tag = ambient
		.iter()
		.position(|module| ambient_identity(db, *module) == definition.module)
		.map(|index| graph.semantic_order.len() + importable.len() + index);
	let tag = project_tag
		.or(importable_tag)
		.or(ambient_tag)
		.ok_or_else(|| nymph_sema::StableNameLookupError::MissingBinding {
			definition: definition.clone(),
		})?;
	let tag = if key.mode(db) == nymph_sema::EntryMode::Repl {
		repl_module_tag(&definition.module)
	} else {
		tag.to_string()
	};
	let preserve = key.preserve_names(db) && definition.module.path == key.entry(db).as_str();
	let entry_main = key.mode(db) == nymph_sema::EntryMode::Entry
		&& definition.module.path == key.entry(db).as_str()
		&& name == "main";
	let implementation_receiver = |implementation: &nymph_sema::DefinitionId| {
		let nymph_sema::DeclarationKey::Implementation { header, .. } = &implementation.key else {
			return Ok(None);
		};
		let request = nymph_sema::StableShapeRequest::Implementation(implementation.clone());
		let interface = complete_interface(db, key, &request).map_err(|_| {
			nymph_sema::StableNameLookupError::MissingBinding {
				definition: implementation.clone(),
			}
		})?;
		let local_ordinal = interface
			.implementations
			.iter()
			.position(|shape| shape.id == *implementation)
			.ok_or_else(|| nymph_sema::StableNameLookupError::MissingBinding {
				definition: implementation.clone(),
			})?;
		let receiver = primitive_header_tag(&header.self_type).unwrap_or("impl");
		Ok(Some(format!("{receiver}$i{local_ordinal}")))
	};
	let receiver = match &definition.key {
		nymph_sema::DeclarationKey::Member { owner, .. } => match &owner.key {
			nymph_sema::DeclarationKey::Implementation { .. } => implementation_receiver(owner)?,
			_ => None,
		},
		nymph_sema::DeclarationKey::MaterializedInterfaceMember { implementation, .. } => {
			implementation_receiver(implementation)?
		}
		_ => None,
	};
	Ok(nymph_sema::EmittedBindingName::new(
		if (preserve || entry_main) && receiver.is_none() {
			name.to_owned()
		} else if let Some(receiver) = receiver {
			format!("$m{tag}${receiver}${name}")
		} else {
			format!("$m{tag}${name}")
		},
	))
}

fn primitive_header_tag(ty: &nymph_sema::HeaderType) -> Option<&'static str> {
	use nymph_sema::HeaderType;
	match ty {
		HeaderType::Int => Some("int"),
		HeaderType::UInt => Some("uint"),
		HeaderType::Float => Some("float"),
		HeaderType::Char => Some("char"),
		HeaderType::String => Some("string"),
		HeaderType::Boolean => Some("bool"),
		HeaderType::Void => Some("void"),
		HeaderType::List(_) | HeaderType::Tuple(_) => Some("list"),
		HeaderType::Map(..) => Some("map"),
		_ => None,
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn member_name<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<nymph_sema::EmittedMemberName, nymph_sema::StableNameLookupError> {
	let protocol_slots = compiler_runtime_roles(db, key.ambient_core_registry(db));
	let selected_interface_member = match &definition.key {
		nymph_sema::DeclarationKey::MaterializedInterfaceMember {
			implementation,
			interface_member,
		} => match stable_shape(
			db,
			key,
			nymph_sema::StableShapeRequest::Implementation((**implementation).clone()),
		) {
			Ok(nymph_sema::StableShapeFact::Implementation(shape))
				if shape
					.member_slots
					.target(interface_member)
					.is_some_and(|slot| {
						slot.member_id == definition
							&& slot.source == nymph_sema::ImplementationMemberSource::InheritedDefault
							&& slot.implementation_id == **implementation
							&& slot.placement_owner == **implementation
					}) =>
			{
				Some((**interface_member).clone())
			}
			_ => return Err(nymph_sema::StableNameLookupError::MissingMember { definition }),
		},
		nymph_sema::DeclarationKey::Member { owner, .. }
			if matches!(
				owner.key,
				nymph_sema::DeclarationKey::Implementation { ref header, .. }
					if header.interface.is_some()
			) =>
		{
			match stable_shape(
				db,
				key,
				nymph_sema::StableShapeRequest::Implementation((**owner).clone()),
			) {
				Ok(nymph_sema::StableShapeFact::Implementation(shape)) => shape
					.members
					.iter()
					.find(|member| member.id == definition)
					.and_then(|member| {
						let interface = shape.interface.as_ref()?;
						let Ok(nymph_sema::StableShapeFact::InterfaceShell(shell)) = stable_shape(
							db,
							key,
							nymph_sema::StableShapeRequest::InterfaceShell(interface.clone()),
						) else {
							return None;
						};
						match shell
							.members
							.iter()
							.find(|interface_member| interface_member.name == member.name)
						{
							Some(interface_member) if interface_member.kind == member.kind => shape
								.member_slots
								.iter()
								.find(|slot| slot.member_id == definition)
								.filter(|slot| slot.interface_member_id == interface_member.id)
								.map(|_| interface_member.id.clone()),
							Some(_) => None,
							None
								if shape
									.member_slots
									.iter()
									.all(|slot| slot.member_id != definition) =>
							{
								Some(definition.clone())
							}
							None => None,
						}
					}),
				_ => None,
			}
		}
		_ => Some(definition.clone()),
	};
	if let Some(selected) = selected_interface_member.as_ref() {
		if protocol_slots
			.display
			.as_ref()
			.is_some_and(|role| selected == &role.member)
		{
			return Ok(nymph_sema::EmittedMemberName::new("$nymph$display"));
		}
		if protocol_slots
			.debug
			.as_ref()
			.is_some_and(|role| selected == &role.member)
		{
			return Ok(nymph_sema::EmittedMemberName::new("$nymph$debug"));
		}
	}
	selected_interface_member
		.as_ref()
		.and_then(declaration_name)
		.map(nymph_sema::EmittedMemberName::new)
		.ok_or(nymph_sema::StableNameLookupError::MissingMember { definition })
}

#[salsa::tracked(returns(clone))]
pub(crate) fn module_specifier<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: nymph_sema::ModuleIdentity,
) -> Result<nymph_sema::CanonicalModuleSpecifier, nymph_sema::StableNameLookupError> {
	let owner = runtime_owner(
		db,
		key,
		&nymph_sema::DefinitionId::new(
			module.clone(),
			nymph_sema::DeclarationKey::top_level(nymph_sema::DeclarationCategory::Namespace, "<module>"),
		),
	)
	.map_err(|_| nymph_sema::StableNameLookupError::MissingModule {
		module: module.clone(),
	})?;
	Ok(match owner.domain(db) {
		SemanticModuleDomain::Project => {
			nymph_sema::CanonicalModuleSpecifier::Project(match module.origin {
				nymph_sema::ModuleOrigin::ResolvedPackage { node } => {
					format!("package::{node}::{}", module.path).into()
				}
				_ => module.path,
			})
		}
		SemanticModuleDomain::ImportableStd => {
			nymph_sema::CanonicalModuleSpecifier::Importable(format!("std::{}", module.path).into())
		}
		SemanticModuleDomain::AmbientCore => nymph_sema::CanonicalModuleSpecifier::CompilerRuntime(
			format!("@nymph/runtime/{}", module.path).into(),
		),
	})
}

struct CompilerStableContext<'db> {
	db: &'db dyn Db,
	key: ProjectKey<'db>,
}

impl nymph_sema::RuntimeDefinitionLookup for CompilerStableContext<'_> {
	fn runtime_definition(
		&self,
		definition: &nymph_sema::DefinitionId,
	) -> Result<Arc<nymph_sema::RuntimeDefinition>, nymph_sema::RuntimeDefinitionLookupError> {
		runtime_definition(self.db, self.key, definition.clone()).map_err(|error| match error {
			super::session::RuntimeDefinitionError::Recovered => {
				nymph_sema::RuntimeDefinitionLookupError::Recovered {
					definition: definition.clone(),
				}
			}
			super::session::RuntimeDefinitionError::DefinitionNotFound
			| super::session::RuntimeDefinitionError::OwnerNotFound => {
				nymph_sema::RuntimeDefinitionLookupError::Missing {
					definition: definition.clone(),
				}
			}
			other => nymph_sema::RuntimeDefinitionLookupError::Unavailable {
				definition: definition.clone(),
				reason: format!("{other:?}").into(),
			},
		})
	}
}

impl nymph_sema::StableShapeLookup for CompilerStableContext<'_> {
	fn stable_shape(
		&self,
		request: &nymph_sema::StableShapeRequest,
	) -> Result<nymph_sema::StableShapeFact, nymph_sema::StableShapeLookupError> {
		stable_shape(self.db, self.key, request.clone())
	}
}

impl nymph_sema::StableNameLookup for CompilerStableContext<'_> {
	fn binding_name(
		&self,
		definition: &nymph_sema::DefinitionId,
	) -> Result<nymph_sema::EmittedBindingName, nymph_sema::StableNameLookupError> {
		binding_name(self.db, self.key, definition.clone())
	}
	fn member_name(
		&self,
		definition: &nymph_sema::DefinitionId,
	) -> Result<nymph_sema::EmittedMemberName, nymph_sema::StableNameLookupError> {
		member_name(self.db, self.key, definition.clone())
	}
	fn module_specifier(
		&self,
		module: &nymph_sema::ModuleIdentity,
	) -> Result<nymph_sema::CanonicalModuleSpecifier, nymph_sema::StableNameLookupError> {
		module_specifier(self.db, self.key, module.clone())
	}
}

#[allow(clippy::result_large_err)]
#[salsa::tracked(returns(clone))]
pub(crate) fn lower_runtime_definition<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	definition: nymph_sema::DefinitionId,
) -> Result<Arc<nymph_sema::LoweredRuntimeDefinition>, nymph_sema::StableLoweringError> {
	#[cfg(feature = "test-support")]
	db.runtime_query_will_execute("lower_runtime_definition", &definition);
	let artifact = runtime_definition(db, key, definition.clone()).map_err(|error| match error {
		super::session::RuntimeDefinitionError::Recovered => {
			nymph_sema::RuntimeDefinitionLookupError::Recovered {
				definition: definition.clone(),
			}
		}
		super::session::RuntimeDefinitionError::DefinitionNotFound
		| super::session::RuntimeDefinitionError::OwnerNotFound => {
			nymph_sema::RuntimeDefinitionLookupError::Missing {
				definition: definition.clone(),
			}
		}
		other => nymph_sema::RuntimeDefinitionLookupError::Unavailable {
			definition: definition.clone(),
			reason: format!("{other:?}").into(),
		},
	})?;
	let context = CompilerStableContext { db, key };
	nymph_sema::lower_runtime_definition(&context, artifact).map(Arc::new)
}

fn collect_unresolved_runtime_calls(
	execution: &nymph_sema::RuntimeExecutionSummary,
	calls: &mut Vec<nymph_sema::UnresolvedRuntimeCall>,
) {
	calls.extend(execution.unresolved_calls().iter().cloned());
	if let Some(invocation) = execution.invocation() {
		collect_unresolved_runtime_calls(invocation, calls);
	}
	for closure in execution.closures() {
		collect_unresolved_runtime_calls(closure, calls);
	}
}

#[allow(clippy::result_large_err)]
#[salsa::tracked(returns(clone))]
pub(crate) fn lower_interface_module<'db>(
	db: &'db dyn Db,
	key: ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<Arc<nymph_sema::StableHirModule>, nymph_sema::StableModuleAssemblyError> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("lower_interface_module", module);
	#[cfg(feature = "test-support")]
	let _timing = super::benchmark_support::phase(super::benchmark_support::Phase::StableLowering);
	let graph = project_graph(db, key);
	let mut environments = vec![module];
	environments.extend(graph.semantic_closure(module).iter().copied());
	for reachable in environments {
		let environment = match reachable {
			SemanticModuleInput::Builtin(input)
				if input.key(db).domain == BuiltinModuleDomain::AmbientCore =>
			{
				ambient_core_environment(db, key.ambient_core_registry(db), input)
			}
			_ => interface_module_environment(db, key, reachable),
		};
		if matches!(
			environment.as_ref(),
			nymph_sema::ModuleEnvironment::Recovered(_)
		) {
			return Err(
				nymph_sema::StableModuleAssemblyError::RecoveredEnvironment {
					module: reachable.identity(db),
				},
			);
		}
	}

	let own = runtime_definition_ids(db, key, module).map_err(|error| match error {
		super::session::RuntimeDefinitionError::Extraction(error) => {
			nymph_sema::StableModuleAssemblyError::RuntimeExtraction(error)
		}
		_ => nymph_sema::StableModuleAssemblyError::RecoveredEnvironment {
			module: module.identity(db),
		},
	})?;
	// Interface default bodies are canonical templates, not independently
	// emitted methods. Only a demanded materialized implementation may lower
	// and attach one to a concrete runtime owner.
	let mut queue = own
		.iter()
		.filter(|definition| {
			!matches!(
				&definition.key,
				nymph_sema::DeclarationKey::Member { owner, .. }
					if matches!(
						owner.key,
						nymph_sema::DeclarationKey::TopLevel {
							category: nymph_sema::DeclarationCategory::Interface,
							..
						}
					)
			)
		})
		.cloned()
		.collect::<std::collections::VecDeque<_>>();
	let mut seen = std::collections::HashSet::new();
	let mut lowered: Vec<nymph_sema::LoweredRuntimeDefinition> = Vec::new();
	let mut unresolved_calls = Vec::new();
	while let Some(definition) = queue.pop_front() {
		if !seen.insert(definition.clone()) {
			continue;
		}
		let fragment =
			lower_runtime_definition(db, key, definition.clone()).map_err(|error| match &error {
				nymph_sema::StableLoweringError::Runtime(
					nymph_sema::RuntimeDefinitionLookupError::Missing { .. },
				) => nymph_sema::StableModuleAssemblyError::UnresolvedDemand {
					definition: definition.clone(),
				},
				nymph_sema::StableLoweringError::Runtime(
					nymph_sema::RuntimeDefinitionLookupError::Recovered { .. },
				) => nymph_sema::StableModuleAssemblyError::RecoveredDemand {
					definition: definition.clone(),
				},
				_ => error.into(),
			})?;
		if let nymph_sema::LoweredHirFragment::TopLevelExternal { abi, .. } = fragment.fragment()
			&& let Some((module, _)) = abi.linked()
		{
			for role in
				crate::host_runtime::HostRuntimeGraph::compiler_facts().semantic_dependencies(module)
			{
				let dependency = match role {
					crate::host_runtime::CompilerRuntimeRole::Option => {
						compiler_runtime_roles(db, key.ambient_core_registry(db))
							.option
							.as_ref()
							.map(|role| role.option.clone())
					}
				}
				.ok_or_else(|| nymph_sema::StableModuleAssemblyError::UnresolvedDemand {
					definition: definition.clone(),
				})?;
				queue.push_back(dependency);
			}
		}
		queue.extend(fragment.demands().iter().cloned());
		// A generic implementation may call a bound member whose concrete owner is
		// known only from the demand closure. Resolve those calls against every
		// demanded shell; the shell and call may enter the queue in either order.
		collect_unresolved_runtime_calls(fragment.execution_summary(), &mut unresolved_calls);
		for call in &unresolved_calls {
			let (interface, member) = match call {
				nymph_sema::UnresolvedRuntimeCall::GenericDispatch { interface, member }
				| nymph_sema::UnresolvedRuntimeCall::IteratorNext { interface, member } => (interface, member),
				_ => continue,
			};
			let request = nymph_sema::StableShapeRequest::ImplementationsForInterface(interface.clone());
			let Ok(nymph_sema::StableShapeFact::Implementations(implementations)) =
				stable_shape(db, key, request)
			else {
				continue;
			};
			for implementation in implementations {
				let owner_is_demanded = matches!(
					&implementation.self_type,
					nymph_sema::InterfaceType::Named { definition: owner, .. }
						if seen.contains(owner)
				);
				if owner_is_demanded && let Some(slot) = implementation.member_slots.target(member) {
					queue.push_back(slot.member_id.clone());
				}
			}
		}
		lowered.push(fragment.as_ref().clone());
	}

	let target = module.identity(db);
	for fragment in &lowered {
		super::assembly::validate_fragment_intrinsic(fragment).map_err(map_runtime_assembly_error)?;
	}
	for fragment in &lowered {
		if own.contains(fragment.definition())
			&& let nymph_sema::RuntimeAssemblyPlacement::Shell(owner) = fragment.placement()
			&& owner.module != target
			&& !matches!(owner.module.origin, nymph_sema::ModuleOrigin::Compiler)
		{
			return Err(nymph_sema::StableModuleAssemblyError::MismatchedPlacement {
				definition: fragment.definition().clone(),
				owner: owner.clone(),
			});
		}
	}
	let local_fragments = lowered
		.iter()
		.filter(|fragment| match fragment.placement() {
			nymph_sema::RuntimeAssemblyPlacement::Module(owner) => owner == &target,
			nymph_sema::RuntimeAssemblyPlacement::Shell(owner) => owner.module == target,
			nymph_sema::RuntimeAssemblyPlacement::Template => true,
		})
		.collect::<Vec<_>>();
	let execution_bodies = lowered.iter();
	let hir = super::assembly::assemble_runtime_module_with_execution(
		&target,
		local_fragments
			.into_iter()
			.map(|fragment| (fragment.definition().clone(), fragment)),
		execution_bodies,
	)
	.map_err(map_runtime_assembly_error)?;
	let imports = lowered
		.iter()
		.filter(|item| {
			item.definition().module != module.identity(db)
				&& !matches!(
					item.definition().module.origin,
					nymph_sema::ModuleOrigin::Compiler
				)
		})
		.map(|item| item.definition().clone())
		.collect();
	let virtual_runtime = lowered
		.iter()
		.filter_map(|item| {
			let owner = match item.placement() {
				nymph_sema::RuntimeAssemblyPlacement::Module(owner) => owner.clone(),
				nymph_sema::RuntimeAssemblyPlacement::Shell(shell) => shell.module.clone(),
				nymph_sema::RuntimeAssemblyPlacement::Template => return None,
			};
			matches!(owner.origin, nymph_sema::ModuleOrigin::Compiler).then_some((item, owner))
		})
		.map(|(item, owner)| nymph_sema::VirtualRuntimeFragment {
			owner,
			definition: item.definition().clone(),
			fragment: item.clone(),
		})
		.collect();
	Ok(Arc::new(nymph_sema::StableHirModule {
		module: module.identity(db),
		hir,
		own_definitions: own.to_vec(),
		fragments: lowered,
		imports,
		virtual_runtime,
	}))
}

fn map_runtime_assembly_error(
	error: super::assembly::RuntimeAssemblyError,
) -> nymph_sema::StableModuleAssemblyError {
	use super::assembly::RuntimeAssemblyError as Error;
	match error {
		Error::DuplicateAttachment { owner, name } => {
			nymph_sema::StableModuleAssemblyError::DuplicateAttachment { owner, name }
		}
		Error::DuplicateRuntimeTypeAttachment { object, name } => {
			nymph_sema::StableModuleAssemblyError::DuplicateRuntimeTypeAttachment { object, name }
		}
		Error::MissingOwnerShell { owner } => {
			nymph_sema::StableModuleAssemblyError::MissingOwnerShell { owner }
		}
		Error::MismatchedShell {
			definition,
			placement_owner,
			..
		} => nymph_sema::StableModuleAssemblyError::MismatchedPlacement {
			definition,
			owner: placement_owner,
		},
		Error::DefinitionMismatch { supplied, lowered } => {
			nymph_sema::StableModuleAssemblyError::MismatchedPlacement {
				definition: supplied,
				owner: lowered,
			}
		}
		Error::DuplicateShell { owner } | Error::Template { definition: owner } => {
			nymph_sema::StableModuleAssemblyError::MismatchedPlacement {
				definition: owner.clone(),
				owner,
			}
		}
		Error::MismatchedModule { definition, .. } => {
			nymph_sema::StableModuleAssemblyError::MismatchedPlacement {
				owner: definition.clone(),
				definition,
			}
		}
		Error::MissingExecutionBody { caller, callee } => {
			nymph_sema::StableModuleAssemblyError::MissingExecutionBody { caller, callee }
		}
		Error::InitializerCycle { cycle } => {
			nymph_sema::StableModuleAssemblyError::InitializerCycle { cycle }
		}
		Error::UnresolvedInitializerCall {
			initializer,
			body,
			call,
		} => nymph_sema::StableModuleAssemblyError::UnresolvedInitializerCall {
			initializer,
			body,
			call,
		},
	}
}

/// Check one module exclusively from its own tree and dependency interfaces.
#[salsa::tracked(returns(clone), no_eq)]
pub(crate) fn interface_module_analysis<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<super::session::ModuleAnalysis> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("interface_module_analysis", module);
	let graph = project_graph(db, key);
	let dependencies = graph
		.semantic_closure(module)
		.iter()
		.copied()
		.filter(|dependency| {
			!key.isolated(db) || dependency.domain(db) != SemanticModuleDomain::Project
		})
		.map(|dependency| interface_module_environment(db, key, dependency))
		.collect::<Vec<_>>();
	let mut roots = Vec::new();
	if key.ambient_prelude(db) {
		let registry = key.ambient_core_registry(db);
		roots.extend(
			registry
				.modules(db)
				.iter()
				.copied()
				.map(|root| ambient_core_environment(db, registry, root)),
		);
	}
	roots.extend(dependencies);
	let parsed = module.parsed(db);
	#[cfg(feature = "test-support")]
	let environment_timing =
		super::benchmark_support::phase(super::benchmark_support::Phase::Environment);
	let mut environment = nymph_sema::SemanticEnvironment::from_modules_with_runtime_roles(
		module.identity(db),
		&roots,
		(*compiler_runtime_roles(db, key.ambient_core_registry(db))).clone(),
	)
	.expect("validated interfaces form a deterministic semantic environment");
	environment.set_diagnostic_module_tags(&graph.semantic_module_tags(db));
	let mut bindings = FxHashMap::default();
	if key.ambient_prelude(db) {
		let registry = key.ambient_core_registry(db);
		for root in registry.modules(db).iter().copied() {
			let identity = SemanticModuleInput::Builtin(root).identity(db);
			if let Some(exports) = environment.module_exports.get(&identity) {
				for (name, stable) in &exports.by_name {
					bindings.insert(
						name.clone(),
						nymph_sema::ResolvedImportBinding::Definition(stable.clone()),
					);
				}
			}
		}
	}
	let resolved = resolved_module_imports(db, key, module);
	bindings.extend(
		resolved
			.bindings
			.iter()
			.map(|(name, binding)| (name.clone(), binding.clone())),
	);
	environment.set_resolved_imports(bindings);
	#[cfg(feature = "test-support")]
	drop(environment_timing);
	#[cfg(feature = "test-support")]
	let checker_timing = super::benchmark_support::phase(super::benchmark_support::Phase::Checker);
	let result = nymph_sema::check_module_with_owned_environment(
		Arc::new(parsed.tree.clone()),
		environment,
		match key.mode(db) {
			nymph_sema::EntryMode::Entry if module.display_key(db) == key.entry(db).as_str() => {
				nymph_sema::EntryMode::Entry
			}
			nymph_sema::EntryMode::Repl => nymph_sema::EntryMode::Repl,
			nymph_sema::EntryMode::Library | nymph_sema::EntryMode::Entry => {
				nymph_sema::EntryMode::Library
			}
		},
	);
	#[cfg(feature = "test-support")]
	drop(checker_timing);
	#[cfg(feature = "test-support")]
	let _reporting_timing =
		super::benchmark_support::phase(super::benchmark_support::Phase::DiagnosticReporting);
	let qualified_access_causes = result
		.analysis
		.annotations
		.unresolved_qualified_accesses()
		.iter()
		.filter_map(|access| {
			let dependency = graph
				.semantic_closure(module)
				.iter()
				.copied()
				.find(|dependency| dependency.identity(db) == access.module)?;
			let summary = namespace_summary(db, key, dependency);
			Some(match summary.declaration(&access.member) {
				Some(declaration)
					if !declaration
						.visibility
						.allows(&summary.module, &module.identity(db)) =>
				{
					ImportDiagnosticCause::PrivateNamespaceMember {
						declaration: declaration.clone(),
						span: access.span,
					}
				}
				_ => ImportDiagnosticCause::MissingNamespaceMember {
					member: access.member.clone(),
					span: access.span,
				},
			})
		})
		.collect::<Vec<_>>();
	let mut diagnostics = resolved.diagnostics.iter().cloned().collect::<Vec<_>>();
	diagnostics.extend(
		result
			.diagnostics
			.iter()
			.cloned()
			.filter_map(|diag| apply_semantic_lint(db, key, module, diag))
			.map(|diag| ProjectDiagnostic {
				module: module.display_key(db),
				diag,
			})
			.collect::<Vec<_>>(),
	);
	diagnostics.extend(
		qualified_access_causes
			.into_iter()
			.map(|cause| cause.render(&module.display_key(db))),
	);
	for (target, span) in result.analysis.checked.local_inherent_owners() {
		if target.module != module.identity(db) {
			diagnostics.push(ProjectDiagnostic {
				module: module.display_key(db),
				diag: Diagnostic::error(
					"INHERENT-IMPL-OWNER".into(),
					format!(
						"inherent impl must be declared in its owning module `{}`",
						target.module.path
					),
					span,
				),
			});
		}
	}
	let mut semantic = result.analysis.as_ref().clone();
	semantic.import_references = resolved.references.clone().into();
	Arc::new(super::session::ModuleAnalysis {
		source: match module {
			SemanticModuleInput::Project(module) => module.source(db).unwrap_or_default(),
			SemanticModuleInput::Builtin(module) => module.source(db),
		},
		semantic: Arc::new(semantic),
		diagnostics: super::session::ProjectDiagnostics(diagnostics.into()),
	})
}

#[salsa::tracked(returns(clone))]
fn interface_declared_headers<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<nymph_sema::DeclaredHeaders> {
	let own = nymph_sema::declared_headers(module.identity(db), &module.parsed(db).tree);
	let mut checked_definitions = own.checked_definitions.clone();
	checked_definitions.extend(
		resolved_module_imports(db, key, module)
			.bindings
			.iter()
			.filter_map(|(name, binding)| match binding {
				nymph_sema::ResolvedImportBinding::Definition(definition) => {
					Some((name.clone(), definition.clone()))
				}
				_ => None,
			})
			.collect::<Vec<_>>(),
	);
	if key.ambient_prelude(db) {
		let registry = key.ambient_core_registry(db);
		for root in registry.modules(db).iter().copied() {
			checked_definitions.extend(
				ambient_core_interface(db, registry, root)
					.exports
					.iter()
					.map(|definition| (definition.name.clone(), definition.id.clone())),
			);
		}
	}
	Arc::new(own.with_checked_definitions(checked_definitions))
}

#[salsa::tracked(returns(clone))]
pub(crate) fn interface_module_interface<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Result<Arc<nymph_sema::ModuleInterface>, Arc<nymph_sema::InterfaceConversionError>> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("interface_module_interface", module);
	let analysis = interface_module_analysis(db, key, module);
	let headers = interface_declared_headers(db, key, module);
	let facts = nymph_sema::ExtractionFactSelection::current_module_from_facts(
		&analysis.semantic.module,
		&analysis.semantic.checked,
	);
	nymph_sema::extract_module_interface_from_facts_with_selection(
		module.identity(db),
		&analysis.semantic.module,
		&analysis.semantic.checked,
		&headers,
		&facts,
	)
	.map(Arc::new)
	.map_err(Arc::new)
}

pub(crate) fn documentation_module_interface<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
	document_private_items: bool,
) -> Result<Arc<nymph_sema::ModuleInterface>, Arc<nymph_sema::InterfaceConversionError>> {
	if !document_private_items {
		return interface_module_interface(db, key, module);
	}
	let analysis = interface_module_analysis(db, key, module);
	let headers = interface_declared_headers(db, key, module);
	let facts = nymph_sema::ExtractionFactSelection::current_module_from_facts(
		&analysis.semantic.module,
		&analysis.semantic.checked,
	)
	.including_private_definitions();
	nymph_sema::extract_module_interface_from_facts_with_selection(
		module.identity(db),
		&analysis.semantic.module,
		&analysis.semantic.checked,
		&headers,
		&facts,
	)
	.map(Arc::new)
	.map_err(Arc::new)
}

#[salsa::tracked(returns(clone))]
pub(crate) fn interface_module_environment<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<nymph_sema::ModuleEnvironment> {
	#[cfg(feature = "test-support")]
	db.semantic_query_will_execute("interface_module_environment", module);
	let analysis = interface_module_analysis(db, key, module);
	let diagnostics = interface_module_diagnostics(db, key, module);
	if diagnostics.is_empty()
		&& let Ok(interface) = interface_module_interface(db, key, module)
	{
		return Arc::new(nymph_sema::ModuleEnvironment::Complete(
			(*interface).clone(),
		));
	}
	let checked = checked_from_analysis(&analysis, diagnostics.iter().map(|item| item.diag.clone()));
	let headers = interface_declared_headers(db, key, module);
	let facts =
		nymph_sema::ExtractionFactSelection::current_module(&analysis.semantic.module, &checked);
	Arc::new(nymph_sema::recover_module_environment_with_facts(
		module.identity(db),
		&analysis.semantic.module,
		&checked,
		&headers,
		&facts,
	))
}

#[salsa::tracked(returns(clone))]
fn interface_module_diagnostics<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
	module: SemanticModuleInput,
) -> Arc<[ProjectDiagnostic]> {
	let analysis = interface_module_analysis(db, key, module);
	analysis.diagnostics.0.clone()
}

#[cfg(not(target_arch = "wasm32"))]
fn prewarm_interface_module_diagnostics(
	db: &dyn Db,
	key: super::session::ProjectKey<'_>,
	ambient: Option<(AmbientCoreRegistryInput, Vec<BuiltinModuleInput>)>,
	modules: Vec<SemanticModuleInput>,
) {
	let ambient_len = ambient.as_ref().map_or(0, |(_, modules)| modules.len());
	if modules.len() < 2 && ambient_len < 2 {
		return;
	}
	static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
	let module_workers = modules
		.iter()
		.map(|_| db.parallel_clone())
		.collect::<Vec<_>>();
	let ambient_workers = ambient.as_ref().map(|(_, modules)| {
		modules
			.iter()
			.map(|_| db.parallel_clone())
			.collect::<Vec<_>>()
	});
	// Salsa Storage clones share memo tables but have distinct local query stacks.
	// The coordinator prevents the aggregate-query caller from helping this private
	// pool and therefore from switching databases while its query stack is active.
	let result = std::thread::scope(|scope| {
		scope
			.spawn(move || {
				POOL
					.get_or_init(|| {
						rayon::ThreadPoolBuilder::new()
							.thread_name(|index| format!("nymph-diagnostics-{index}"))
							.build()
							.expect("build diagnostics worker pool")
					})
					.install(|| {
						#[cfg(feature = "test-support")]
						super::benchmark_support::record_prewarm_workers(rayon::current_num_threads());
						if let (Some((registry, modules)), Some(workers)) = (ambient, ambient_workers) {
							workers
								.into_par_iter()
								.zip(modules.into_par_iter())
								.for_each(|(worker, module)| {
									#[cfg(feature = "test-support")]
									let _active = super::benchmark_support::prewarm_task();
									ambient_core_environment(worker.as_ref(), registry, module);
								});
						}
						module_workers
							.into_par_iter()
							.zip(modules.into_par_iter())
							.for_each(|(worker, module)| {
								#[cfg(feature = "test-support")]
								let _active = super::benchmark_support::prewarm_task();
								interface_module_diagnostics(worker.as_ref(), key, module);
							});
					});
			})
			.join()
	});
	if let Err(payload) = result {
		std::panic::resume_unwind(payload);
	}
}

#[salsa::tracked(returns(clone))]
pub(crate) fn interface_project_diagnostics<'db>(
	db: &'db dyn Db,
	key: super::session::ProjectKey<'db>,
) -> super::session::ProjectDiagnostics {
	let policy_diagnostics = policy_project_diagnostics(db, key);
	let graph = project_graph(db, key);
	if !graph.diagnostics.is_empty() {
		let mut diagnostics = graph.diagnostics.iter().cloned().collect::<Vec<_>>();
		// Graph roots remain first. Then check valid dependencies and modules
		// recovered solely from parser errors in dependency order.
		for (module, _) in graph.semantic_direct.iter() {
			let parsed = module.parsed(db);
			let valid = graph.semantic_order.contains(module);
			let parser_recovered = parsed.diagnostics.iter().any(Diagnostic::is_error)
				&& graph
					.diagnostics
					.iter()
					.filter(|diagnostic| diagnostic.module == module.display_key(db))
					.all(|diagnostic| parsed.diagnostics.contains(&diagnostic.diag))
				&& graph
					.semantic_direct_dependencies(*module)
					.iter()
					.all(|dependency| graph.semantic_order.contains(dependency));
			if !valid && !parser_recovered {
				continue;
			}
			for diagnostic in interface_module_diagnostics(db, key, *module).iter() {
				if !diagnostics.contains(diagnostic) {
					diagnostics.push(diagnostic.clone());
				}
			}
		}
		diagnostics.extend(policy_diagnostics.0.iter().cloned());
		return super::session::ProjectDiagnostics(diagnostics.into());
	}
	// Native cold builds evaluate independent branches through cloned Salsa handles.
	// The serial fold remains authoritative for dependency registration and graph-order
	// diagnostics. Non-threaded wasm skips prewarming and executes this fold directly.
	#[cfg(not(target_arch = "wasm32"))]
	{
		let ambient = key.ambient_prelude(db).then(|| {
			let registry = key.ambient_core_registry(db);
			(registry, registry.modules(db).to_vec())
		});
		prewarm_interface_module_diagnostics(db, key, ambient, graph.semantic_order.to_vec());
	}
	#[cfg(feature = "test-support")]
	let _reporting_timing =
		super::benchmark_support::phase(super::benchmark_support::Phase::DiagnosticReporting);
	let mut all = Vec::new();
	for module in graph.semantic_order.iter().copied() {
		all.extend(
			interface_module_diagnostics(db, key, module)
				.iter()
				.cloned(),
		);
	}
	all.extend(policy_diagnostics.0.iter().cloned());
	let diagnostics = all.into();
	super::session::ProjectDiagnostics(diagnostics)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
	Gray,
	Black,
}

#[salsa::tracked]
pub(crate) fn project_graph<'db>(db: &'db dyn Db, key: ProjectKey<'db>) -> Arc<ProjectGraph> {
	use std::collections::BTreeMap;

	struct Walker<'a> {
		db: &'a dyn Db,
		active: &'a BTreeMap<(PackageId, ModulePath), ModuleInput>,
		builtins: &'a BTreeMap<Arc<str>, BuiltinModuleInput>,
		aliases: &'a BTreeMap<(PackageId, Arc<str>), PackageId>,
		colors: FxHashMap<SemanticModuleInput, Color>,
		stack: Vec<SemanticModuleInput>,
		order: Vec<ModuleInput>,
		direct: Vec<(ModuleInput, Arc<[ModuleInput]>)>,
		semantic_order: Vec<SemanticModuleInput>,
		semantic_direct: Vec<(SemanticModuleInput, Arc<[SemanticModuleInput]>)>,
		diagnostics: Vec<ProjectDiagnostic>,
	}

	impl Walker<'_> {
		fn diagnostic(&mut self, module: &str, diag: Diagnostic) {
			self.diagnostics.push(ProjectDiagnostic {
				module: module.to_string(),
				diag,
			});
		}

		fn resolve_target(
			&self,
			owner: SemanticModuleInput,
			target: &ResolvedImportTarget,
		) -> Option<SemanticModuleInput> {
			match target {
				ResolvedImportTarget::ImportableStd(path) => self
					.builtins
					.get(path.as_str())
					.copied()
					.map(SemanticModuleInput::Builtin),
				ResolvedImportTarget::CurrentPackage(path) => match owner {
					SemanticModuleInput::Project(module) => self
						.active
						.get(&(
							module.package(self.db),
							ModulePath::new(path).expect("resolved current-package path is canonical"),
						))
						.copied()
						.map(SemanticModuleInput::Project),
					SemanticModuleInput::Builtin(_) => self
						.builtins
						.get(path.as_str())
						.copied()
						.map(SemanticModuleInput::Builtin),
				},
				ResolvedImportTarget::Package { alias, path } => {
					let SemanticModuleInput::Project(module) = owner else {
						return None;
					};
					let package = self
						.aliases
						.get(&(module.package(self.db), Arc::from(alias.as_str())))?;
					self
						.active
						.get(&(
							package.clone(),
							ModulePath::new(path).expect("resolved package path is canonical"),
						))
						.copied()
						.map(SemanticModuleInput::Project)
				}
			}
		}

		fn target_label(target: &ResolvedImportTarget) -> String {
			match target {
				ResolvedImportTarget::CurrentPackage(path) => path.clone(),
				ResolvedImportTarget::Package { alias, path } => format!("{alias}/{path}"),
				ResolvedImportTarget::ImportableStd(path) => format!("std/{path}"),
			}
		}

		fn visit(&mut self, module: SemanticModuleInput) -> bool {
			let display = module.display_key(self.db);
			match self.colors.get(&module) {
				Some(Color::Black) => return true,
				Some(Color::Gray) => {
					let start = self
						.stack
						.iter()
						.position(|item| *item == module)
						.unwrap_or(0);
					let mut cycle = self.stack[start..]
						.iter()
						.map(|item| item.display_key(self.db))
						.collect::<Vec<_>>();
					cycle.push(display.clone());
					self.diagnostic(
						&display,
						Diagnostic::error(
							"IMPORT-CYCLE".into(),
							format!("import cycle detected: {}", cycle.join(" -> ")),
							Span::new(0, 0),
						),
					);
					return false;
				}
				None => {}
			}
			self.colors.insert(module, Color::Gray);
			self.stack.push(module);

			let parsed = module.parsed(self.db);
			let mut ok = true;
			for diag in parsed.diagnostics.iter().filter(|diag| diag.is_error()) {
				self.diagnostic(&display, diag.clone());
				ok = false;
			}
			let imports = module.imports(self.db);
			let mut handles = Vec::new();
			let mut semantic_handles = Vec::new();
			let mut unresolved_targets = Vec::new();
			for import in imports.iter() {
				match &import.target {
					Ok(target) => {
						let Some(handle) = self.resolve_target(module, target) else {
							if !unresolved_targets.contains(target) {
								unresolved_targets.push(target.clone());
								let label = Self::target_label(target);
								self.diagnostic(
									&display,
									Diagnostic::error(
										"IMPORT-UNRESOLVED".into(),
										format!("module `{label}` could not be resolved (no source file found)"),
										import.span,
									),
								);
							}
							ok = false;
							continue;
						};
						if !semantic_handles.contains(&handle) {
							semantic_handles.push(handle);
						}
						if let SemanticModuleInput::Project(project) = handle
							&& !handles.contains(&project)
						{
							handles.push(project);
						}
						let child_ok = self.visit(handle);
						ok = ok && child_ok;
					}
					Err(diag) => {
						self.diagnostic(&display, diag.clone());
						ok = false;
					}
				}
			}
			if let SemanticModuleInput::Project(project) = module {
				self.direct.push((project, handles.into()));
			}
			self.semantic_direct.push((module, semantic_handles.into()));
			self.colors.insert(module, Color::Black);
			self.stack.pop();
			if ok {
				self.semantic_order.push(module);
				if let SemanticModuleInput::Project(project) = module {
					self.order.push(project);
				}
			}
			ok
		}
	}

	let project_input: ProjectInput = key.project_input(db);
	let active: BTreeMap<(PackageId, ModulePath), ModuleInput> = project_input
		.active_modules(db)
		.iter()
		.map(|module| ((module.package(db), module.path(db)), *module))
		.collect();
	let builtins: BTreeMap<Arc<str>, BuiltinModuleInput> = key
		.builtin_registry(db)
		.modules(db)
		.iter()
		.map(|module| (module.key(db).path, *module))
		.collect();
	let aliases = project_input
		.package_aliases(db)
		.iter()
		.map(|alias| {
			(
				(alias.owner.clone(), alias.name.clone()),
				alias.target.clone(),
			)
		})
		.collect::<BTreeMap<_, _>>();
	let mut walker = Walker {
		db,
		active: &active,
		builtins: &builtins,
		aliases: &aliases,
		colors: FxHashMap::default(),
		stack: Vec::new(),
		order: Vec::new(),
		direct: Vec::new(),
		semantic_order: Vec::new(),
		semantic_direct: Vec::new(),
		diagnostics: Vec::new(),
	};
	let entry_package = PackageId::root(project_input.project(db));
	if let Some(entry) = active.get(&(entry_package, key.entry(db))).copied() {
		walker.visit(SemanticModuleInput::Project(entry));
	} else {
		let entry = key.entry(db).to_string();
		walker.diagnostic(
			&entry,
			Diagnostic::error(
				"IMPORT-UNRESOLVED".into(),
				format!("module `{entry}` could not be resolved (no source file found)"),
				Span::new(0, 0),
			),
		);
	}
	Arc::new(ProjectGraph {
		order: walker.order.into(),
		direct: walker.direct.into(),
		semantic_order: walker.semantic_order.into(),
		semantic_direct: walker.semantic_direct.into(),
		diagnostics: walker.diagnostics.into(),
	})
}

#[cfg(test)]
mod tests {
	use nymph_ast::Span;
	use nymph_sema::EntryMode;
	use salsa::Setter;

	use super::*;
	use crate::project::session::{BuiltinModuleKey, BuiltinRegistryInput, ProjectId};

	#[salsa::db]
	#[derive(Clone)]
	struct TestDb {
		storage: salsa::Storage<Self>,
	}
	#[salsa::db]
	impl salsa::Database for TestDb {}
	#[salsa::db]
	impl Db for TestDb {
		#[cfg(not(target_arch = "wasm32"))]
		fn parallel_clone(&self) -> Box<dyn Db> {
			Box::new(self.clone())
		}
	}

	#[test]
	fn runtime_manifest_rejects_duplicate_exact_identities_with_a_typed_error() {
		let db = TestDb {
			storage: salsa::Storage::default(),
		};
		let definition = nymph_sema::DefinitionId::new(
			nymph_sema::ModuleIdentity {
				origin: nymph_sema::ModuleOrigin::Project("manifest-duplicate".into()),
				project: "manifest-duplicate".into(),
				path: "main".into(),
			},
			nymph_sema::DeclarationKey::top_level(nymph_sema::DeclarationCategory::Struct, "Item"),
		);
		let artifact = nymph_sema::RuntimeDefinition {
			definition: definition.clone(),
			source_owner: definition.module.clone(),
			placement: nymph_sema::RuntimePlacement::TopLevel,
			payload: nymph_sema::RuntimePayload::Struct(nymph_sema::StructShell {
				binders: Vec::new(),
				constraints: Vec::new(),
				fields: Vec::new(),
				defaults: Vec::new(),
			}),
		};

		assert!(matches!(
			RuntimeManifest::new(&db, [artifact.clone(), artifact]),
			Err(nymph_sema::RuntimeExtractionError::DuplicateRuntimeDefinition(found))
				if found == definition
		));
	}

	#[test]
	fn private_access_cause_renders_without_inspecting_other_diagnostics() {
		let span = Span::new(10, 16);
		let definition = nymph_sema::DefinitionId::new(
			nymph_sema::ModuleIdentity {
				origin: nymph_sema::ModuleOrigin::Project("privacy".into()),
				project: "privacy".into(),
				path: "dependency".into(),
			},
			nymph_sema::DeclarationKey::top_level(nymph_sema::DeclarationCategory::Function, "helper"),
		);
		let diagnostic = ImportDiagnosticCause::PrivateNamespaceMember {
			declaration: nymph_sema::NamespaceDeclaration {
				name: "helper".into(),
				definition,
				visibility: nymph_sema::NamespaceVisibility::Private,
			},
			span,
		}
		.render("main");

		assert_eq!(diagnostic.module, "main");
		assert_eq!(diagnostic.diag.code, "IMPORT-PRIVATE-NAME");
		assert_eq!(diagnostic.diag.span, span);
	}

	fn fixture(files: &[(&str, &str)], builtins: &[(&str, &str)]) -> (TestDb, ProjectKey<'static>) {
		let db = TestDb {
			storage: salsa::Storage::default(),
		};
		let project = ProjectId::new("graph-regression");
		let root_package = super::super::session::PackageId::root(project.clone());
		let modules: Arc<[ModuleInput]> = files
			.iter()
			.map(|(path, source)| {
				ModuleInput::new(
					&db,
					project.clone(),
					root_package.clone(),
					ModulePath::new(path).unwrap(),
					Some(Arc::from(*source)),
					Arc::from(format!("{path}.nym")),
					None,
					nymph_diagnostics::SourceVersion(1),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let builtin_modules: Arc<[BuiltinModuleInput]> = builtins
			.iter()
			.map(|(key, source)| {
				BuiltinModuleInput::new(
					&db,
					BuiltinModuleKey {
						domain: BuiltinModuleDomain::ImportableStd,
						path: Arc::from(*key),
					},
					Arc::from(*source),
				)
			})
			.collect::<Vec<_>>()
			.into();
		let policy = ProjectPolicyInput::new(
			&db,
			PackageId::root(project.clone()),
			crate::project::BuildProfile::Development,
			Arc::new([]),
		);
		let input = ProjectInput::new(&db, project, modules, Arc::new([]));
		let registry = BuiltinRegistryInput::new(&db, builtin_modules);
		let ambient = AmbientCoreRegistryInput::new(&db, Arc::new([]));
		let key = ProjectKey::new(
			&db,
			input,
			policy,
			registry,
			ambient,
			ModulePath::new("main").unwrap(),
			EntryMode::Entry,
			false,
			true,
			false,
		);
		// Test databases outlive each key in these tests; Salsa's key does not
		// contain an actual reference despite its invariant database lifetime.
		let key = unsafe { std::mem::transmute::<ProjectKey<'_>, ProjectKey<'static>>(key) };
		(db, key)
	}

	type DiagnosticTuple = (String, String, String, usize, usize);

	fn tuples(diags: &[ProjectDiagnostic]) -> Vec<DiagnosticTuple> {
		diags
			.iter()
			.map(|item| {
				(
					item.module.clone(),
					item.diag.code.to_string(),
					item.diag.message.to_string(),
					item.diag.span.start,
					item.diag.span.end,
				)
			})
			.collect()
	}

	fn graph_diagnostics(files: &[(&str, &str)]) -> Vec<DiagnosticTuple> {
		let (db, key) = fixture(files, &[]);
		tuples(&project_graph(&db, key).diagnostics)
	}

	#[test]
	fn graph_diagnostics_preserve_dfs_order_and_deduplication() {
		let cycle = graph_diagnostics(&[
			("main", "import @/a"),
			("a", "import @/b"),
			("b", "import @/a"),
		]);
		assert_eq!(cycle[0].0, "a");
		assert_eq!(cycle[0].1, "IMPORT-CYCLE");
		assert_eq!(cycle[0].2, "import cycle detected: a -> b -> a");

		let recovered = graph_diagnostics(&[("main", "import @/missing\nfunc broken(: int = 1")]);
		assert!(recovered.len() >= 2);
		assert_ne!(recovered[0].1, "IMPORT-UNRESOLVED");
		assert_eq!(recovered.last().unwrap().1, "IMPORT-UNRESOLVED");

		let mixed = graph_diagnostics(&[("main", "import pkg/nope\nimport @/missing")]);
		assert_eq!(
			mixed.iter().map(|item| item.1.as_str()).collect::<Vec<_>>(),
			["IMPORT-UNRESOLVED", "IMPORT-UNRESOLVED"]
		);

		let duplicate = graph_diagnostics(&[("main", "import @/missing\nimport @/missing")]);
		assert_eq!(
			duplicate
				.iter()
				.filter(|item| item.1 == "IMPORT-UNRESOLVED")
				.count(),
			1
		);
	}

	#[test]
	fn graph_ignores_unreachable_errors_and_preserves_clean_public_contracts() {
		let (db, key) = fixture(
			&[
				("main", "import @/a\nimport std/tool"),
				("a", "import @/b"),
				("b", "let value = 1"),
				("unreachable", "import @/missing"),
			],
			&[("tool", "public let answer = 42")],
		);
		let graph = project_graph(&db, key);
		assert!(graph.diagnostics.is_empty());
		assert_eq!(
			graph
				.order
				.iter()
				.map(|module| module.path(&db).as_str().to_string())
				.collect::<Vec<_>>(),
			["b", "a", "main"]
		);
		assert_eq!(graph.direct.len(), 3);
	}

	#[test]
	fn semantic_graph_includes_importable_builtins_dependency_first_without_changing_project_order() {
		let (db, key) = fixture(
			&[
				("main", "import @/a\nimport std/tool"),
				("a", "import std/base"),
			],
			&[
				("tool", "import ./base\npublic let tool = 1"),
				("base", "public let base = 1"),
			],
		);
		let graph = project_graph(&db, key);
		assert!(graph.diagnostics.is_empty());
		assert_eq!(
			graph
				.order
				.iter()
				.map(|module| module.path(&db).as_str().to_string())
				.collect::<Vec<_>>(),
			["a", "main"]
		);
		assert_eq!(
			graph
				.semantic_order
				.iter()
				.map(|module| module.display_key(&db))
				.collect::<Vec<_>>(),
			["std::base", "a", "std::tool", "main"]
		);
		let main = SemanticModuleInput::Project(
			*graph
				.order
				.iter()
				.find(|module| module.path(&db).as_str() == "main")
				.unwrap(),
		);
		assert_eq!(
			graph
				.semantic_closure(main)
				.iter()
				.map(|module| module.display_key(&db))
				.collect::<Vec<_>>(),
			["std::base", "a", "std::tool"]
		);
		assert_eq!(
			graph
				.semantic_direct_dependencies(main)
				.iter()
				.map(|module| module.display_key(&db))
				.collect::<Vec<_>>(),
			["a", "std::tool"]
		);
		assert_eq!(graph.semantic_direct_imports(&db, main).len(), 2);
	}

	#[test]
	fn semantic_module_identity_domains_do_not_collide_or_include_ambient_core() {
		let (db, key) = fixture(
			&[
				("main", "import @/std/tool\nimport std/tool"),
				("std/tool", ""),
			],
			&[("tool", "")],
		);
		let graph = project_graph(&db, key);
		let identities = graph
			.semantic_order
			.iter()
			.map(|module| module.identity(&db))
			.collect::<std::collections::BTreeSet<_>>();
		assert_eq!(identities.len(), 3);
		assert!(graph.semantic_order.iter().any(|module| {
			module.domain(&db) == SemanticModuleDomain::Project && module.display_key(&db) == "std/tool"
		}));
		assert!(graph.semantic_order.iter().any(|module| {
			module.domain(&db) == SemanticModuleDomain::ImportableStd
				&& module.display_key(&db) == "std::tool"
		}));
		assert!(
			graph
				.semantic_order
				.iter()
				.all(|module| !module.is_ambient_core(&db))
		);
		assert_eq!(graph.order.len(), 2);
	}

	#[test]
	fn builtin_parse_uses_the_external_display_path() {
		let (db, key) = fixture(&[("main", "")], &[("custom", "public let answer = 42")]);
		let builtin = key.builtin_registry(&db).modules(&db)[0];
		assert_eq!(
			parse_builtin(&db, builtin).tree.path.as_str(),
			"std::custom.nym"
		);
	}

	#[test]
	fn canonical_ambient_parse_is_shared_but_mutations_fall_back() {
		fn option_input(db: &TestDb, source: Arc<str>) -> BuiltinModuleInput {
			BuiltinModuleInput::new(
				db,
				BuiltinModuleKey {
					domain: BuiltinModuleDomain::AmbientCore,
					path: Arc::from("option"),
				},
				source,
			)
		}

		let (_, canonical_source) = crate::prelude::core_source("option").unwrap();
		let db_a = TestDb {
			storage: salsa::Storage::default(),
		};
		let input_a = option_input(&db_a, Arc::from(canonical_source));
		let canonical_a = parse_builtin(&db_a, input_a).clone();
		assert_eq!(
			canonical_a,
			parse_source(Arc::from(canonical_source), "core::option.nym".into())
		);

		let mut db_b = TestDb {
			storage: salsa::Storage::default(),
		};
		let input_b = option_input(&db_b, Arc::from(canonical_source));
		let canonical_b = parse_builtin(&db_b, input_b).clone();
		assert!(Arc::ptr_eq(&canonical_a, &canonical_b));

		let malformed: Arc<str> = Arc::from("public enum Option<T> {");
		input_b.set_source(&mut db_b).to(malformed.clone());
		let changed = parse_builtin(&db_b, input_b).clone();
		assert!(!Arc::ptr_eq(&canonical_a, &changed));
		assert_eq!(changed, parse_source(malformed, "core::option.nym".into()));
		assert!(!changed.diagnostics.is_empty());
		assert_eq!(changed.tree.path.as_str(), "core::option.nym");

		input_b
			.set_source(&mut db_b)
			.to(Arc::from(canonical_source));
		let restored = parse_builtin(&db_b, input_b).clone();
		assert!(Arc::ptr_eq(&canonical_a, &restored));
	}
}
