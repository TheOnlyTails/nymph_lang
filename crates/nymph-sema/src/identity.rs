//! Owned semantic identities which remain stable when non-header source changes.

use std::collections::HashMap;

use ecow::EcoString;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum ModuleOrigin {
	Project(EcoString),
	Compiler,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ModuleIdentity {
	pub origin: ModuleOrigin,
	pub project: EcoString,
	pub path: EcoString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum DeclarationCategory {
	Function,
	Let,
	TypeAlias,
	Struct,
	Enum,
	Interface,
	Namespace,
	Variant,
	Field,
	Method,
	Static,
	Implementation,
	MethodBody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct HeaderParameterId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct HeaderBinder {
	pub parameter: HeaderParameterId,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum HeaderType {
	Poison,
	Int,
	UInt,
	Float,
	Char,
	String,
	Boolean,
	Void,
	Never,
	SelfType,
	List(Box<Self>),
	Tuple(Vec<Self>),
	Map(Box<Self>, Box<Self>),
	Function {
		parameters: Vec<Self>,
		return_type: Box<Self>,
	},
	Named {
		definition: DefinitionId,
		positional: Vec<Self>,
		named: Vec<(EcoString, Self)>,
	},
	Intersection(Vec<Self>),
	Mutable(Box<Self>),
	Generic(HeaderParameterId),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct HeaderConstraint {
	pub parameter: HeaderParameterId,
	pub interface: DefinitionId,
	pub positional: Vec<HeaderType>,
	pub named: Vec<(EcoString, HeaderType)>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ImplementationHeader {
	pub interface: Option<DefinitionId>,
	pub interface_arguments: Vec<(EcoString, HeaderType)>,
	pub self_type: HeaderType,
	pub mutable: bool,
	pub binders: Vec<HeaderBinder>,
	pub constraints: Vec<HeaderConstraint>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct RecoveredHeaderConstraint {
	pub parameter: HeaderParameterId,
	pub interface: EcoString,
	pub positional: Vec<RecoveredHeaderType>,
	pub named: Vec<(EcoString, RecoveredHeaderType)>,
}

/// Span-free source structure used only to stabilize malformed implementation IDs.
/// Semantic recovered interface slots deliberately remain `Known | Poison`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum RecoveredHeaderType {
	Atom(EcoString),
	List(Box<Self>),
	Tuple(Vec<Self>),
	Map(Box<Self>, Box<Self>),
	Function {
		parameters: Vec<Self>,
		return_type: Box<Self>,
	},
	Reference {
		name: EcoString,
		positional: Vec<Self>,
		named: Vec<(EcoString, Self)>,
	},
	Intersection(Vec<Self>),
	Mutable(Box<Self>),
	Generic(HeaderParameterId),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct RecoveredImplementationHeader {
	pub interface: Option<EcoString>,
	pub interface_arguments: Vec<(EcoString, RecoveredHeaderType)>,
	pub self_type: RecoveredHeaderType,
	pub mutable: bool,
	pub binders: Vec<HeaderBinder>,
	pub constraints: Vec<RecoveredHeaderConstraint>,
}

impl RecoveredImplementationHeader {
	pub fn canonical(mut self) -> Self {
		fn canonical_type(ty: &mut RecoveredHeaderType) {
			match ty {
				RecoveredHeaderType::List(inner) | RecoveredHeaderType::Mutable(inner) => {
					canonical_type(inner)
				}
				RecoveredHeaderType::Tuple(items) => items.iter_mut().for_each(canonical_type),
				RecoveredHeaderType::Map(key, value) => {
					canonical_type(key);
					canonical_type(value);
				}
				RecoveredHeaderType::Function {
					parameters,
					return_type,
				} => {
					parameters.iter_mut().for_each(canonical_type);
					canonical_type(return_type);
				}
				RecoveredHeaderType::Reference {
					positional, named, ..
				} => {
					positional.iter_mut().for_each(canonical_type);
					for (_, ty) in &mut *named {
						canonical_type(ty);
					}
					named.sort();
				}
				RecoveredHeaderType::Intersection(items) => {
					items.iter_mut().for_each(canonical_type);
					items.sort();
				}
				RecoveredHeaderType::Atom(_) | RecoveredHeaderType::Generic(_) => {}
			}
		}
		for (_, ty) in &mut self.interface_arguments {
			canonical_type(ty);
		}
		canonical_type(&mut self.self_type);
		for constraint in &mut self.constraints {
			constraint.positional.iter_mut().for_each(canonical_type);
			for (_, ty) in &mut constraint.named {
				canonical_type(ty);
			}
			constraint.named.sort();
		}
		self.interface_arguments.sort();
		self.constraints.sort();
		self
	}
}

impl ImplementationHeader {
	pub fn canonical(mut self) -> Self {
		let mut slots = HashMap::new();
		for (slot, binder) in self.binders.iter().enumerate() {
			assert!(
				slots
					.insert(binder.parameter, HeaderParameterId(slot as u32))
					.is_none(),
				"implementation header contains duplicate binder parameter ID"
			);
		}
		let rewrite_parameter = |parameter: &mut HeaderParameterId| {
			*parameter = *slots
				.get(parameter)
				.expect("implementation header type references an undeclared binder")
		};
		fn rewrite_type(ty: &mut HeaderType, rewrite_parameter: &impl Fn(&mut HeaderParameterId)) {
			match ty {
				HeaderType::List(inner) | HeaderType::Mutable(inner) => {
					rewrite_type(inner, rewrite_parameter);
				}
				HeaderType::Tuple(items) => {
					for item in &mut *items {
						rewrite_type(item, rewrite_parameter);
					}
				}
				HeaderType::Intersection(items) => {
					let mut flattened = Vec::new();
					for mut item in std::mem::take(items) {
						rewrite_type(&mut item, rewrite_parameter);
						match item {
							HeaderType::Intersection(nested) => flattened.extend(nested),
							other => flattened.push(other),
						}
					}
					flattened.sort();
					flattened.dedup();
					*ty = match flattened.len() {
						0 => HeaderType::Void,
						1 => flattened.pop().unwrap(),
						_ => HeaderType::Intersection(flattened),
					};
				}
				HeaderType::Map(key, value) => {
					rewrite_type(key, rewrite_parameter);
					rewrite_type(value, rewrite_parameter);
				}
				HeaderType::Function {
					parameters,
					return_type,
				} => {
					for parameter in parameters {
						rewrite_type(parameter, rewrite_parameter);
					}
					rewrite_type(return_type, rewrite_parameter);
				}
				HeaderType::Named {
					positional, named, ..
				} => {
					for argument in positional {
						rewrite_type(argument, rewrite_parameter);
					}
					for (_, argument) in &mut *named {
						rewrite_type(argument, rewrite_parameter);
					}
					named.sort();
				}
				HeaderType::Generic(parameter) => rewrite_parameter(parameter),
				HeaderType::Poison
				| HeaderType::Int
				| HeaderType::UInt
				| HeaderType::Float
				| HeaderType::Char
				| HeaderType::String
				| HeaderType::Boolean
				| HeaderType::Void
				| HeaderType::Never
				| HeaderType::SelfType => {}
			}
		}
		for (slot, binder) in self.binders.iter_mut().enumerate() {
			binder.parameter = HeaderParameterId(slot as u32);
		}
		for (_, argument) in &mut self.interface_arguments {
			rewrite_type(argument, &rewrite_parameter);
		}
		rewrite_type(&mut self.self_type, &rewrite_parameter);
		for constraint in &mut self.constraints {
			rewrite_parameter(&mut constraint.parameter);
			for argument in &mut constraint.positional {
				rewrite_type(argument, &rewrite_parameter);
			}
			for (_, argument) in &mut constraint.named {
				rewrite_type(argument, &rewrite_parameter);
			}
			constraint.named.sort();
		}
		self.interface_arguments.sort();
		self.constraints.sort();
		self
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum DeclarationKey {
	TopLevel {
		category: DeclarationCategory,
		name: EcoString,
		duplicate: u32,
	},
	Member {
		owner: Box<DefinitionId>,
		category: DeclarationCategory,
		name: EcoString,
		duplicate: u32,
	},
	Implementation {
		header: Box<ImplementationHeader>,
		duplicate: u32,
	},
	RecoveredImplementation {
		header: Box<RecoveredImplementationHeader>,
		duplicate: u32,
	},
	MethodBody {
		owner: Box<DefinitionId>,
		name: EcoString,
		duplicate: u32,
	},
	MaterializedInterfaceMember {
		implementation: Box<DefinitionId>,
		interface_member: Box<DefinitionId>,
	},
}

impl DeclarationKey {
	pub fn top_level(category: DeclarationCategory, name: impl Into<EcoString>) -> Self {
		Self::TopLevel {
			category,
			name: name.into(),
			duplicate: 0,
		}
	}

	pub fn member(
		owner: DefinitionId,
		category: DeclarationCategory,
		name: impl Into<EcoString>,
	) -> Self {
		Self::Member {
			owner: Box::new(owner),
			category,
			name: name.into(),
			duplicate: 0,
		}
	}

	pub fn implementation(header: ImplementationHeader) -> Self {
		Self::Implementation {
			header: Box::new(header.canonical()),
			duplicate: 0,
		}
	}

	pub fn recovered_implementation(header: RecoveredImplementationHeader) -> Self {
		Self::RecoveredImplementation {
			header: Box::new(header.canonical()),
			duplicate: 0,
		}
	}

	pub fn method_body(owner: DefinitionId, name: impl Into<EcoString>) -> Self {
		Self::MethodBody {
			owner: Box::new(owner),
			name: name.into(),
			duplicate: 0,
		}
	}

	pub fn materialized_interface_member(
		implementation: DefinitionId,
		interface_member: DefinitionId,
	) -> Self {
		Self::MaterializedInterfaceMember {
			implementation: Box::new(implementation),
			interface_member: Box::new(interface_member),
		}
	}

	pub fn duplicate(&self) -> u32 {
		match self {
			Self::TopLevel { duplicate, .. }
			| Self::Member { duplicate, .. }
			| Self::Implementation { duplicate, .. }
			| Self::RecoveredImplementation { duplicate, .. }
			| Self::MethodBody { duplicate, .. } => *duplicate,
			Self::MaterializedInterfaceMember { .. } => 0,
		}
	}

	fn with_duplicate(mut self, value: u32) -> Self {
		match &mut self {
			Self::TopLevel { duplicate, .. }
			| Self::Member { duplicate, .. }
			| Self::Implementation { duplicate, .. }
			| Self::RecoveredImplementation { duplicate, .. }
			| Self::MethodBody { duplicate, .. } => *duplicate = value,
			Self::MaterializedInterfaceMember { .. } => {
				assert_eq!(
					value, 0,
					"structural materialized member IDs cannot have duplicates"
				);
			}
		}
		self
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct DefinitionId {
	pub module: ModuleIdentity,
	pub key: DeclarationKey,
}

impl DefinitionId {
	pub fn new(module: ModuleIdentity, key: DeclarationKey) -> Self {
		Self { module, key }
	}

	/// Derives an ordinary externally usable binder after this definition's
	/// stable identity has been allocated.
	pub fn binder(&self, scope: BinderScope, local_index: u32) -> BinderId {
		BinderId::new(self.clone(), scope, local_index)
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct BinderId {
	pub owner: DefinitionId,
	pub scope: BinderScope,
	pub local_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum BinderScope {
	Definition,
	Member,
	Implementation,
}

impl BinderId {
	pub fn new(owner: DefinitionId, scope: BinderScope, local_index: u32) -> Self {
		Self {
			owner,
			scope,
			local_index,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct GenericParameterId {
	pub binder: BinderId,
	pub index: u32,
}

impl GenericParameterId {
	pub fn new(binder: BinderId, index: u32) -> Self {
		Self { binder, index }
	}
}

pub struct StableIdBuilder {
	module: ModuleIdentity,
	duplicates: HashMap<DeclarationKey, u32>,
}

impl StableIdBuilder {
	pub fn new(module: ModuleIdentity) -> Self {
		Self {
			module,
			duplicates: HashMap::new(),
		}
	}

	pub fn allocate(&mut self, key: DeclarationKey) -> DefinitionId {
		let base = key.with_duplicate(0);
		let duplicate = self.duplicates.entry(base.clone()).or_default();
		let id = DefinitionId::new(self.module.clone(), base.with_duplicate(*duplicate));
		*duplicate += 1;
		id
	}
}
