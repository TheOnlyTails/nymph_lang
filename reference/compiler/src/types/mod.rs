pub mod error;

#[cfg(test)]
mod tests;

use std::{
	collections::{BTreeMap, HashMap, HashSet},
	fmt::Display,
	fs,
	hash::Hash,
	ops::Range,
	path::{Path, PathBuf},
	sync::Arc,
};

use ecow::EcoString;
use itertools::Itertools;
use salsa::Accumulator;

use crate::{ast::Span, types::error::TypeErrorKind};
use crate::{
	ast::{
		self, Ident, Spanned,
		declaration::{
			Declaration, FuncDeclaration, ImplMember, ImportRoot, InterfaceElement, InterfaceMember,
			LetDeclaration, Module, StructInnerMember, TypeAliasDeclaration, Visibility,
		},
		expr::{
			Expr, ListItem, Pattern, Statement, anonymous_param_syntaxes, anonymous_params,
			rewrite_anonymous_params,
		},
		ops::{BinaryOperator, PrefixOperator, TypeOperator},
	},
	config,
	db::{
		Db, DefKey, Diagnostic, DiagnosticKind, Diagnostics, NymphDatabase, ProjectConfig, SourceFile,
		TypeErrors,
	},
	prelude::IMPLICIT_PRELUDE_MODULES,
	queries,
	types::error::TypeError,
};

/// Extract a `Range<usize>` from a `Span`
fn span_to_range(span: Span) -> std::ops::Range<usize> {
	span.start..span.end
}

fn private_context_entry(entry: &ContextEntry) -> ContextEntry {
	match entry {
		ContextEntry::Value(value) => ContextEntry::Value(ContextValue {
			type_: value.type_.clone(),
			mutable: value.mutable,
			visibility: Visibility::Private,
		}),
		ContextEntry::Impl { parent, members } => ContextEntry::Impl {
			parent: Box::new(ContextValue {
				type_: parent.type_.clone(),
				mutable: parent.mutable,
				visibility: Visibility::Private,
			}),
			members: members.clone(),
		},
	}
}

const BUILTIN_RANGE_CONSTRUCTOR: &str = "__nymph_builtin_range_Range";
const BUILTIN_RANGE_FROM_CONSTRUCTOR: &str = "__nymph_builtin_range_RangeFrom";
const BUILTIN_RANGE_TO_CONSTRUCTOR: &str = "__nymph_builtin_range_RangeTo";
const BUILTIN_RANGE_INCLUSIVE_CONSTRUCTOR: &str = "__nymph_builtin_range_RangeInclusive";
const BUILTIN_RANGE_TO_INCLUSIVE_CONSTRUCTOR: &str = "__nymph_builtin_range_RangeToInclusive";
const BUILTIN_RANGE_ITEMS: [(&str, &str); 5] = [
	("Range", BUILTIN_RANGE_CONSTRUCTOR),
	("RangeFrom", BUILTIN_RANGE_FROM_CONSTRUCTOR),
	("RangeTo", BUILTIN_RANGE_TO_CONSTRUCTOR),
	("RangeInclusive", BUILTIN_RANGE_INCLUSIVE_CONSTRUCTOR),
	("RangeToInclusive", BUILTIN_RANGE_TO_INCLUSIVE_CONSTRUCTOR),
];

/// Unique identifier for type variables, to distinguish variables with the same name in different scopes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeVarId(u64);

/// Result type for processing struct/enum members
type ProcessMembersResult = (
	BTreeMap<EcoString, StructMember>,
	BTreeMap<EcoString, Type>,
	Vec<ImplRecord>,
);

/// Resolved generic parameter info (for type constructors)
#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct GenericParamInfo {
	pub id: TypeVarId,
	pub name: EcoString,
	pub constraint: Option<Type>,
	pub default: Option<Type>,
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub enum Type {
	Int,
	UInt,
	Float,
	Char,
	String,
	Boolean,
	Void,
	Never,
	Intersection {
		first: Box<Self>,
		second: Box<Self>,
	},
	List {
		item: Box<Self>,
	},
	Tuple {
		items: Vec<Self>,
	},
	Map {
		key: Box<Self>,
		value: Box<Self>,
	},
	Function {
		generics: Arc<Vec<GenericParamInfo>>,
		params: Vec<(Option<EcoString>, Self)>,
		has_spread: bool,
		return_type: Box<Self>,
		/// Whether this function is a struct constructor (needs `new` in JS emission).
		constructor: bool,
	},
	Variable {
		id: TypeVarId,
		name: EcoString,
		constraint: Option<Box<Self>>,
	},
	Struct {
		name: EcoString,
		def_key: Option<DefKey>,
		generics: Arc<Vec<GenericParamInfo>>,
		type_args: Vec<Self>,
		fields: Arc<BTreeMap<EcoString, Self>>,
		members: Arc<BTreeMap<EcoString, StructMember>>,
		impls: Arc<BTreeMap<EcoString, Self>>,
	},
	Enum {
		name: EcoString,
		def_key: Option<DefKey>,
		generics: Arc<Vec<GenericParamInfo>>,
		type_args: Vec<Self>,
		variants: Arc<BTreeMap<EcoString, BTreeMap<EcoString, Self>>>,
		members: Arc<BTreeMap<EcoString, StructMember>>,
		impls: Arc<BTreeMap<EcoString, Self>>,
	},
	EnumVariant {
		name: EcoString,
		variant_name: EcoString,
		fields: Arc<BTreeMap<EcoString, Self>>,
		variant_of: Box<Self>,
		impls: Arc<BTreeMap<EcoString, Self>>,
	},
	Interface {
		name: EcoString,
		def_key: Option<DefKey>,
		generics: Arc<Vec<GenericParamInfo>>,
		type_args: Vec<Self>,
		members: Arc<BTreeMap<EcoString, StructMember>>,
		impls: Arc<BTreeMap<EcoString, Self>>,
	},
	Module {
		name: EcoString,
		members: Arc<BTreeMap<EcoString, ContextEntry>>,
	},
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct StructMember {
	pub type_: Box<Type>,
	pub kind: StructMemberKind,
	pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructMemberKind {
	Namespace,
	Mutable,
	Immutable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplRecord {
	pub generics: Arc<Vec<GenericParamInfo>>,
	pub receiver: Type,
	pub interface: Type,
	pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceExtensionRecord {
	pub generics: Arc<Vec<GenericParamInfo>>,
	pub interface: Type,
	pub members: BTreeMap<EcoString, StructMember>,
	pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExtensionRecord {
	pub generics: Arc<Vec<GenericParamInfo>>,
	pub receiver: Type,
	pub members: BTreeMap<EcoString, StructMember>,
	pub span: Range<usize>,
}

impl Type {
	fn assignable_to(&self, target: &Self, ctx: &Context) -> bool {
		match (self, target) {
			(a, b) if a == b => true,
			(Type::Never, _) => true,
			(_, Type::Never) => false,
			(_, Type::Variable { .. }) | (Type::Variable { .. }, _) => true,
			(Type::Intersection { first, second }, target) => {
				self.intersection_to_type(first, second, target, ctx)
			}
			(source, Type::Intersection { first, second }) => {
				source.assignable_to(first, ctx) && source.assignable_to(second, ctx)
			}
			(Type::List { item: item_a }, Type::List { item: item_b }) => {
				item_a.assignable_to(item_b, ctx)
			}
			(Type::Tuple { items: items_a }, Type::Tuple { items: items_b }) => {
				items_a.len() == items_b.len()
					&& items_a
						.iter()
						.zip(items_b)
						.all(|(a, b)| a.assignable_to(b, ctx))
			}
			(
				Type::Map {
					key: key_a,
					value: value_a,
				},
				Type::Map {
					key: key_b,
					value: value_b,
				},
			) => key_a.assignable_to(key_b, ctx) && value_a.assignable_to(value_b, ctx),
			(
				Type::Function {
					params: params_a,
					has_spread: _spread_a,
					return_type: return_a,
					..
				},
				Type::Function {
					params: params_b,
					has_spread: _spread_b,
					return_type: return_b,
					..
				},
			) => {
				return_a.assignable_to(return_b, ctx)
					&& params_a.len() == params_b.len()
					&& params_a
						.iter()
						.zip(params_b)
						.all(|((_, a), (_, b))| b.assignable_to(a, ctx))
			}
			_ => false,
		}
	}

	fn join(&self, other: &Self) -> Self {
		if self == other {
			self.clone()
		} else {
			match (self, other) {
				(Type::Never, t) | (t, Type::Never) => t.clone(),
				(a, b) if a.assignable_to(b, &Context::default()) => b.clone(),
				(a, b) if b.assignable_to(a, &Context::default()) => a.clone(),
				_ => Type::Never,
			}
		}
	}

	fn intersection_to_type(
		&self,
		first: &Self,
		second: &Self,
		target: &Self,
		ctx: &Context,
	) -> bool {
		match self.normalize_intersection(first, second, ctx) {
			Type::Never => true,
			normalized => normalized.assignable_to(target, ctx),
		}
	}

	fn normalize_intersection(&self, first: &Self, second: &Self, ctx: &Context) -> Self {
		if first == second {
			return first.clone();
		}

		if matches!(first, Type::Never) || matches!(second, Type::Never) {
			return Type::Never;
		}

		if first.assignable_to(second, ctx) {
			return first.clone();
		}

		if second.assignable_to(first, ctx) {
			return second.clone();
		}

		match (first, second) {
			(Type::Int, Type::UInt)
			| (Type::Int, Type::Float)
			| (Type::Int, Type::Char)
			| (Type::Int, Type::String)
			| (Type::Int, Type::Boolean)
			| (Type::UInt, Type::Float)
			| (Type::UInt, Type::Char)
			| (Type::UInt, Type::String)
			| (Type::UInt, Type::Boolean)
			| (Type::Float, Type::Char)
			| (Type::Float, Type::String)
			| (Type::Float, Type::Boolean)
			| (Type::Char, Type::String)
			| (Type::Char, Type::Boolean)
			| (Type::String, Type::Boolean) => Type::Never,
			(Type::List { item: first_item }, Type::List { item: second_item }) => {
				let item = self.normalize_intersection(first_item, second_item, ctx);
				if matches!(item, Type::Never) {
					Type::Never
				} else {
					Type::List {
						item: Box::new(item),
					}
				}
			}
			(
				Type::Tuple { items: first_items },
				Type::Tuple {
					items: second_items,
				},
			) if first_items.len() == second_items.len() => {
				let mut items = Vec::with_capacity(first_items.len());
				for (first_item, second_item) in first_items.iter().zip(second_items) {
					let item = self.normalize_intersection(first_item, second_item, ctx);
					if matches!(item, Type::Never) {
						return Type::Never;
					}
					items.push(item);
				}
				Type::Tuple { items }
			}
			(
				Type::Map {
					key: first_key,
					value: first_value,
				},
				Type::Map {
					key: second_key,
					value: second_value,
				},
			) => {
				let key = self.normalize_intersection(first_key, second_key, ctx);
				let value = self.normalize_intersection(first_value, second_value, ctx);
				if matches!(key, Type::Never) || matches!(value, Type::Never) {
					Type::Never
				} else {
					Type::Map {
						key: Box::new(key),
						value: Box::new(value),
					}
				}
			}
			_ => Type::Intersection {
				first: Box::new(first.clone()),
				second: Box::new(second.clone()),
			},
		}
	}
}

impl Display for Type {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Type::Int => write!(f, "int"),
			Type::UInt => write!(f, "uint"),
			Type::Float => write!(f, "float"),
			Type::Char => write!(f, "char"),
			Type::String => write!(f, "string"),
			Type::Boolean => write!(f, "boolean"),
			Type::Void => write!(f, "void"),
			Type::Never => write!(f, "never"),
			Type::Intersection { first, second } => write!(f, "{} + {}", first, second),
			Type::List { item } => write!(f, "#[{}]", item),
			Type::Tuple { items } => {
				write!(f, "#(")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", item)?;
				}
				write!(f, ")")
			}
			Type::Map { key, value } => write!(f, "#{{{}: {}}}", key, value),
			Type::Function {
				generics,
				params,
				return_type,
				has_spread,
				..
			} => {
				if !generics.is_empty() {
					write!(f, "<")?;
					for (i, g) in generics.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", g.name)?;
						if let Some(c) = &g.constraint {
							write!(f, ": {}", c)?;
						}
					}
					write!(f, ">")?;
				}
				write!(f, "(")?;
				for (i, (name, param)) in params.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					if let Some(n) = name {
						write!(f, "{}: ", n)?;
					}
					if *has_spread && i == params.len() - 1 {
						write!(f, "...")?;
					}
					write!(f, "{}", param)?;
				}
				write!(f, ") -> {}", return_type)
			}
			Type::Variable {
				name, constraint, ..
			} => {
				if let Some(c) = constraint {
					write!(f, "{}: {}", name, c)
				} else {
					write!(f, "{}", name)
				}
			}
			Type::Struct {
				name,
				type_args,
				fields,
				..
			} => {
				write!(f, "{}", name)?;
				if !type_args.is_empty() {
					write!(
						f,
						"<{}>",
						type_args.iter().map(|t| t.to_string()).join(", ")
					)?;
				}
				if !fields.is_empty() {
					write!(
						f,
						"({})",
						fields
							.iter()
							.map(|(k, v)| format!("{}: {}", k, v))
							.join(", ")
					)?;
				}
				Ok(())
			}
			Type::Enum {
				name, type_args, ..
			} => {
				write!(f, "{}", name)?;
				if !type_args.is_empty() {
					write!(
						f,
						"<{}>",
						type_args.iter().map(|t| t.to_string()).join(", ")
					)?;
				}
				Ok(())
			}
			Type::EnumVariant {
				name,
				variant_name,
				fields,
				..
			} => {
				write!(f, "{}.{}", name, variant_name)?;
				if !fields.is_empty() {
					write!(
						f,
						"({})",
						fields
							.iter()
							.map(|(k, v)| format!("{}: {}", k, v))
							.join(", ")
					)?;
				}
				Ok(())
			}
			Type::Interface {
				name, type_args, ..
			} => {
				write!(f, "{}", name)?;
				if !type_args.is_empty() {
					write!(
						f,
						"<{}>",
						type_args.iter().map(|t| t.to_string()).join(", ")
					)?;
				}
				Ok(())
			}
			Type::Module { name, .. } => write!(f, "module {}", name),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct Context {
	pub local_ctx: HashMap<EcoString, ContextEntry>,
	pub file_ctx: HashMap<EcoString, HashSet<(EcoString, ContextEntry)>>,
	/// Top-level `impl Interface for Type` registrations.
	pub impl_records: Vec<ImplRecord>,
	/// Top-level `impl Interface<...> { ... }` extension members.
	pub interface_extensions: Vec<InterfaceExtensionRecord>,
	/// Top-level `impl Type { ... }` extension members.
	pub type_extensions: Vec<TypeExtensionRecord>,
	/// The current `self` type (for interface definitions and impl blocks)
	pub self_type: Option<Type>,
	/// The next type variable ID to use (threaded through salsa queries to avoid ID collisions)
	pub next_type_var_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextEntry {
	Value(ContextValue),
	Impl {
		parent: Box<ContextValue>,
		members: BTreeMap<EcoString, StructMember>,
	},
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextValue {
	pub type_: Type,
	pub mutable: bool,
	pub visibility: Visibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AvailableInterface {
	interface: Type,
	span: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemberCandidate {
	interface: Type,
	member: StructMember,
	span: Option<Range<usize>>,
}

impl Context {
	pub fn with_new_entry(&self, name: EcoString, entry: ContextEntry) -> Self {
		let mut new_ctx = self.clone();
		new_ctx.local_ctx.insert(name, entry);
		new_ctx
	}

	/// Add an entry in place without cloning the context.
	pub fn insert_entry(&mut self, name: EcoString, entry: ContextEntry) {
		self.local_ctx.insert(name, entry);
	}

	pub fn with_new_entries<I: IntoIterator<Item = (EcoString, ContextEntry)>>(
		&self,
		entries: I,
	) -> Self {
		let mut new_ctx = self.clone();
		for (name, entry) in entries {
			new_ctx.local_ctx.insert(name, entry);
		}
		new_ctx
	}

	/// Look up a type by name in the context
	pub fn lookup_type(&self, name: &EcoString) -> Option<Type> {
		self.local_ctx.get(name).map(|entry| match entry {
			ContextEntry::Value(val) => val.type_.clone(),
			ContextEntry::Impl { parent, .. } => parent.type_.clone(),
		})
	}

	/// Look up a type by name in the context, returning a reference.
	pub fn lookup_type_ref(&self, name: &EcoString) -> Option<&Type> {
		self.local_ctx.get(name).map(|entry| match entry {
			ContextEntry::Value(val) => &val.type_,
			ContextEntry::Impl { parent, .. } => &parent.type_,
		})
	}

	pub fn with_impl_record(&self, record: ImplRecord) -> Self {
		let mut new_ctx = self.clone();
		new_ctx.impl_records.push(record);
		new_ctx
	}

	pub fn with_interface_extension(&self, record: InterfaceExtensionRecord) -> Self {
		let mut new_ctx = self.clone();
		new_ctx.interface_extensions.push(record);
		new_ctx
	}

	pub fn with_type_extension(&self, record: TypeExtensionRecord) -> Self {
		let mut new_ctx = self.clone();
		new_ctx.type_extensions.push(record);
		new_ctx
	}

	/// Set the `self` type for interface definitions and impl blocks
	pub fn with_self_type(&self, self_type: Type) -> Self {
		let mut new_ctx = self.clone();
		new_ctx.self_type = Some(self_type);
		new_ctx
	}
}

/// Status of a module in the cache (for cycle detection)
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
enum ModuleStatus {
	/// Module is currently being processed; keep a predeclared context available for cycles.
	InProgress(Context),
	/// Module has been fully processed
	Complete(Context),
}

#[derive(Clone, Debug)]
pub struct TypeChecker {
	/// Counter for generating unique type variable IDs
	pub next_type_var_id: u64,
	/// Cache of processed modules (absolute path -> status) — used in non-salsa mode
	module_cache: HashMap<PathBuf, ModuleStatus>,
	/// The project root directory (where nymph.toml is located)
	project_root: Option<PathBuf>,
	/// The current file being processed (for resolving relative imports)
	current_file: Option<PathBuf>,
	implicit_prelude: bool,
}

impl Default for TypeChecker {
	fn default() -> Self {
		Self::new(None)
	}
}

impl TypeChecker {
	/// Create a new TypeChecker with the given file path (non-salsa mode)
	pub fn new(file_path: Option<PathBuf>) -> Self {
		let project_root = file_path.as_ref().and_then(|p| Self::find_project_root(p));
		let implicit_prelude = project_root
			.as_ref()
			.and_then(|root| config::implicit_prelude_enabled(root).ok())
			.unwrap_or(true);
		Self {
			next_type_var_id: 0,
			module_cache: HashMap::new(),
			project_root,
			current_file: file_path,
			implicit_prelude,
		}
	}

	/// Create a new TypeChecker for use with salsa queries
	pub fn with_salsa(
		file_path: PathBuf,
		project_root: PathBuf,
		next_type_var_id: u64,
		implicit_prelude: bool,
	) -> Self {
		Self {
			next_type_var_id,
			module_cache: HashMap::new(),
			project_root: Some(project_root),
			current_file: Some(file_path),
			implicit_prelude,
		}
	}

	fn fresh_type_var_id(&mut self) -> TypeVarId {
		let id = self.next_type_var_id;
		self.next_type_var_id += 1;
		TypeVarId(id)
	}

	pub fn fresh_var(&mut self, name: impl Into<EcoString>, constraint: Option<Type>) -> Type {
		Type::Variable {
			id: self.fresh_type_var_id(),
			name: name.into(),
			constraint: constraint.map(Box::new),
		}
	}

	fn resolve_func_param_type(
		&mut self,
		param: &Spanned<ast::declaration::FuncParam>,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let ty = self.resolve_ast_type(&param.0.type_.0, param.0.type_.1, ctx)?;
		if param.0.spread {
			Ok(Type::List { item: Box::new(ty) })
		} else {
			Ok(ty)
		}
	}

	/// Find the project root by searching for nymph.toml
	pub fn find_project_root(start: &Path) -> Option<PathBuf> {
		// If it's a file (or looks like a file with an extension), start from parent
		// We check for extension as the file might not exist yet (unsaved buffer)
		let mut current = if start.is_file() || start.extension().is_some() {
			start.parent()?.to_path_buf()
		} else {
			start.to_path_buf()
		};

		loop {
			let toml_path = current.join("nymph.toml");
			if toml_path.exists() {
				return Some(current);
			}
			if !current.pop() {
				return None;
			}
		}
	}

	fn project_module_path(&self, path: &[&str]) -> Option<PathBuf> {
		let mut module_path = self.project_root.as_ref()?.join("src");
		for segment in path {
			module_path = module_path.join(segment);
		}

		let file_path = module_path.with_extension("nym");
		let dir_path = module_path.join("mod.nym");

		match (file_path.exists(), dir_path.exists()) {
			(true, false) => Some(file_path),
			(false, true) | (true, true) => Some(dir_path),
			(false, false) => None,
		}
	}

	fn builtin_range_module_path(&self) -> Option<PathBuf> {
		self.project_module_path(&["range"])
	}

	fn is_current_module(&self, module_path: &Path) -> bool {
		let Some(current_file) = &self.current_file else {
			return false;
		};

		let current_file = current_file
			.canonicalize()
			.unwrap_or_else(|_| current_file.clone());
		let module_path = module_path
			.canonicalize()
			.unwrap_or_else(|_| module_path.to_path_buf());

		current_file == module_path
	}

	fn is_current_implicit_prelude_module(&self) -> bool {
		IMPLICIT_PRELUDE_MODULES.iter().any(|module| {
			self
				.project_module_path(module.path)
				.as_ref()
				.is_some_and(|path| self.is_current_module(path))
		}) || self
			.builtin_range_module_path()
			.as_ref()
			.is_some_and(|path| self.is_current_module(path))
	}

	fn should_inject_implicit_prelude(&self, module: &Module) -> bool {
		if !self.implicit_prelude {
			return false;
		}

		if self.is_current_implicit_prelude_module() {
			return false;
		}

		!module.members.iter().any(|decl| {
			let Declaration::Import {
				root: ImportRoot::Project,
				path,
				..
			} = decl
			else {
				return false;
			};

			IMPLICIT_PRELUDE_MODULES.iter().any(|module| {
				path.len() == module.path.len()
					&& path
						.iter()
						.zip(module.path.iter())
						.all(|(segment, expected)| segment.0.as_ref() == *expected)
			})
		})
	}

	fn implicit_prelude_entry(
		base_ctx: &Context,
		module_ctx: &Context,
		name: &str,
	) -> Option<(EcoString, ContextEntry)> {
		let name = EcoString::from(name);
		if base_ctx.local_ctx.contains_key(&name) {
			return None;
		}

		let entry = module_ctx.local_ctx.get(&name)?;
		let visibility = match entry {
			ContextEntry::Value(value) => value.visibility,
			ContextEntry::Impl { parent, .. } => parent.visibility,
		};

		(visibility == Visibility::Public).then(|| (name, private_context_entry(entry)))
	}

	fn implicit_prelude_entries(
		base_ctx: &Context,
		module_ctx: &Context,
		name: &str,
	) -> Vec<(EcoString, ContextEntry)> {
		let Some((name, entry)) = Self::implicit_prelude_entry(base_ctx, module_ctx, name) else {
			return Vec::new();
		};

		let mut entries = vec![(name, entry.clone())];
		for (variant_name, variant_entry) in Self::enum_variant_context_entries(
			match &entry {
				ContextEntry::Value(value) => &value.type_,
				ContextEntry::Impl { parent, .. } => &parent.type_,
			},
			Visibility::Private,
		) {
			if !base_ctx.local_ctx.contains_key(&variant_name) {
				entries.push((variant_name, variant_entry));
			}
		}

		entries
	}

	fn merge_module_effects(&self, base_ctx: &Context, module_ctx: &Context) -> Context {
		let mut next_ctx = base_ctx.clone();

		for record in &module_ctx.impl_records {
			if !next_ctx.impl_records.contains(record) {
				next_ctx.impl_records.push(record.clone());
			}
		}

		for record in &module_ctx.interface_extensions {
			if !next_ctx.interface_extensions.contains(record) {
				next_ctx.interface_extensions.push(record.clone());
			}
		}

		for record in &module_ctx.type_extensions {
			if !next_ctx.type_extensions.contains(record) {
				next_ctx.type_extensions.push(record.clone());
			}
		}

		next_ctx
	}

	fn inject_implicit_prelude_entries(&mut self, ctx: &Context) -> Context {
		if self.is_current_implicit_prelude_module() {
			return ctx.clone();
		}

		let mut next_ctx = ctx.clone();

		for module in IMPLICIT_PRELUDE_MODULES {
			let Some(module_path) = self.project_module_path(module.path) else {
				continue;
			};

			if self.is_current_module(&module_path) {
				continue;
			}

			let Ok(module_ctx) = self.load_module(&module_path, Span::new(0, 0), &next_ctx) else {
				continue;
			};
			next_ctx = self.merge_module_effects(&next_ctx, &module_ctx);

			for name in module.names {
				for (name, entry) in Self::implicit_prelude_entries(&next_ctx, &module_ctx, name) {
					next_ctx.insert_entry(name, entry);
				}
			}
		}

		next_ctx
	}

	fn inject_implicit_prelude_entries_salsa(
		&mut self,
		db: &dyn Db,
		config: ProjectConfig,
		ctx: &Context,
	) -> Context {
		if self.is_current_implicit_prelude_module() {
			return ctx.clone();
		}

		let mut next_ctx = ctx.clone();

		for module in IMPLICIT_PRELUDE_MODULES {
			let Some(module_path) = self.project_module_path(module.path) else {
				continue;
			};

			if self.is_current_module(&module_path) {
				continue;
			}

			let imported_file = queries::load_source_file(db, module_path.to_string_lossy().to_string());
			let module_ctx = self.check_file_salsa(db, imported_file, config);
			next_ctx = self.merge_module_effects(&next_ctx, &module_ctx);

			for name in module.names {
				for (name, entry) in Self::implicit_prelude_entries(&next_ctx, &module_ctx, name) {
					next_ctx.insert_entry(name, entry);
				}
			}
		}

		next_ctx
	}

	fn with_builtin_range_entries(&self, ctx: &Context, module_ctx: &Context) -> Context {
		let mut next_ctx = ctx.clone();

		for (public_name, hidden_name) in BUILTIN_RANGE_ITEMS {
			let public_name = EcoString::from(public_name);
			let hidden_name = EcoString::from(hidden_name);

			if next_ctx.local_ctx.contains_key(&hidden_name) {
				continue;
			}

			if let Some(entry) = module_ctx.local_ctx.get(&public_name) {
				let hidden_entry = match entry {
					ContextEntry::Value(value) => ContextEntry::Value(ContextValue {
						type_: value.type_.clone(),
						mutable: value.mutable,
						visibility: Visibility::Private,
					}),
					ContextEntry::Impl { parent, members } => ContextEntry::Impl {
						parent: Box::new(ContextValue {
							type_: parent.type_.clone(),
							mutable: parent.mutable,
							visibility: Visibility::Private,
						}),
						members: members.clone(),
					},
				};
				next_ctx.insert_entry(hidden_name, hidden_entry);
			}
		}

		next_ctx
	}

	fn inject_builtin_range_entries(&mut self, ctx: &Context) -> Context {
		let Some(range_path) = self.builtin_range_module_path() else {
			return ctx.clone();
		};

		if self.is_current_module(&range_path) {
			return ctx.clone();
		}

		let base_ctx = Context::default();
		let Ok(module_ctx) = self.load_module(&range_path, Span::new(0, 0), &base_ctx) else {
			return ctx.clone();
		};

		self.with_builtin_range_entries(ctx, &module_ctx)
	}

	fn inject_builtin_range_entries_salsa(
		&mut self,
		db: &dyn Db,
		config: ProjectConfig,
		ctx: &Context,
	) -> Context {
		let Some(range_path) = self.builtin_range_module_path() else {
			return ctx.clone();
		};

		if self.is_current_module(&range_path) {
			return ctx.clone();
		}

		let range_file = queries::load_source_file(db, range_path.to_string_lossy().to_string());
		let module_ctx = self.check_file_salsa(db, range_file, config);
		self.with_builtin_range_entries(ctx, &module_ctx)
	}

	/// Resolve an import path to an absolute file path
	pub fn resolve_import_path(
		&self,
		root: &ImportRoot,
		path: &[ast::Ident],
		span: Span,
	) -> Result<PathBuf, TypeError> {
		let base_dir = match root {
			ImportRoot::Package(Spanned(pkg_name, pkg_span)) => {
				return Err(TypeError {
					kind: TypeErrorKind::ExternalDependencyNotSupported {
						package: pkg_name.clone(),
					},
					span: span_to_range(*pkg_span),
				});
			}
			ImportRoot::Project => {
				let root = self.project_root.clone().ok_or_else(|| {
					let searched_from = self
						.current_file
						.as_ref()
						.map(|p| p.display().to_string())
						.unwrap_or_else(|| "<unknown>".to_string());
					TypeError {
						kind: TypeErrorKind::ProjectRootNotFound {
							searched_from: searched_from.into(),
						},
						span: span_to_range(span),
					}
				})?;
				// Source files are in the src/ subdirectory
				root.join("src")
			}
			ImportRoot::Current => self
				.current_file
				.as_ref()
				.and_then(|p| p.parent())
				.map(|p| p.to_path_buf())
				.ok_or_else(|| TypeError {
					kind: TypeErrorKind::ProjectRootNotFound {
						searched_from: "<unknown>".into(),
					},
					span: span_to_range(span),
				})?,
			ImportRoot::Parent => self
				.current_file
				.as_ref()
				.and_then(|p| p.parent())
				.and_then(|p| p.parent())
				.map(|p| p.to_path_buf())
				.ok_or_else(|| TypeError {
					kind: TypeErrorKind::ProjectRootNotFound {
						searched_from: "<unknown>".into(),
					},
					span: span_to_range(span),
				})?,
		};

		// Build the path from segments
		let mut module_path = base_dir;
		for segment in path {
			module_path = module_path.join(segment.0.as_str());
		}

		// Check for both foo.nym and foo/mod.nym
		let file_path = module_path.with_extension("nym");
		let dir_path = module_path.join("mod.nym");

		let file_exists = file_path.exists();
		let dir_exists = dir_path.exists();

		match (file_exists, dir_exists) {
			(true, true) => Err(TypeError {
				kind: TypeErrorKind::AmbiguousModule {
					path: path.iter().map(|s| s.0.as_str()).join("/").into(),
					file_path: file_path.display().to_string().into(),
					dir_path: dir_path.display().to_string().into(),
				},
				span: span_to_range(span),
			}),
			(true, false) => Ok(file_path),
			(false, true) => Ok(dir_path),
			(false, false) => Err(TypeError {
				kind: TypeErrorKind::ModuleNotFound {
					path: path.iter().map(|s| s.0.as_str()).join("/").into(),
				},
				span: span_to_range(span),
			}),
		}
	}

	/// Load and type-check a module, returning its exported context
	fn load_module(
		&mut self,
		module_path: &Path,
		span: Span,
		base_ctx: &Context,
	) -> Result<Context, TypeError> {
		let abs_path = module_path.canonicalize().map_err(|_| TypeError {
			kind: TypeErrorKind::ModuleNotFound {
				path: module_path.display().to_string().into(),
			},
			span: span_to_range(span),
		})?;

		// Check cache
		if let Some(status) = self.module_cache.get(&abs_path) {
			return match status {
				ModuleStatus::InProgress(ctx) => Ok(ctx.clone()),
				ModuleStatus::Complete(ctx) => Ok(ctx.clone()),
			};
		}

		// Read and parse the module
		let source = fs::read_to_string(&abs_path).map_err(|_| TypeError {
			kind: TypeErrorKind::ModuleNotFound {
				path: abs_path.display().to_string().into(),
			},
			span: span_to_range(span),
		})?;

		let filename: EcoString = abs_path.display().to_string().into();
		let db = NymphDatabase::default();
		let file = SourceFile::new(&db, filename.to_string(), source.to_string());
		let result = queries::parse_file(&db, file);
		let parse_errors: Vec<_> = queries::parse_file::accumulated::<Diagnostics>(&db, file)
			.into_iter()
			.filter(|d| d.0.kind == DiagnosticKind::ParseError)
			.collect();

		if let Some(first) = parse_errors.first() {
			let diag = &first.0;
			return Err(TypeError {
				kind: TypeErrorKind::ModuleParseError {
					module_path: filename,
					message: diag.message.clone().into(),
				},
				span: diag.span.start..diag.span.end,
			});
		}

		let module = result.module.ok_or_else(|| TypeError {
			kind: TypeErrorKind::ModuleParseError {
				module_path: filename,
				message: "Failed to parse module".into(),
			},
			span: 0..0,
		})?;

		let predeclared_ctx = self.predeclare_module(&module.0);
		self
			.module_cache
			.insert(abs_path.clone(), ModuleStatus::InProgress(predeclared_ctx));

		// Save current file and set new one
		let prev_file = self.current_file.take();
		self.current_file = Some(abs_path.clone());

		// Type-check the module
		let module_ctx = self.check_module(&module.0, base_ctx).map_err(|e| {
			let span = e.span.clone();
			TypeError {
				kind: TypeErrorKind::ModuleTypeError {
					module_path: abs_path.display().to_string().into(),
					error: Box::new(e),
				},
				span,
			}
		})?;

		// Restore previous file
		self.current_file = prev_file;

		// Cache the result
		self
			.module_cache
			.insert(abs_path, ModuleStatus::Complete(module_ctx.clone()));

		Ok(module_ctx)
	}

	/// Extract public exports from a module context as a Module type
	fn context_to_module_type(&self, name: EcoString, ctx: &Context) -> Type {
		let mut members = BTreeMap::new();

		for (entry_name, entry) in &ctx.local_ctx {
			// Only include public entries
			let visibility = match entry {
				ContextEntry::Value(val) => val.visibility,
				ContextEntry::Impl { parent, .. } => parent.visibility,
			};

			if visibility == Visibility::Public {
				members.insert(entry_name.clone(), entry.clone());
			}
		}

		Type::Module {
			name,
			members: Arc::new(members),
		}
	}

	/// Infer the type of an expression in a given context
	pub fn infer(&mut self, expr: &Spanned<Expr>, ctx: &Context) -> Result<Type, TypeError> {
		self.infer_with_expected(expr, None, ctx)
	}

	fn infer_with_expected(
		&mut self,
		expr: &Spanned<Expr>,
		expected: Option<&Type>,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let placeholders = anonymous_params(expr);
		if placeholders.is_empty() {
			return self.infer_expr(&expr.0, expr.1, ctx);
		}

		if matches!(expected, Some(Type::Function { .. })) {
			return self.infer_anonymous_function(expr, expected, ctx, &placeholders);
		} else {
			self.infer_expr(&expr.0, expr.1, ctx).map_err(|error| {
				if matches!(
					error.kind,
					TypeErrorKind::CannotInferAnonymousFunction { .. }
				) {
					TypeError {
						kind: TypeErrorKind::CannotInferAnonymousFunction {
							placeholders: anonymous_param_syntaxes(expr),
						},
						span: error.span,
					}
				} else {
					error
				}
			})
		}
	}

	fn infer_anonymous_function(
		&mut self,
		expr: &Spanned<Expr>,
		expected: Option<&Type>,
		ctx: &Context,
		placeholders: &BTreeMap<usize, Span>,
	) -> Result<Type, TypeError> {
		let Some(Type::Function {
			generics,
			params,
			has_spread,
			return_type,
			constructor: _,
		}) = expected
		else {
			return Err(TypeError {
				kind: TypeErrorKind::CannotInferAnonymousFunction {
					placeholders: anonymous_param_syntaxes(expr),
				},
				span: span_to_range(expr.1),
			});
		};

		let required_params = placeholders.keys().next_back().map_or(0, |index| index + 1);
		if params.len() < required_params {
			let found = Type::Function {
				generics: Arc::new(Vec::new()),
				params: (0..required_params)
					.map(|index| (None, self.fresh_var(format!("$anon{index}"), None)))
					.collect(),
				has_spread: false,
				return_type: Box::new(self.fresh_var("_anon_return", None)),
				constructor: false,
			};
			return Err(TypeError {
				kind: TypeErrorKind::TypeMismatch {
					expected: Box::new(expected.cloned().unwrap()),
					found: Box::new(found),
				},
				span: span_to_range(expr.1),
			});
		}

		let mut rewritten_names = BTreeMap::new();
		let mut closure_ctx = ctx.clone();
		for (index, param) in params.iter().enumerate().take(required_params) {
			let name: EcoString = format!("__anon_param_{index}").into();
			rewritten_names.insert(index, name.clone());
			closure_ctx.insert_entry(
				name,
				ContextEntry::Value(ContextValue {
					type_: param.1.clone(),
					mutable: false,
					visibility: Visibility::Private,
				}),
			);
		}

		let rewritten = rewrite_anonymous_params(expr, &rewritten_names);
		let body_type = self.infer_expr(&rewritten.0, rewritten.1, &closure_ctx)?;
		if !self.type_satisfies(&body_type, return_type, &closure_ctx) {
			return Err(TypeError {
				kind: TypeErrorKind::TypeMismatch {
					expected: return_type.clone(),
					found: Box::new(body_type.clone()),
				},
				span: span_to_range(expr.1),
			});
		}

		Ok(Type::Function {
			generics: generics.clone(),
			params: params.clone(),
			has_spread: *has_spread,
			return_type: Box::new(body_type),
			constructor: false,
		})
	}

	fn infer_expr(&mut self, expr: &Expr, span: Span, ctx: &Context) -> Result<Type, TypeError> {
		match expr {
			Expr::Int(_) => Ok(Type::Int),
			Expr::UInt(_) => Ok(Type::UInt),
			Expr::Float(_) => Ok(Type::Float),
			Expr::Char(_) => Ok(Type::Char),
			Expr::String(_) => Ok(Type::String),
			Expr::Boolean(_) => Ok(Type::Boolean),
			Expr::Identifier(ident_spanned) => {
				let Spanned(name, ident_span) = ident_spanned;
				ctx
					.local_ctx
					.get(name)
					.map(|entry| match entry {
						ContextEntry::Value(val) => val.type_.clone(),
						ContextEntry::Impl { parent, .. } => parent.type_.clone(),
					})
					.ok_or_else(|| TypeError {
						kind: TypeErrorKind::UnknownIdentifier {
							name: name.clone(),
							suggestion: find_similar_name(name, ctx.local_ctx.keys()),
						},
						span: span_to_range(*ident_span),
					})
			}
			Expr::List(items) => {
				if items.is_empty() {
					// Empty list: can't infer element type
					Ok(Type::List {
						item: Box::new(self.fresh_var("_infer", None)),
					})
				} else {
					let item_type = match &items[0].0 {
						ListItem::Expr(val) => self.infer(val, ctx)?,
						ListItem::Spread(val) => match self.infer(val, ctx)? {
							Type::List { item } => *item,
							other => other,
						},
					};
					Ok(Type::List {
						item: Box::new(item_type),
					})
				}
			}
			Expr::Tuple(items) => {
				let types: Result<Vec<_>, _> = items
					.iter()
					.map(|item| match &item.0 {
						ListItem::Expr(val) => self.infer(val, ctx),
						ListItem::Spread(_) => Err(TypeError {
							kind: TypeErrorKind::SpreadNonFinalParam,
							span: span_to_range(item.1),
						}),
					})
					.collect();
				Ok(Type::Tuple { items: types? })
			}
			Expr::Map(entries) => {
				if entries.is_empty() {
					Ok(Type::Map {
						key: Box::new(self.fresh_var("_infer_key", None)),
						value: Box::new(self.fresh_var("_infer_val", None)),
					})
				} else {
					let (key_type, value_type) = match &entries[0].0 {
						ast::expr::MapEntry::Expr(k, v) => {
							let kt = self.infer(k, ctx)?;
							let vt = self.infer(v, ctx)?;
							(kt, vt)
						}
						ast::expr::MapEntry::Spread(val) => match self.infer(val, ctx)? {
							Type::Map { key, value } => (*key, *value),
							other => (other.clone(), other),
						},
					};
					Ok(Type::Map {
						key: Box::new(key_type),
						value: Box::new(value_type),
					})
				}
			}
			Expr::Range(range) => self.infer_range_expr(range, span, ctx),
			Expr::Call {
				func,
				generics,
				args,
			} => {
				let func_type = self.infer(func, ctx)?;
				match func_type {
					Type::Function {
						generics: func_generics,
						params,
						return_type,
						..
					} => {
						if func_generics.is_empty() {
							for arg in args {
								if let Some(name_ident) = &arg.0.name {
									let name = &name_ident.0;
									if !params
										.iter()
										.any(|(p_name, _)| p_name.as_ref().is_some_and(|n| n == name))
									{
										let suggestion =
											find_similar_name(name, params.iter().filter_map(|(n, _)| n.as_ref()));
										return Err(TypeError {
											kind: TypeErrorKind::UnknownNamedArgument {
												name: name.clone(),
												suggestion,
											},
											span: span_to_range(name_ident.1),
										});
									}
								}
							}
							let min_args = std::cmp::min(args.len(), params.len());
							for i in 0..min_args {
								let arg_type =
									self.infer_with_expected(&args[i].0.value, Some(&params[i].1), ctx)?;
								let (_, param_type) = &params[i];
								if !arg_type.assignable_to(param_type, ctx) {
									return Err(TypeError {
										kind: TypeErrorKind::TypeMismatch {
											expected: Box::new(param_type.clone()),
											found: Box::new(arg_type),
										},
										span: span_to_range(args[i].0.value.1),
									});
								}
							}
							Ok(*return_type)
						} else {
							let instantiated_func = self.instantiate_generic_call(
								&func_generics,
								&params,
								&return_type,
								generics,
								args,
								span,
								ctx,
							)?;
							Ok(instantiated_func)
						}
					}
					other => Err(TypeError {
						kind: TypeErrorKind::NotCallable(other.into()),
						span: span_to_range(span),
					}),
				}
			}
			Expr::MemberAccess {
				parent,
				member,
				optional,
			} => {
				let parent_type = self.infer(parent, ctx)?;
				let resolved = self.access_member(&parent_type, member, ctx)?;
				if *optional {
					Ok(resolved.join(&Type::Void))
				} else {
					Ok(resolved)
				}
			}
			Expr::IndexAccess {
				parent,
				index: _,
				optional,
			} => {
				let parent_type = self.infer(parent, ctx)?;
				let result = match parent_type {
					Type::List { item } => Ok(*item),
					Type::Map { value, .. } => Ok(*value),
					Type::Tuple { .. } => Ok(self.fresh_var("_index_result", None)),
					_ => Err(TypeError {
						kind: TypeErrorKind::NotIndexable,
						span: span_to_range(span),
					}),
				};
				if *optional {
					result.map(|t| t.join(&Type::Void))
				} else {
					result
				}
			}
			Expr::Closure {
				params,
				generics,
				return_type,
				body,
			} => {
				let (generic_params, mut closure_ctx) = self.resolve_generic_params(generics, ctx)?;

				let mut param_types = Vec::new();
				for param in params {
					let param_type = match &param.0.type_ {
						Some(t) => self.resolve_ast_type(&t.0, t.1, &closure_ctx)?,
						None => self.fresh_var("_infer", None),
					};
					param_types.push((
						match &param.0.name.0 {
							Pattern::Binding { name, .. } => Some(name.0.clone()),
							_ => None,
						},
						param_type.clone(),
					));
					if let Pattern::Binding { name, .. } = &param.0.name.0 {
						closure_ctx.insert_entry(
							name.0.clone(),
							ContextEntry::Value(ContextValue {
								type_: param_type,
								mutable: param.0.mutable,
								visibility: Visibility::Private,
							}),
						);
					}
				}

				let return_t = match return_type {
					Some(t) => {
						let expected = self.resolve_ast_type(&t.0, t.1, &closure_ctx)?;
						self.check_expr(body, &expected, &closure_ctx)?
					}
					None => self.infer(body, &closure_ctx)?,
				};

				Ok(Type::Function {
					generics: Arc::new(generic_params),
					params: param_types,
					has_spread: params.last().map(|p| p.0.spread).unwrap_or(false),
					return_type: Box::new(return_t),
					constructor: false,
				})
			}
			Expr::PrefixOp { op, value } => {
				let val_type = self.infer(value, ctx)?;
				self.infer_prefix_op(*op, val_type, span)
			}
			Expr::PostfixOp { op, value } => {
				let val_type = self.infer(value, ctx)?;
				self.infer_postfix_op(*op, val_type)
			}
			Expr::BinaryOp { lhs, op, rhs } => {
				let lhs_type = self.infer(lhs, ctx)?;
				let rhs_type = if *op == BinaryOperator::Pipe {
					let expected_rhs = Type::Function {
						generics: Arc::new(Vec::new()),
						params: vec![(None, lhs_type.clone())],
						has_spread: false,
						return_type: Box::new(self.fresh_var("_pipe_return", None)),
						constructor: false,
					};
					self.infer_with_expected(rhs, Some(&expected_rhs), ctx)?
				} else {
					self.infer(rhs, ctx)?
				};
				self.infer_binary_op(lhs_type, *op, rhs_type, span)
			}
			Expr::TypeOp { lhs, op, rhs } => {
				let _lhs_type = self.infer(lhs, ctx)?;
				let rhs_type = self.resolve_ast_type(&rhs.0, rhs.1, ctx)?;
				match op {
					TypeOperator::As => Ok(rhs_type),
				}
			}
			Expr::PatternOp {
				lhs: _,
				op: _,
				rhs: _,
			} => {
				Ok(Type::Boolean) // Pattern matches return boolean
			}
			Expr::AssignOp { lhs, op: _, rhs } => {
				let lhs_type = self.infer(lhs, ctx)?;
				self.infer_with_expected(rhs, Some(&lhs_type), ctx)?;
				Ok(lhs_type)
			}
			Expr::Return { value, label: _ } => {
				if let Some(v) = value {
					self.infer(v, ctx)?;
				}
				Ok(Type::Void)
			}
			Expr::Break { value, label: _ } => {
				if let Some(v) = value {
					self.infer(v, ctx)?;
				}
				Ok(Type::Void)
			}
			Expr::Continue { label: _ } => Ok(Type::Void),
			Expr::While {
				condition,
				body,
				label: _,
			} => {
				self.infer(condition, ctx)?;
				self.infer(body, ctx)?;
				Ok(Type::Void)
			}
			Expr::For {
				variable,
				iterable,
				body,
				label: _,
			} => {
				let iterable_type = self.infer(iterable, ctx)?;
				let item_type = match iterable_type {
					Type::List { item } => *item,
					Type::Map { value, .. } => *value,
					_ => self.fresh_var("_infer", None),
				};

				let identifiers = self.pattern_identifiers(&variable.0, item_type, ctx)?;
				let mut body_ctx = ctx.clone();
				for (name, ty) in identifiers {
					body_ctx.insert_entry(
						name,
						ContextEntry::Value(ContextValue {
							type_: ty,
							mutable: false,
							visibility: Visibility::Private,
						}),
					);
				}

				self.infer(body, &body_ctx)?;
				Ok(Type::Void)
			}
			Expr::If {
				condition,
				then,
				otherwise,
			} => {
				self.infer(condition, ctx)?;
				let then_type = self.infer(then, ctx)?;
				match otherwise {
					Some(else_expr) => {
						let else_type = self.infer(else_expr, ctx)?;
						Ok(then_type.join(&else_type))
					}
					None => Ok(then_type.join(&Type::Void)),
				}
			}
			Expr::Match { value, arms } => {
				let scrutinee_type = self.infer(value, ctx)?;
				let mut result_type = Type::Never;
				for arm in arms {
					let identifiers =
						self.pattern_identifiers(&arm.pattern.0, scrutinee_type.clone(), ctx)?;

					let mut arm_ctx = ctx.clone();
					for (name, ty) in identifiers {
						arm_ctx.insert_entry(
							name,
							ContextEntry::Value(ContextValue {
								type_: ty,
								mutable: false,
								visibility: Visibility::Private,
							}),
						);
					}

					if let Some(guard) = &arm.guard {
						self.infer(guard, &arm_ctx)?;
					}
					let arm_type = self.infer(&arm.body, &arm_ctx)?;
					result_type = result_type.join(&arm_type);
				}
				Ok(result_type)
			}
			Expr::This => ctx
				.local_ctx
				.get(&EcoString::from("this"))
				.map(|entry| match entry {
					ContextEntry::Value(val) => val.type_.clone(),
					ContextEntry::Impl { parent, .. } => parent.type_.clone(),
				})
				.ok_or(TypeError {
					kind: TypeErrorKind::ThisOutsideStruct,
					span: span_to_range(span),
				}),
			Expr::AnonymousParam(index) => Err(TypeError {
				kind: TypeErrorKind::CannotInferAnonymousFunction {
					placeholders: vec![*index],
				},
				span: span_to_range(span),
			}),
			Expr::Placeholder => Ok(self.fresh_var("_placeholder", None)),
			Expr::Block { body, label: _ } => {
				let mut block_ctx = ctx.clone();
				let mut result_type = Type::Void;

				for stmt in body {
					match &stmt.0 {
						Statement::Expr(e) => {
							result_type = self.infer(e, &block_ctx)?;
						}
						Statement::Let {
							meta: LetDeclaration {
								name,
								type_,
								mutable,
							},
							value,
						} => {
							let final_type = match type_ {
								Some(t) => {
									let expected = self.resolve_ast_type(&t.0, t.1, ctx)?;
									self.check_expr(value, &expected, &block_ctx)?
								}
								None => self.infer(value, &block_ctx)?,
							};

							let binding_name = match &name.0 {
								Pattern::Binding { name, .. } => Some(name.0.clone()),
								Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
									Some(path[0].0.clone())
								}
								_ => None,
							};

							if let Some(binding_name) = binding_name {
								block_ctx.insert_entry(
									binding_name,
									ContextEntry::Value(ContextValue {
										type_: final_type,
										mutable: *mutable,
										visibility: Visibility::Private,
									}),
								);
							}

							result_type = Type::Void;
						}
					}
				}

				Ok(result_type)
			}
			Expr::Grouped(inner) => self.infer(inner, ctx),
		}
	}

	fn infer_range_expr(
		&mut self,
		range: &ast::expr::RangeKind,
		span: Span,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		match range {
			ast::expr::RangeKind::From(start) => {
				let item_type = self.infer(start, ctx)?;
				self.instantiate_range_type(
					ctx,
					BUILTIN_RANGE_FROM_CONSTRUCTOR,
					"RangeFrom",
					item_type,
					span,
				)
			}
			ast::expr::RangeKind::To(end) => {
				let item_type = self.infer(end, ctx)?;
				self.instantiate_range_type(
					ctx,
					BUILTIN_RANGE_TO_CONSTRUCTOR,
					"RangeTo",
					item_type,
					span,
				)
			}
			ast::expr::RangeKind::Exclusive { min, max } => {
				let item_type = self.infer_range_bound_pair(min, max, span, ctx)?;
				self.instantiate_range_type(ctx, BUILTIN_RANGE_CONSTRUCTOR, "Range", item_type, span)
			}
			ast::expr::RangeKind::ToInclusive(end) => {
				let item_type = self.infer(end, ctx)?;
				self.instantiate_range_type(
					ctx,
					BUILTIN_RANGE_TO_INCLUSIVE_CONSTRUCTOR,
					"RangeToInclusive",
					item_type,
					span,
				)
			}
			ast::expr::RangeKind::Inclusive { min, max } => {
				let item_type = self.infer_range_bound_pair(min, max, span, ctx)?;
				self.instantiate_range_type(
					ctx,
					BUILTIN_RANGE_INCLUSIVE_CONSTRUCTOR,
					"RangeInclusive",
					item_type,
					span,
				)
			}
		}
	}

	fn infer_range_bound_pair(
		&mut self,
		min: &Spanned<Expr>,
		max: &Spanned<Expr>,
		span: Span,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let min_type = self.infer(min, ctx)?;
		let max_type = self.infer(max, ctx)?;

		if min_type.assignable_to(&max_type, ctx) {
			return Ok(max_type);
		}

		if max_type.assignable_to(&min_type, ctx) {
			return Ok(min_type);
		}

		Err(TypeError {
			kind: TypeErrorKind::TypeMismatch {
				expected: Box::new(min_type),
				found: Box::new(max_type),
			},
			span: span_to_range(span),
		})
	}

	fn instantiate_range_type(
		&self,
		ctx: &Context,
		hidden_name: &str,
		fallback_name: &str,
		item_type: Type,
		span: Span,
	) -> Result<Type, TypeError> {
		let hidden_name = EcoString::from(hidden_name);

		if let Some(raw_type) = ctx.lookup_type(&hidden_name) {
			let base_type = match &raw_type {
				Type::Function { return_type, .. }
					if matches!(return_type.as_ref(), Type::Struct { .. }) =>
				{
					*return_type.clone()
				}
				_ => raw_type,
			};

			if let Type::Struct { generics, .. } = &base_type
				&& let Some(param) = generics.first()
			{
				let mut subst = HashMap::new();
				subst.insert(param.id, item_type.clone());
				return self.substitute_with_args(&base_type, &subst, vec![item_type], span);
			}

			return Ok(base_type);
		}

		Ok(self.synthesize_range_type(fallback_name, item_type))
	}

	fn synthesize_range_type(&self, name: &str, item_type: Type) -> Type {
		let mut fields = BTreeMap::new();
		let mut members = BTreeMap::new();
		let bound_type = self.synthesize_bound_type(item_type.clone());

		match name {
			"Range" | "RangeInclusive" => {
				fields.insert(EcoString::from("start"), item_type.clone());
				fields.insert(EcoString::from("end"), item_type.clone());
			}
			"RangeFrom" => {
				fields.insert(EcoString::from("start"), item_type.clone());
			}
			"RangeTo" | "RangeToInclusive" => {
				fields.insert(EcoString::from("end"), item_type.clone());
			}
			_ => {}
		}

		members.insert(
			EcoString::from("contains"),
			StructMember {
				type_: Box::new(Type::Function {
					generics: Arc::new(Vec::new()),
					params: vec![(Some(EcoString::from("item")), item_type.clone())],
					has_spread: false,
					return_type: Box::new(Type::Boolean),
					constructor: false,
				}),
				kind: StructMemberKind::Immutable,
				required: false,
			},
		);
		members.insert(
			EcoString::from("start_bound"),
			StructMember {
				type_: Box::new(Type::Function {
					generics: Arc::new(Vec::new()),
					params: Vec::new(),
					has_spread: false,
					return_type: Box::new(bound_type.clone()),
					constructor: false,
				}),
				kind: StructMemberKind::Immutable,
				required: false,
			},
		);
		members.insert(
			EcoString::from("end_bound"),
			StructMember {
				type_: Box::new(Type::Function {
					generics: Arc::new(Vec::new()),
					params: Vec::new(),
					has_spread: false,
					return_type: Box::new(bound_type),
					constructor: false,
				}),
				kind: StructMemberKind::Immutable,
				required: false,
			},
		);
		members.insert(
			EcoString::from("into"),
			StructMember {
				type_: Box::new(Type::Function {
					generics: Arc::new(Vec::new()),
					params: Vec::new(),
					has_spread: false,
					return_type: Box::new(Type::String),
					constructor: false,
				}),
				kind: StructMemberKind::Immutable,
				required: false,
			},
		);

		if matches!(name, "Range" | "RangeInclusive") {
			members.insert(
				EcoString::from("is_empty"),
				StructMember {
					type_: Box::new(Type::Function {
						generics: Arc::new(Vec::new()),
						params: Vec::new(),
						has_spread: false,
						return_type: Box::new(Type::Boolean),
						constructor: false,
					}),
					kind: StructMemberKind::Immutable,
					required: false,
				},
			);
		}

		Type::Struct {
			name: EcoString::from(name),
			def_key: None,
			generics: Arc::new(Vec::new()),
			type_args: vec![item_type],
			fields: Arc::new(fields),
			members: Arc::new(members),
			impls: Arc::new(BTreeMap::new()),
		}
	}

	fn synthesize_bound_type(&self, item_type: Type) -> Type {
		Type::Enum {
			name: EcoString::from("Bound"),
			def_key: None,
			generics: Arc::new(Vec::new()),
			type_args: vec![item_type.clone()],
			variants: Arc::new(BTreeMap::from([
				(
					EcoString::from("Included"),
					BTreeMap::from([(EcoString::from("value"), item_type.clone())]),
				),
				(
					EcoString::from("Excluded"),
					BTreeMap::from([(EcoString::from("value"), item_type)]),
				),
				(EcoString::from("Unbounded"), BTreeMap::new()),
			])),
			members: Arc::new(BTreeMap::new()),
			impls: Arc::new(BTreeMap::new()),
		}
	}

	fn range_item_type(&self, ty: &Type) -> Option<Type> {
		let Type::Struct {
			name,
			type_args,
			fields,
			..
		} = ty
		else {
			return None;
		};

		match name.as_str() {
			"Range" | "RangeFrom" | "RangeTo" | "RangeInclusive" | "RangeToInclusive" => type_args
				.first()
				.cloned()
				.or_else(|| fields.values().next().cloned()),
			_ => None,
		}
	}

	/// Check that an expression has a specific type (bidirectional checking)
	fn check_expr(
		&mut self,
		expr: &Spanned<Expr>,
		expected: &Type,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let inferred = self.infer_with_expected(expr, Some(expected), ctx)?;
		if self.type_satisfies(&inferred, expected, ctx) {
			Ok(expected.clone())
		} else {
			Err(TypeError {
				kind: TypeErrorKind::TypeMismatch {
					expected: expected.clone().into(),
					found: inferred.into(),
				},
				span: span_to_range(expr.1),
			})
		}
	}

	fn access_member(
		&self,
		ty: &Type,
		member: &Spanned<EcoString>,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		match ty {
			Type::Struct {
				fields, members, ..
			} => {
				if let Some(field_type) = fields.get(&member.0) {
					Ok(field_type.clone())
				} else if let Some(member_def) = members.get(&member.0) {
					Ok(member_def.type_.as_ref().clone())
				} else {
					self.resolve_interface_member(ty, member, ctx)
				}
			}
			Type::Enum {
				members, variants, ..
			} => {
				if let Some(member_def) = members.get(&member.0) {
					Ok(member_def.type_.as_ref().clone())
				} else if let Some(variant_fields) = variants.get(&member.0) {
					let mut param_types = Vec::new();
					for (field_name, field_type) in variant_fields {
						param_types.push((Some(field_name.clone()), field_type.clone()));
					}
					Ok(Type::Function {
						generics: Arc::new(Vec::new()),
						params: param_types,
						has_spread: false,
						return_type: Box::new(ty.clone()),
						constructor: false,
					})
				} else {
					self.resolve_interface_member(ty, member, ctx)
				}
			}
			Type::EnumVariant { fields, .. } => {
				if let Some(field_type) = fields.get(&member.0) {
					Ok(field_type.clone())
				} else {
					self.resolve_interface_member(ty, member, ctx)
				}
			}
			Type::Interface { members, .. } => {
				if let Some(member_def) = members.get(&member.0) {
					Ok(member_def.type_.as_ref().clone())
				} else {
					self.resolve_interface_member(ty, member, ctx)
				}
			}
			Type::Module { members, .. } => members
				.get(&member.0)
				.map(|entry| match entry {
					ContextEntry::Value(val) => val.type_.clone(),
					ContextEntry::Impl { parent, .. } => parent.type_.clone(),
				})
				.ok_or_else(|| TypeError {
					kind: TypeErrorKind::UnknownMember {
						type_: ty.clone().into(),
						member: member.0.clone(),
						suggestion: find_similar_name(&member.0, members.keys()),
					},
					span: span_to_range(member.1),
				}),
			Type::Variable {
				constraint: Some(constraint),
				..
			} => self.access_member(constraint, member, ctx),
			_ => self.resolve_interface_member(ty, member, ctx),
		}
	}

	fn resolve_interface_member(
		&self,
		receiver: &Type,
		member: &Spanned<EcoString>,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let candidates = self.collect_interface_member_candidates(receiver, &member.0, ctx);
		if candidates.len() == 1 {
			return Ok(candidates[0].member.type_.as_ref().clone());
		}

		if candidates.len() > 1 {
			return Err(TypeError {
				kind: TypeErrorKind::AmbiguousMemberAccess {
					type_: receiver.clone().into(),
					member: member.0.clone(),
					candidates: candidates
						.into_iter()
						.map(|candidate| error::AmbiguousMemberCandidate {
							interface: candidate.interface.into(),
							span: candidate.span,
						})
						.collect(),
				},
				span: span_to_range(member.1),
			});
		}

		let candidate_names = self.collect_available_member_names(receiver, ctx);
		Err(TypeError {
			kind: TypeErrorKind::UnknownMember {
				type_: receiver.clone().into(),
				member: member.0.clone(),
				suggestion: find_similar_name(&member.0, candidate_names.iter()),
			},
			span: span_to_range(member.1),
		})
	}

	fn collect_interface_member_candidates(
		&self,
		receiver: &Type,
		member_name: &EcoString,
		ctx: &Context,
	) -> Vec<MemberCandidate> {
		let mut candidates = Vec::new();
		for extension in &ctx.type_extensions {
			let mut subst = HashMap::new();
			if self.match_type_pattern(&extension.receiver, receiver, &mut subst)
				&& self.impl_constraints_satisfied(&extension.generics, &subst, ctx, &extension.span)
				&& let Some(member) = extension.members.get(member_name)
				&& let Ok(member) = self.substitute_member(
					member,
					&subst,
					Span::new(extension.span.start, extension.span.end),
				) {
				let interface = self
					.substitute(
						&extension.receiver,
						&subst,
						Span::new(extension.span.start, extension.span.end),
					)
					.unwrap_or_else(|_| receiver.clone());
				self.push_member_candidate(
					&mut candidates,
					interface,
					member,
					Some(extension.span.clone()),
				);
			}
		}

		for available in self.collect_available_interfaces(receiver, ctx) {
			if let Type::Interface { members, .. } = &available.interface
				&& let Some(member) = members.get(member_name)
			{
				self.push_member_candidate(
					&mut candidates,
					available.interface.clone(),
					member.clone(),
					available.span.clone(),
				);
			}

			for extension in &ctx.interface_extensions {
				let mut subst = HashMap::new();
				if self.match_type_pattern(&extension.interface, &available.interface, &mut subst)
					&& self.impl_constraints_satisfied(&extension.generics, &subst, ctx, &extension.span)
					&& let Some(member) = extension.members.get(member_name)
					&& let Ok(member) = self.substitute_member(
						member,
						&subst,
						Span::new(extension.span.start, extension.span.end),
					) {
					self.push_member_candidate(
						&mut candidates,
						available.interface.clone(),
						member,
						Some(extension.span.clone()),
					);
				}
			}
		}

		candidates
	}

	fn push_member_candidate(
		&self,
		candidates: &mut Vec<MemberCandidate>,
		interface: Type,
		member: StructMember,
		span: Option<Range<usize>>,
	) {
		if let Some(existing) = candidates.iter_mut().find(|candidate| {
			self.same_type_identity(&candidate.interface, &interface) && candidate.member == member
		}) {
			if existing.span.is_none() {
				existing.span = span;
			}
			return;
		}

		candidates.push(MemberCandidate {
			interface,
			member,
			span,
		});
	}

	fn collect_available_member_names(&self, receiver: &Type, ctx: &Context) -> Vec<EcoString> {
		let mut names = Vec::new();
		match receiver {
			Type::Struct {
				fields, members, ..
			} => {
				names.extend(fields.keys().cloned());
				names.extend(members.keys().cloned());
			}
			Type::EnumVariant { fields, .. } => {
				names.extend(fields.keys().cloned());
			}
			Type::Enum {
				members, variants, ..
			} => {
				names.extend(members.keys().cloned());
				names.extend(variants.keys().cloned());
			}
			Type::Interface { members, .. } => {
				names.extend(members.keys().cloned());
			}
			Type::Module { members, .. } => {
				names.extend(members.keys().cloned());
			}
			_ => {}
		}

		for extension in &ctx.type_extensions {
			let mut subst = HashMap::new();
			if self.match_type_pattern(&extension.receiver, receiver, &mut subst)
				&& self.impl_constraints_satisfied(&extension.generics, &subst, ctx, &extension.span)
			{
				names.extend(extension.members.keys().cloned());
			}
		}

		for available in self.collect_available_interfaces(receiver, ctx) {
			if let Type::Interface { members, .. } = &available.interface {
				names.extend(members.keys().cloned());
			}
			for extension in &ctx.interface_extensions {
				let mut subst = HashMap::new();
				if self.match_type_pattern(&extension.interface, &available.interface, &mut subst)
					&& self.impl_constraints_satisfied(&extension.generics, &subst, ctx, &extension.span)
				{
					names.extend(extension.members.keys().cloned());
				}
			}
		}

		names.sort();
		names.dedup();
		names
	}

	fn type_satisfies(&self, source: &Type, target: &Type, ctx: &Context) -> bool {
		if source.assignable_to(target, ctx) {
			return true;
		}

		match target {
			Type::Interface { .. } => self.satisfies_interface(source, target, ctx),
			Type::Intersection { first, second } => {
				self.type_satisfies(source, first, ctx) && self.type_satisfies(source, second, ctx)
			}
			_ => false,
		}
	}

	fn satisfies_interface(&self, source: &Type, target: &Type, ctx: &Context) -> bool {
		self
			.collect_available_interfaces(source, ctx)
			.into_iter()
			.any(|available| self.interface_matches_goal(&available.interface, target, ctx))
	}

	fn interface_matches_goal(&self, candidate: &Type, target: &Type, ctx: &Context) -> bool {
		let (
			Type::Interface {
				type_args: candidate_args,
				..
			},
			Type::Interface {
				type_args: target_args,
				..
			},
		) = (candidate, target)
		else {
			return false;
		};

		self.same_type_identity(candidate, target)
			&& candidate_args.len() == target_args.len()
			&& candidate_args
				.iter()
				.zip(target_args)
				.all(|(candidate_arg, target_arg)| self.type_satisfies(candidate_arg, target_arg, ctx))
	}

	fn collect_available_interfaces(
		&self,
		receiver: &Type,
		ctx: &Context,
	) -> Vec<AvailableInterface> {
		let mut available = Vec::new();
		if matches!(receiver, Type::Interface { .. }) {
			self.collect_interface_recursive(receiver, receiver, None, &mut available);
		}

		for interface in self.direct_type_impls(receiver) {
			self.collect_interface_recursive(receiver, &interface, None, &mut available);
		}

		for record in &ctx.impl_records {
			let mut subst = HashMap::new();
			if self.match_type_pattern(&record.receiver, receiver, &mut subst)
				&& self.impl_constraints_satisfied(&record.generics, &subst, ctx, &record.span)
				&& let Ok(interface) = self.substitute(
					&record.interface,
					&subst,
					Span::new(record.span.start, record.span.end),
				) {
				self.collect_interface_recursive(
					receiver,
					&interface,
					Some(record.span.clone()),
					&mut available,
				);
			}
		}

		available
	}

	fn collect_interface_recursive(
		&self,
		receiver: &Type,
		interface: &Type,
		span: Option<Range<usize>>,
		available: &mut Vec<AvailableInterface>,
	) {
		let Type::Interface { .. } = interface else {
			return;
		};
		let normalized = self.substitute_self_type(interface, receiver);
		if let Some(existing) = available
			.iter_mut()
			.find(|item| item.interface == normalized)
		{
			if existing.span.is_none() {
				existing.span = span;
			}
			return;
		}

		available.push(AvailableInterface {
			interface: normalized.clone(),
			span: span.clone(),
		});

		if let Type::Interface { impls, .. } = normalized {
			for implied in impls.values() {
				self.collect_interface_recursive(receiver, implied, span.clone(), available);
			}
		}
	}

	fn direct_type_impls(&self, receiver: &Type) -> Vec<Type> {
		match receiver {
			Type::Struct { impls, .. }
			| Type::Enum { impls, .. }
			| Type::EnumVariant { impls, .. }
			| Type::Interface { impls, .. } => impls.values().cloned().collect(),
			_ => Vec::new(),
		}
	}

	fn impl_constraints_satisfied(
		&self,
		generics: &[GenericParamInfo],
		subst: &HashMap<TypeVarId, Type>,
		ctx: &Context,
		span: &Range<usize>,
	) -> bool {
		generics.iter().all(|generic| {
			let Some(constraint) = &generic.constraint else {
				return true;
			};
			let Some(type_) = subst.get(&generic.id) else {
				return false;
			};
			let Ok(constraint) = self.substitute(constraint, subst, Span::new(span.start, span.end))
			else {
				return false;
			};
			self.type_satisfies(type_, &constraint, ctx)
		})
	}

	fn substitute_self_type(&self, ty: &Type, receiver: &Type) -> Type {
		match ty {
			Type::Variable { name, .. } if name == "self" => receiver.clone(),
			Type::List { item } => Type::List {
				item: Box::new(self.substitute_self_type(item, receiver)),
			},
			Type::Tuple { items } => Type::Tuple {
				items: items
					.iter()
					.map(|item| self.substitute_self_type(item, receiver))
					.collect(),
			},
			Type::Map { key, value } => Type::Map {
				key: Box::new(self.substitute_self_type(key, receiver)),
				value: Box::new(self.substitute_self_type(value, receiver)),
			},
			Type::Function {
				generics,
				params,
				has_spread,
				return_type,
				constructor,
			} => Type::Function {
				generics: generics.clone(),
				params: params
					.iter()
					.map(|(name, type_)| (name.clone(), self.substitute_self_type(type_, receiver)))
					.collect(),
				has_spread: *has_spread,
				return_type: Box::new(self.substitute_self_type(return_type, receiver)),
				constructor: *constructor,
			},
			Type::Intersection { first, second } => Type::Intersection {
				first: Box::new(self.substitute_self_type(first, receiver)),
				second: Box::new(self.substitute_self_type(second, receiver)),
			},
			Type::Struct {
				name,
				def_key,
				generics,
				type_args,
				fields,
				members,
				impls,
			} => Type::Struct {
				name: name.clone(),
				def_key: *def_key,
				generics: generics.clone(),
				type_args: type_args
					.iter()
					.map(|arg| self.substitute_self_type(arg, receiver))
					.collect(),
				fields: Arc::new(
					fields
						.iter()
						.map(|(name, type_)| (name.clone(), self.substitute_self_type(type_, receiver)))
						.collect(),
				),
				members: Arc::new(
					members
						.iter()
						.map(|(name, member)| {
							(
								name.clone(),
								StructMember {
									type_: Box::new(self.substitute_self_type(&member.type_, receiver)),
									kind: member.kind,
									required: member.required,
								},
							)
						})
						.collect(),
				),
				impls: Arc::new(
					impls
						.iter()
						.map(|(name, interface)| (name.clone(), self.substitute_self_type(interface, receiver)))
						.collect(),
				),
			},
			Type::Enum {
				name,
				def_key,
				generics,
				type_args,
				variants,
				members,
				impls,
			} => Type::Enum {
				name: name.clone(),
				def_key: *def_key,
				generics: generics.clone(),
				type_args: type_args
					.iter()
					.map(|arg| self.substitute_self_type(arg, receiver))
					.collect(),
				variants: Arc::new(
					variants
						.iter()
						.map(|(variant_name, fields)| {
							(
								variant_name.clone(),
								fields
									.iter()
									.map(|(name, type_)| (name.clone(), self.substitute_self_type(type_, receiver)))
									.collect(),
							)
						})
						.collect(),
				),
				members: Arc::new(
					members
						.iter()
						.map(|(name, member)| {
							(
								name.clone(),
								StructMember {
									type_: Box::new(self.substitute_self_type(&member.type_, receiver)),
									kind: member.kind,
									required: member.required,
								},
							)
						})
						.collect(),
				),
				impls: Arc::new(
					impls
						.iter()
						.map(|(name, interface)| (name.clone(), self.substitute_self_type(interface, receiver)))
						.collect(),
				),
			},
			Type::EnumVariant {
				name,
				variant_name,
				fields,
				variant_of,
				impls,
			} => Type::EnumVariant {
				name: name.clone(),
				variant_name: variant_name.clone(),
				fields: Arc::new(
					fields
						.iter()
						.map(|(name, type_)| (name.clone(), self.substitute_self_type(type_, receiver)))
						.collect(),
				),
				variant_of: Box::new(self.substitute_self_type(variant_of, receiver)),
				impls: Arc::new(
					impls
						.iter()
						.map(|(name, interface)| (name.clone(), self.substitute_self_type(interface, receiver)))
						.collect(),
				),
			},
			Type::Interface {
				name,
				def_key,
				generics,
				type_args,
				members,
				impls,
			} => Type::Interface {
				name: name.clone(),
				def_key: *def_key,
				generics: generics.clone(),
				type_args: type_args
					.iter()
					.map(|arg| self.substitute_self_type(arg, receiver))
					.collect(),
				members: Arc::new(
					members
						.iter()
						.map(|(name, member)| {
							(
								name.clone(),
								StructMember {
									type_: Box::new(self.substitute_self_type(&member.type_, receiver)),
									kind: member.kind,
									required: member.required,
								},
							)
						})
						.collect(),
				),
				impls: Arc::new(
					impls
						.iter()
						.map(|(name, interface)| (name.clone(), self.substitute_self_type(interface, receiver)))
						.collect(),
				),
			},
			_ => ty.clone(),
		}
	}

	fn substitute_member(
		&self,
		member: &StructMember,
		subst: &HashMap<TypeVarId, Type>,
		span: Span,
	) -> Result<StructMember, TypeError> {
		Ok(StructMember {
			type_: Box::new(self.substitute(&member.type_, subst, span)?),
			kind: member.kind,
			required: member.required,
		})
	}

	fn same_type_identity(&self, left: &Type, right: &Type) -> bool {
		match (left, right) {
			(
				Type::Struct {
					name: left_name,
					def_key: left_key,
					..
				},
				Type::Struct {
					name: right_name,
					def_key: right_key,
					..
				},
			)
			| (
				Type::Enum {
					name: left_name,
					def_key: left_key,
					..
				},
				Type::Enum {
					name: right_name,
					def_key: right_key,
					..
				},
			)
			| (
				Type::Interface {
					name: left_name,
					def_key: left_key,
					..
				},
				Type::Interface {
					name: right_name,
					def_key: right_key,
					..
				},
			) => match (left_key, right_key) {
				(Some(left_key), Some(right_key)) => left_key == right_key,
				_ => left_name == right_name,
			},
			(
				Type::Module {
					name: left_name, ..
				},
				Type::Module {
					name: right_name, ..
				},
			) => left_name == right_name,
			_ => left == right,
		}
	}

	fn match_type_pattern(
		&self,
		pattern: &Type,
		actual: &Type,
		subst: &mut HashMap<TypeVarId, Type>,
	) -> bool {
		match pattern {
			Type::Variable { id, .. } => match subst.get(id) {
				Some(existing) => existing == actual,
				None => {
					subst.insert(*id, actual.clone());
					true
				}
			},
			Type::Int
			| Type::UInt
			| Type::Float
			| Type::Char
			| Type::String
			| Type::Boolean
			| Type::Void
			| Type::Never => pattern == actual,
			Type::List { item } => {
				matches!(actual, Type::List { item: actual_item } if self.match_type_pattern(item, actual_item, subst))
			}
			Type::Tuple { items } => {
				matches!(actual, Type::Tuple { items: actual_items } if items.len() == actual_items.len() && items.iter().zip(actual_items).all(|(pattern_item, actual_item)| self.match_type_pattern(pattern_item, actual_item, subst)))
			}
			Type::Map { key, value } => {
				matches!(actual, Type::Map { key: actual_key, value: actual_value } if self.match_type_pattern(key, actual_key, subst) && self.match_type_pattern(value, actual_value, subst))
			}
			Type::Function {
				params,
				return_type,
				..
			} => {
				matches!(actual, Type::Function { params: actual_params, return_type: actual_return_type, .. } if params.len() == actual_params.len() && params.iter().zip(actual_params).all(|((_, pattern_param), (_, actual_param))| self.match_type_pattern(pattern_param, actual_param, subst)) && self.match_type_pattern(return_type, actual_return_type, subst))
			}
			Type::Intersection { first, second } => {
				self.match_type_pattern(first, actual, subst)
					&& self.match_type_pattern(second, actual, subst)
			}
			Type::Struct { type_args, .. }
			| Type::Enum { type_args, .. }
			| Type::Interface { type_args, .. } => {
				let actual_args = match actual {
					Type::Struct { type_args, .. }
					| Type::Enum { type_args, .. }
					| Type::Interface { type_args, .. } => type_args,
					_ => return false,
				};
				self.same_type_identity(pattern, actual)
					&& type_args.len() == actual_args.len()
					&& type_args
						.iter()
						.zip(actual_args)
						.all(|(pattern_arg, actual_arg)| {
							self.match_type_pattern(pattern_arg, actual_arg, subst)
						})
			}
			Type::EnumVariant {
				variant_name,
				variant_of,
				..
			} => {
				matches!(actual, Type::EnumVariant { variant_name: actual_variant_name, variant_of: actual_variant_of, .. } if variant_name == actual_variant_name && self.match_type_pattern(variant_of, actual_variant_of, subst))
			}
			Type::Module { .. } => self.same_type_identity(pattern, actual),
		}
	}

	fn infer_prefix_op(
		&self,
		op: PrefixOperator,
		operand: Type,
		span: Span,
	) -> Result<Type, TypeError> {
		match (op, operand) {
			(PrefixOperator::BoolNot, Type::Boolean) => Ok(Type::Boolean),
			(PrefixOperator::Negate, Type::Int) => Ok(Type::Int),
			(PrefixOperator::Negate, Type::Float) => Ok(Type::Float),
			(PrefixOperator::BitNot, Type::Int) => Ok(Type::Int),
			_ => Err(TypeError {
				kind: TypeErrorKind::InvalidUnaryOp,
				span: span_to_range(span),
			}),
		}
	}

	fn infer_postfix_op(
		&self,
		_op: ast::ops::PostfixOperator,
		operand: Type,
	) -> Result<Type, TypeError> {
		// Error unwrap operator returns the value if not error
		Ok(operand)
	}

	fn infer_binary_op(
		&self,
		lhs: Type,
		op: BinaryOperator,
		rhs: Type,
		span: Span,
	) -> Result<Type, TypeError> {
		match op {
			BinaryOperator::Plus
			| BinaryOperator::Minus
			| BinaryOperator::Times
			| BinaryOperator::Divide
			| BinaryOperator::Remainder
			| BinaryOperator::Power => match (&lhs, &rhs) {
				(Type::Int, Type::Int) => Ok(Type::Int),
				(Type::Float, Type::Float) => Ok(Type::Float),
				(Type::Int, Type::Float) | (Type::Float, Type::Int) => Ok(Type::Float),
				(Type::String, Type::String) if matches!(op, BinaryOperator::Plus) => Ok(Type::String),
				_ => Err(TypeError {
					kind: TypeErrorKind::InvalidBinaryOp,
					span: span_to_range(span),
				}),
			},
			BinaryOperator::Equals
			| BinaryOperator::NotEquals
			| BinaryOperator::LessThan
			| BinaryOperator::LessThanEquals
			| BinaryOperator::GreaterThan
			| BinaryOperator::GreaterThanEquals => Ok(Type::Boolean),
			BinaryOperator::BoolAnd | BinaryOperator::BoolOr => {
				if matches!(lhs, Type::Boolean) && matches!(rhs, Type::Boolean) {
					Ok(Type::Boolean)
				} else {
					Err(TypeError {
						kind: TypeErrorKind::InvalidBinaryOp,
						span: span_to_range(span),
					})
				}
			}
			BinaryOperator::BitAnd | BinaryOperator::BitOr | BinaryOperator::BitXor => {
				if matches!(lhs, Type::Int) && matches!(rhs, Type::Int) {
					Ok(Type::Int)
				} else {
					Err(TypeError {
						kind: TypeErrorKind::InvalidBinaryOp,
						span: span_to_range(span),
					})
				}
			}
			BinaryOperator::LeftShift | BinaryOperator::RightShift => {
				if matches!(lhs, Type::Int) && matches!(rhs, Type::Int) {
					Ok(Type::Int)
				} else {
					Err(TypeError {
						kind: TypeErrorKind::InvalidBinaryOp,
						span: span_to_range(span),
					})
				}
			}
			BinaryOperator::In | BinaryOperator::NotIn => match rhs {
				Type::List { .. } | Type::Map { .. } => Ok(Type::Boolean),
				_ if self.range_item_type(&rhs).is_some() => Ok(Type::Boolean),
				_ => Err(TypeError {
					kind: TypeErrorKind::InvalidBinaryOp,
					span: span_to_range(span),
				}),
			},
			BinaryOperator::Pipe => {
				// Pipe: lhs |> rhs, rhs should be a function that accepts lhs
				match rhs {
					Type::Function {
						params,
						return_type,
						..
					} => {
						if params.len() == 1 {
							if lhs.assignable_to(&params[0].1, &Default::default()) {
								Ok(*return_type)
							} else {
								Err(TypeError {
									kind: TypeErrorKind::InvalidBinaryOp,
									span: span_to_range(span),
								})
							}
						} else {
							Err(TypeError {
								kind: TypeErrorKind::InvalidBinaryOp,
								span: span_to_range(span),
							})
						}
					}
					_ => Err(TypeError {
						kind: TypeErrorKind::InvalidBinaryOp,
						span: span_to_range(span),
					}),
				}
			}
			BinaryOperator::Unwrap => {
				// ?? operator: returns rhs if lhs is a None option or an Error result, otherwise lhs
				Ok(lhs.join(&rhs))
			}
		}
	}

	fn resolve_ast_type(
		&mut self,
		ast_type: &ast::types::Type,
		span: Span,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		match ast_type {
			ast::types::Type::Int => Ok(Type::Int),
			ast::types::Type::UInt => Ok(Type::UInt),
			ast::types::Type::Float => Ok(Type::Float),
			ast::types::Type::Char => Ok(Type::Char),
			ast::types::Type::String => Ok(Type::String),
			ast::types::Type::Boolean => Ok(Type::Boolean),
			ast::types::Type::Void => Ok(Type::Void),
			ast::types::Type::Never => Ok(Type::Never),
			ast::types::Type::Self_ => ctx.self_type.clone().ok_or_else(|| TypeError {
				kind: TypeErrorKind::SelfTypeInGlobalScope,
				span: span_to_range(span),
			}),
			ast::types::Type::Infer => Ok(self.fresh_var("_infer", None)),
			ast::types::Type::Intersection(a, b) => {
				let first = self.resolve_ast_type(&a.0, a.1, ctx)?;
				let second = self.resolve_ast_type(&b.0, b.1, ctx)?;
				Ok(Type::Intersection {
					first: Box::new(first),
					second: Box::new(second),
				})
			}
			ast::types::Type::List(item) => {
				let item_type = self.resolve_ast_type(&item.0, item.1, ctx)?;
				Ok(Type::List {
					item: Box::new(item_type),
				})
			}
			ast::types::Type::Tuple(items) => {
				let resolved: Result<Vec<_>, _> = items
					.iter()
					.map(|t| self.resolve_ast_type(&t.0, t.1, ctx))
					.collect();
				Ok(Type::Tuple { items: resolved? })
			}
			ast::types::Type::Map(key, value) => {
				let key_type = self.resolve_ast_type(&key.0, key.1, ctx)?;
				let value_type = self.resolve_ast_type(&value.0, value.1, ctx)?;
				Ok(Type::Map {
					key: Box::new(key_type),
					value: Box::new(value_type),
				})
			}
			ast::types::Type::Function {
				params,
				return_type,
			} => {
				let param_types: Result<Vec<_>, _> = params
					.iter()
					.map(|(name, ty)| {
						self
							.resolve_ast_type(&ty.0, ty.1, ctx)
							.map(|t| (name.clone().map(|n| n.0.clone()), t))
					})
					.collect();
				let return_t = self.resolve_ast_type(&return_type.0, return_type.1, ctx)?;
				Ok(Type::Function {
					generics: Arc::new(Vec::new()),
					params: param_types?,
					has_spread: false,
					return_type: Box::new(return_t),
					constructor: false,
				})
			}
			ast::types::Type::Reference { name, generics } => {
				// Resolve named types from context, with generic parameter support
				self.resolve_qualified_type(&name.0, generics, span, ctx)
			}
			ast::types::Type::Grouped(inner) => self.resolve_ast_type(&inner.0, inner.1, ctx),
		}
	}

	/// Resolve a qualified type name (e.g., `module::Type` or `Type<A, B>`)
	fn resolve_qualified_type(
		&mut self,
		name: &EcoString,
		generics: &[Spanned<ast::types::GenericArg>],
		span: Span,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		// Look up the base type
		let raw_type = ctx.lookup_type(name).ok_or_else(|| TypeError {
			kind: TypeErrorKind::UnknownType {
				name: name.clone(),
				suggestion: find_similar_name(name, ctx.local_ctx.keys()),
			},
			span: span_to_range(span),
		})?;

		// If the entry is a constructor function whose return type is a struct,
		// resolve to the struct type instead.
		let base_type = match &raw_type {
			Type::Function { return_type, .. } if matches!(return_type.as_ref(), Type::Struct { .. }) => {
				*return_type.clone()
			}
			_ => raw_type,
		};

		// If there are generic arguments, instantiate them
		if !generics.is_empty() {
			self.instantiate_generic(base_type, generics, span, ctx)
		} else {
			Ok(base_type)
		}
	}

	/// Resolve AST generic parameters into GenericParamInfo, also returning the
	/// context extended with the new type variables so callers don't need to
	/// re-create entries with different IDs.
	fn resolve_generic_params(
		&mut self,
		params: &[Spanned<ast::types::GenericParam>],
		ctx: &Context,
	) -> Result<(Vec<GenericParamInfo>, Context), TypeError> {
		let mut result = Vec::with_capacity(params.len());
		let mut resolve_ctx = ctx.clone();

		for param in params {
			let id = self.fresh_type_var_id();
			let gp = GenericParamInfo {
				id,
				name: param.0.name.0.clone(),
				constraint: None,
				default: None,
			};
			result.push(gp.clone());

			resolve_ctx.insert_entry(
				param.0.name.0.clone(),
				ContextEntry::Value(ContextValue {
					type_: Type::Variable {
						id,
						name: param.0.name.0.clone(),
						constraint: gp.constraint.clone().map(Box::new),
					},
					mutable: false,
					visibility: Visibility::Private,
				}),
			);
		}

		for (index, param) in params.iter().enumerate() {
			let constraint = match &param.0.constraint {
				Some(c) => Some(self.resolve_ast_type(&c.0, c.1, &resolve_ctx)?),
				None => None,
			};
			let default = match &param.0.default {
				Some(d) => Some(self.resolve_ast_type(&d.0, d.1, &resolve_ctx)?),
				None => None,
			};

			result[index].constraint = constraint.clone();
			result[index].default = default;
			resolve_ctx.insert_entry(
				param.0.name.0.clone(),
				ContextEntry::Value(ContextValue {
					type_: Type::Variable {
						id: result[index].id,
						name: param.0.name.0.clone(),
						constraint: constraint.map(Box::new),
					},
					mutable: false,
					visibility: Visibility::Private,
				}),
			);
		}

		Ok((result, resolve_ctx))
	}

	/// Instantiate a generic type with concrete type arguments
	fn instantiate_generic(
		&mut self,
		base_type: Type,
		generics: &[Spanned<ast::types::GenericArg>],
		span: Span,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let type_params = match &base_type {
			Type::Struct { generics, .. }
			| Type::Enum { generics, .. }
			| Type::Interface { generics, .. }
			| Type::Function { generics, .. } => (**generics).clone(),
			_ => Vec::new(),
		};

		if type_params.is_empty() && !generics.is_empty() {
			return Err(TypeError {
				kind: TypeErrorKind::GenericArgumentMismatch {
					expected: 0,
					found: generics.len(),
				},
				span: span_to_range(span),
			});
		}

		let mut subst: HashMap<TypeVarId, Type> = HashMap::new();
		let mut provided_by_name: HashMap<EcoString, Type> = HashMap::new();
		let mut positional_args: Vec<Type> = Vec::new();

		for arg in generics {
			let resolved_type = self.resolve_ast_type(&arg.0.value.0, arg.0.value.1, ctx)?;
			if let Some(name_ident) = &arg.0.name {
				provided_by_name.insert(name_ident.0.clone(), resolved_type);
			} else {
				positional_args.push(resolved_type);
			}
		}

		let mut positional_idx = 0;
		for param in &type_params {
			let arg_type = if let Some(ty) = provided_by_name.remove(&param.name) {
				Some(ty)
			} else if positional_idx < positional_args.len() {
				let ty = positional_args[positional_idx].clone();
				positional_idx += 1;
				Some(ty)
			} else if let Some(default) = &param.default {
				Some(self.substitute(default, &subst, span)?)
			} else {
				None
			};

			match arg_type {
				Some(ty) => {
					if let Some(constraint) = &param.constraint {
						let subst_constraint = self.substitute(constraint, &subst, span)?;
						self.check_constraint_at(&ty, &subst_constraint, span, ctx)?;
					}
					subst.insert(param.id, ty);
				}
				None => {
					return Err(TypeError {
						kind: TypeErrorKind::GenericArgumentMismatch {
							expected: type_params.len(),
							found: generics.len(),
						},
						span: span_to_range(span),
					});
				}
			}
		}

		let type_args: Vec<Type> = type_params.iter().map(|p| subst[&p.id].clone()).collect();
		self.substitute_with_args(&base_type, &subst, type_args, span)
	}

	/// Substitute type variables in a type according to the substitution map
	fn substitute(
		&self,
		ty: &Type,
		subst: &HashMap<TypeVarId, Type>,
		span: Span,
	) -> Result<Type, TypeError> {
		self.substitute_with_args(ty, subst, Vec::new(), span)
	}

	fn resolve_nominal_type_args(
		&self,
		generics: &[GenericParamInfo],
		existing_type_args: &[Type],
		type_args: Vec<Type>,
		subst: &HashMap<TypeVarId, Type>,
		span: Span,
	) -> Result<Vec<Type>, TypeError> {
		if !type_args.is_empty() {
			return Ok(type_args);
		}

		if !existing_type_args.is_empty() {
			return existing_type_args
				.iter()
				.map(|arg| self.substitute(arg, subst, span))
				.collect();
		}

		let inferred_type_args: Vec<_> = generics
			.iter()
			.filter_map(|param| subst.get(&param.id).cloned())
			.collect();
		if inferred_type_args.len() == generics.len() {
			Ok(inferred_type_args)
		} else {
			Ok(Vec::new())
		}
	}

	fn substitute_with_args(
		&self,
		ty: &Type,
		subst: &HashMap<TypeVarId, Type>,
		type_args: Vec<Type>,
		span: Span,
	) -> Result<Type, TypeError> {
		match ty {
			Type::Variable { id, name, .. } => {
				if let Some(replacement) = subst.get(id) {
					let is_identity = matches!(replacement, Type::Variable { id: rid, .. } if rid == id);
					if !is_identity && self.occurs_in(id, replacement) {
						return Err(TypeError {
							kind: TypeErrorKind::InfiniteTypeInstantiation {
								var: name.clone(),
								ty: Box::new(replacement.clone()),
							},
							span: span_to_range(span),
						});
					}
					Ok(replacement.clone())
				} else {
					Ok(ty.clone())
				}
			}
			Type::List { item } => Ok(Type::List {
				item: Box::new(self.substitute(item, subst, span)?),
			}),
			Type::Tuple { items } => Ok(Type::Tuple {
				items: items
					.iter()
					.map(|i| self.substitute(i, subst, span))
					.collect::<Result<_, _>>()?,
			}),
			Type::Map { key, value } => Ok(Type::Map {
				key: Box::new(self.substitute(key, subst, span)?),
				value: Box::new(self.substitute(value, subst, span)?),
			}),
			Type::Function {
				generics,
				params,
				has_spread,
				return_type,
				..
			} => {
				let new_params: Result<Vec<_>, _> = params
					.iter()
					.map(|(name, ty)| self.substitute(ty, subst, span).map(|t| (name.clone(), t)))
					.collect();
				Ok(Type::Function {
					generics: generics.clone(),
					params: new_params?,
					has_spread: *has_spread,
					return_type: Box::new(self.substitute(return_type, subst, span)?),
					constructor: false,
				})
			}
			Type::Intersection { first, second } => Ok(Type::Intersection {
				first: Box::new(self.substitute(first, subst, span)?),
				second: Box::new(self.substitute(second, subst, span)?),
			}),
			Type::Struct {
				name,
				def_key,
				generics,
				type_args: existing_type_args,
				fields,
				members,
				impls,
			} => {
				let resolved_type_args = self.resolve_nominal_type_args(
					generics,
					existing_type_args,
					type_args,
					subst,
					span,
				)?;
				let new_fields: Result<BTreeMap<_, _>, _> = fields
					.iter()
					.map(|(k, v)| self.substitute(v, subst, span).map(|t| (k.clone(), t)))
					.collect();
				let new_members: Result<BTreeMap<_, _>, _> = members
					.iter()
					.map(|(k, m)| {
						self.substitute(&m.type_, subst, span).map(|t| {
							(
								k.clone(),
								StructMember {
									type_: Box::new(t),
									kind: m.kind,
									required: m.required,
								},
							)
						})
					})
					.collect();
				let new_impls: Result<BTreeMap<_, _>, _> = impls
					.iter()
					.map(|(k, v)| self.substitute(v, subst, span).map(|t| (k.clone(), t)))
					.collect();
				Ok(Type::Struct {
					name: name.clone(),
					def_key: *def_key,
					generics: generics.clone(),
					type_args: resolved_type_args,
					fields: Arc::new(new_fields?),
					members: Arc::new(new_members?),
					impls: Arc::new(new_impls?),
				})
			}
			Type::Enum {
				name,
				def_key,
				generics,
				type_args: existing_type_args,
				variants,
				members,
				impls,
			} => {
				let resolved_type_args = self.resolve_nominal_type_args(
					generics,
					existing_type_args,
					type_args,
					subst,
					span,
				)?;
				let new_variants: Result<BTreeMap<_, _>, _> = variants
					.iter()
					.map(|(vname, vfields)| {
						let new_vfields: Result<BTreeMap<_, _>, _> = vfields
							.iter()
							.map(|(k, v)| self.substitute(v, subst, span).map(|t| (k.clone(), t)))
							.collect();
						new_vfields.map(|f| (vname.clone(), f))
					})
					.collect();
				let new_members: Result<BTreeMap<_, _>, _> = members
					.iter()
					.map(|(k, m)| {
						self.substitute(&m.type_, subst, span).map(|t| {
							(
								k.clone(),
								StructMember {
									type_: Box::new(t),
									kind: m.kind,
									required: m.required,
								},
							)
						})
					})
					.collect();
				let new_impls: Result<BTreeMap<_, _>, _> = impls
					.iter()
					.map(|(k, v)| self.substitute(v, subst, span).map(|t| (k.clone(), t)))
					.collect();
				Ok(Type::Enum {
					name: name.clone(),
					def_key: *def_key,
					generics: generics.clone(),
					type_args: resolved_type_args,
					variants: Arc::new(new_variants?),
					members: Arc::new(new_members?),
					impls: Arc::new(new_impls?),
				})
			}
			Type::EnumVariant {
				name,
				variant_name,
				fields,
				variant_of,
				impls,
			} => {
				let new_fields: Result<BTreeMap<_, _>, _> = fields
					.iter()
					.map(|(k, v)| self.substitute(v, subst, span).map(|t| (k.clone(), t)))
					.collect();
				Ok(Type::EnumVariant {
					name: name.clone(),
					variant_name: variant_name.clone(),
					fields: Arc::new(new_fields?),
					variant_of: Box::new(self.substitute(variant_of, subst, span)?),
					impls: Arc::new(
						impls
							.iter()
							.map(|(k, v)| self.substitute(v, subst, span).map(|t| (k.clone(), t)))
							.collect::<Result<_, _>>()?,
					),
				})
			}
			Type::Interface {
				name,
				def_key,
				generics,
				type_args: existing_type_args,
				members,
				impls,
			} => {
				let resolved_type_args = self.resolve_nominal_type_args(
					generics,
					existing_type_args,
					type_args,
					subst,
					span,
				)?;
				let new_members: Result<BTreeMap<_, _>, _> = members
					.iter()
					.map(|(k, m)| {
						self.substitute(&m.type_, subst, span).map(|t| {
							(
								k.clone(),
								StructMember {
									type_: Box::new(t),
									kind: m.kind,
									required: m.required,
								},
							)
						})
					})
					.collect();
				let new_impls: Result<BTreeMap<_, _>, _> = impls
					.iter()
					.map(|(k, v)| self.substitute(v, subst, span).map(|t| (k.clone(), t)))
					.collect();
				Ok(Type::Interface {
					name: name.clone(),
					def_key: *def_key,
					generics: generics.clone(),
					type_args: resolved_type_args,
					members: Arc::new(new_members?),
					impls: Arc::new(new_impls?),
				})
			}
			Type::Int
			| Type::UInt
			| Type::Float
			| Type::Char
			| Type::String
			| Type::Boolean
			| Type::Void
			| Type::Never
			| Type::Module { .. } => Ok(ty.clone()),
		}
	}

	fn occurs_in(&self, var: &TypeVarId, ty: &Type) -> bool {
		match ty {
			Type::Variable { id, .. } => id == var,
			Type::List { item } => self.occurs_in(var, item),
			Type::Tuple { items } => items.iter().any(|i| self.occurs_in(var, i)),
			Type::Map { key, value } => self.occurs_in(var, key) || self.occurs_in(var, value),
			Type::Function {
				params,
				return_type,
				..
			} => params.iter().any(|(_, t)| self.occurs_in(var, t)) || self.occurs_in(var, return_type),
			Type::Intersection { first, second } => {
				self.occurs_in(var, first) || self.occurs_in(var, second)
			}
			Type::Struct { fields, .. } => fields.values().any(|f| self.occurs_in(var, f)),
			Type::Enum { variants, .. } => variants
				.values()
				.any(|vf| vf.values().any(|f| self.occurs_in(var, f))),
			Type::EnumVariant {
				fields, variant_of, ..
			} => fields.values().any(|f| self.occurs_in(var, f)) || self.occurs_in(var, variant_of),
			Type::Interface { members, .. } => members.values().any(|m| self.occurs_in(var, &m.type_)),
			_ => false,
		}
	}

	fn check_constraint_at(
		&self,
		ty: &Type,
		constraint: &Type,
		span: Span,
		ctx: &Context,
	) -> Result<(), TypeError> {
		if self.type_satisfies(ty, constraint, ctx) {
			Ok(())
		} else if matches!(constraint, Type::Interface { .. }) {
			Err(TypeError {
				kind: TypeErrorKind::ImplNotFound {
					type_: ty.clone().into(),
					interface: constraint.clone().into(),
				},
				span: span_to_range(span),
			})
		} else if let Type::Intersection { first, second } = constraint {
			if !self.type_satisfies(ty, first, ctx) {
				self.check_constraint_at(ty, first, span, ctx)
			} else {
				self.check_constraint_at(ty, second, span, ctx)
			}
		} else {
			Err(TypeError {
				kind: TypeErrorKind::ConstraintViolation {
					type_: ty.clone().into(),
					constraint: constraint.clone().into(),
				},
				span: span_to_range(span),
			})
		}
	}

	/// Instantiate a generic function call with explicit type arguments and/or inference from arguments
	#[allow(clippy::too_many_arguments)]
	fn instantiate_generic_call(
		&mut self,
		func_generics: &[GenericParamInfo],
		func_params: &[(Option<EcoString>, Type)],
		func_return_type: &Type,
		explicit_generics: &[Spanned<ast::types::GenericArg>],
		args: &[Spanned<ast::expr::CallArg>],
		span: Span,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let mut subst: HashMap<TypeVarId, Type> = HashMap::new();

		let mut provided_by_name: HashMap<EcoString, Type> = HashMap::new();
		let mut positional_args: Vec<Type> = Vec::new();

		for arg in explicit_generics {
			let resolved_type = self.resolve_ast_type(&arg.0.value.0, arg.0.value.1, ctx)?;
			if let Some(name_ident) = &arg.0.name {
				provided_by_name.insert(name_ident.0.clone(), resolved_type);
			} else {
				positional_args.push(resolved_type);
			}
		}

		let mut positional_idx = 0;
		for param in func_generics {
			if let Some(ty) = provided_by_name.remove(&param.name) {
				subst.insert(param.id, ty);
			} else if positional_idx < positional_args.len() {
				subst.insert(param.id, positional_args[positional_idx].clone());
				positional_idx += 1;
			}
		}

		for arg in args {
			if let Some(name_ident) = &arg.0.name {
				let name = &name_ident.0;
				if !func_params
					.iter()
					.any(|(p_name, _)| p_name.as_ref().is_some_and(|n| n == name))
				{
					let suggestion =
						find_similar_name(name, func_params.iter().filter_map(|(n, _)| n.as_ref()));
					return Err(TypeError {
						kind: TypeErrorKind::UnknownNamedArgument {
							name: name.clone(),
							suggestion,
						},
						span: span_to_range(name_ident.1),
					});
				}
			}
		}

		let min_args = std::cmp::min(args.len(), func_params.len());
		for i in 0..min_args {
			let arg_type = self.infer_with_expected(&args[i].0.value, Some(&func_params[i].1), ctx)?;
			let (_, param_type) = &func_params[i];

			self.unify_for_inference(param_type, &arg_type, &mut subst);
		}

		for param in func_generics {
			if !subst.contains_key(&param.id) {
				if let Some(default) = &param.default {
					let default_resolved = self.substitute(default, &subst, span)?;
					subst.insert(param.id, default_resolved);
				} else {
					return Err(TypeError {
						kind: TypeErrorKind::GenericArgumentMismatch {
							expected: func_generics.len(),
							found: explicit_generics.len(),
						},
						span: span_to_range(span),
					});
				}
			}
		}

		for param in func_generics {
			if let Some(constraint) = &param.constraint {
				let subst_constraint = self.substitute(constraint, &subst, span)?;
				if let Some(arg_ty) = subst.get(&param.id) {
					self.check_constraint_at(arg_ty, &subst_constraint, span, ctx)?;
				}
			}
		}

		self.substitute(func_return_type, &subst, span)
	}

	fn unify_for_inference(
		&self,
		param_type: &Type,
		arg_type: &Type,
		subst: &mut HashMap<TypeVarId, Type>,
	) {
		match param_type {
			_ if matches!(arg_type, Type::Variable { constraint: Some(_), .. }) => {
				if let Type::Variable {
					constraint: Some(constraint),
					..
				} = arg_type
				{
					self.unify_for_inference(param_type, constraint, subst);
				}
			}
			Type::Variable { id, name, .. } if !subst.contains_key(id) && !name.starts_with('_') => {
				subst.insert(*id, arg_type.clone());
			}
			Type::List { item } => {
				if let Type::List { item: arg_item } = arg_type {
					self.unify_for_inference(item, arg_item, subst);
				}
			}
			Type::Tuple { items } => {
				if let Type::Tuple { items: arg_items } = arg_type {
					for (p, a) in items.iter().zip(arg_items.iter()) {
						self.unify_for_inference(p, a, subst);
					}
				}
			}
			Type::Map { key, value } => {
				if let Type::Map {
					key: arg_key,
					value: arg_value,
				} = arg_type
				{
					self.unify_for_inference(key, arg_key, subst);
					self.unify_for_inference(value, arg_value, subst);
				}
			}
			Type::Function {
				params,
				return_type,
				..
			} => {
				if let Type::Function {
					params: arg_params,
					return_type: arg_return,
					..
				} = arg_type
				{
					for ((_, p), (_, a)) in params.iter().zip(arg_params.iter()) {
						self.unify_for_inference(p, a, subst);
					}
					self.unify_for_inference(return_type, arg_return, subst);
				}
			}
			Type::Struct {
				type_args: param_args,
				..
			}
			| Type::Enum {
				type_args: param_args,
				..
			}
			| Type::Interface {
				type_args: param_args,
				..
			} => match arg_type {
				Type::Struct {
					type_args: arg_args, ..
				}
				| Type::Enum {
					type_args: arg_args, ..
				}
				| Type::Interface {
					type_args: arg_args, ..
				} => {
					for (param_arg, arg_arg) in param_args.iter().zip(arg_args.iter()) {
						self.unify_for_inference(param_arg, arg_arg, subst);
					}
				}
				_ => {}
			},
			_ => {}
		}
	}

	fn pattern_identifiers(
		&mut self,
		pattern: &Pattern,
		scrutinee: Type,
		_ctx: &Context,
	) -> Result<HashMap<EcoString, Type>, TypeError> {
		let mut identifiers = HashMap::new();
		self.collect_pattern_identifiers(pattern, scrutinee, &mut identifiers)?;
		Ok(identifiers)
	}

	fn collect_pattern_identifiers(
		&mut self,
		pattern: &Pattern,
		scrutinee: Type,
		identifiers: &mut HashMap<EcoString, Type>,
	) -> Result<(), TypeError> {
		match pattern {
			Pattern::Int(_)
			| Pattern::UInt(_)
			| Pattern::Float(_)
			| Pattern::Char(_)
			| Pattern::String(_)
			| Pattern::Boolean(_)
			| Pattern::Placeholder => Ok(()),
			Pattern::Binding { name, inner } => {
				self.collect_pattern_identifiers(&inner.0, scrutinee.clone(), identifiers)?;
				if identifiers.insert(name.0.clone(), scrutinee).is_some() {
					return Err(TypeError {
						kind: TypeErrorKind::DuplicatePatternIdentifier {
							pattern: pattern.clone(),
							identifier: name.0.clone(),
						},
						span: 0..0,
					});
				}
				Ok(())
			}
			Pattern::List(items) => {
				let item_type = match scrutinee {
					Type::List { item } => *item,
					_ => {
						return Err(TypeError {
							kind: TypeErrorKind::PatternTypeMismatch {
								pattern: pattern.clone(),
								scrutinee: scrutinee.into(),
							},
							span: 0..0,
						});
					}
				};

				for item in items {
					match &item.0 {
						ast::expr::ListPatternEntry::Item(p) => {
							self.collect_pattern_identifiers(&p.0, item_type.clone(), identifiers)?;
						}
						ast::expr::ListPatternEntry::Rest(opt_name) => {
							if let Some(name) = opt_name
								&& identifiers
									.insert(
										name.0.clone(),
										Type::List {
											item: Box::new(item_type.clone()),
										},
									)
									.is_some()
							{
								return Err(TypeError {
									kind: TypeErrorKind::DuplicatePatternIdentifier {
										pattern: pattern.clone(),
										identifier: name.0.clone(),
									},
									span: 0..0,
								});
							}
						}
					}
				}
				Ok(())
			}
			Pattern::Tuple(items) => {
				let tuple_items = match scrutinee {
					Type::Tuple { items: t } => t,
					_ => {
						return Err(TypeError {
							kind: TypeErrorKind::PatternTypeMismatch {
								pattern: pattern.clone(),
								scrutinee: scrutinee.into(),
							},
							span: 0..0,
						});
					}
				};

				if items.len() > tuple_items.len() {
					return Err(TypeError {
						kind: TypeErrorKind::TuplePatternTooLong {
							pattern: pattern.clone(),
							tuple_items,
						},
						span: 0..0,
					});
				}

				for (item, ty) in items.iter().zip(tuple_items.iter()) {
					match &item.0 {
						ast::expr::ListPatternEntry::Item(p) => {
							self.collect_pattern_identifiers(&p.0, ty.clone(), identifiers)?;
						}
						ast::expr::ListPatternEntry::Rest(_) => {
							return Err(TypeError {
								kind: TypeErrorKind::RestPatternNotAtEnd {
									pattern: pattern.clone(),
								},
								span: 0..0,
							});
						}
					}
				}
				Ok(())
			}
			Pattern::Map(entries) => {
				match scrutinee {
					Type::Map { .. } => {}
					_ => {
						return Err(TypeError {
							kind: TypeErrorKind::PatternTypeMismatch {
								pattern: pattern.clone(),
								scrutinee: scrutinee.into(),
							},
							span: 0..0,
						});
					}
				}

				for entry in entries {
					match &entry.0 {
						ast::expr::MapPatternEntry::Entry(key, value) => {
							if !key.0.is_constant() {
								return Err(TypeError {
									kind: TypeErrorKind::NonConstantMapPatternKey {
										key_pattern: key.0.clone(),
										pattern: pattern.clone(),
									},
									span: 0..0,
								});
							}
							let value_type = self.fresh_var("_map_value", None);
							self.collect_pattern_identifiers(&value.0, value_type, identifiers)?;
						}
						ast::expr::MapPatternEntry::Rest(_) => {}
					}
				}
				Ok(())
			}
			Pattern::Range(_) => Ok(()),
			Pattern::Struct { path, fields } => {
				// Handle struct/enum variant pattern matching
				// For enum variants like `Some(value)` or `Ok(field_name = value)`
				let variant_fields = match &scrutinee {
					Type::Enum { variants, .. } => {
						// Get the variant name from the path
						if let Some(variant_name) = path.first() {
							variants.get(&variant_name.0).cloned()
						} else {
							None
						}
					}
					Type::Struct {
						fields: struct_fields,
						..
					} => Some((**struct_fields).clone()),
					_ => None,
				};

				if let Some(variant_fields) = variant_fields {
					for field in fields {
						match &field.0 {
							ast::expr::StructPatternField::Value { name, value } => {
								// Get the type for this field from the variant
								let field_type = variant_fields
									.get(&name.0)
									.cloned()
									.unwrap_or_else(|| self.fresh_var("_unknown_field", None));
								// Recursively extract identifiers from the value pattern
								self.collect_pattern_identifiers(&value.0, field_type, identifiers)?;
							}
							ast::expr::StructPatternField::Named(ident) => {
								// Shorthand: `field_name` binds field_name to the field's type
								let field_type = variant_fields
									.get(&ident.0)
									.cloned()
									.unwrap_or_else(|| self.fresh_var("_unknown_field", None));
								if identifiers.insert(ident.0.clone(), field_type).is_some() {
									return Err(TypeError {
										kind: TypeErrorKind::DuplicatePatternIdentifier {
											pattern: pattern.clone(),
											identifier: ident.0.clone(),
										},
										span: 0..0,
									});
								}
							}
							ast::expr::StructPatternField::Rest => {
								// `...` - no bindings
							}
						}
					}
				} else if path.len() == 1 && fields.is_empty() {
					// A single identifier with no fields that doesn't match any variant/struct
					// is treated as a variable binding (e.g., `a` in `Some(value = a)`)
					let name = &path[0];
					if identifiers.insert(name.0.clone(), scrutinee).is_some() {
						return Err(TypeError {
							kind: TypeErrorKind::DuplicatePatternIdentifier {
								pattern: pattern.clone(),
								identifier: name.0.clone(),
							},
							span: 0..0,
						});
					}
				}
				Ok(())
			}
			Pattern::Union(first, second) => {
				let first_idents = {
					let mut idents = HashMap::new();
					self.collect_pattern_identifiers(&first.0, scrutinee.clone(), &mut idents)?;
					idents
				};
				self.collect_pattern_identifiers(&second.0, scrutinee, identifiers)?;

				for (name, ty) in first_idents {
					if let Some(other_ty) = identifiers.get(&name) {
						if ty != *other_ty {
							return Err(TypeError {
								kind: TypeErrorKind::ConflictingUnionPatternIdentifiers {
									identifier: name,
									first_type: ty.into(),
									second_type: other_ty.clone().into(),
								},
								span: 0..0,
							});
						}
					} else {
						identifiers.insert(name, ty);
					}
				}
				Ok(())
			}
			Pattern::Grouped(inner) => self.collect_pattern_identifiers(&inner.0, scrutinee, identifiers),
		}
	}

	pub fn check_module(&mut self, module: &Module, ctx: &Context) -> Result<Context, TypeError> {
		let mut current_ctx = if self.should_inject_implicit_prelude(module) {
			self.inject_implicit_prelude_entries(ctx)
		} else {
			ctx.clone()
		};
		current_ctx = self.inject_builtin_range_entries(&current_ctx);

		for decl in &module.members {
			current_ctx = self.check_declaration(decl, &current_ctx)?;
		}

		Ok(current_ctx)
	}

	fn predeclare_module(&mut self, module: &Module) -> Context {
		let mut current_ctx = Context::default();

		for decl in &module.members {
			current_ctx = self.predeclare_declaration(decl, &current_ctx);
		}

		current_ctx
	}

	fn predeclare_declaration(&mut self, declaration: &Declaration, ctx: &Context) -> Context {
		match declaration {
			Declaration::Struct {
				visibility: _,
				name,
				generics,
				fields,
				members: _,
			} => {
				if fields.is_empty() {
					return ctx.clone();
				}

				let Ok((generic_params, mut struct_ctx)) = self.resolve_generic_params(generics, ctx)
				else {
					return ctx.clone();
				};

				let forward_struct_type = Type::Struct {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					fields: Arc::new(BTreeMap::new()),
					members: Arc::new(BTreeMap::new()),
					impls: Arc::new(BTreeMap::new()),
					def_key: None,
				};
				struct_ctx = struct_ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: forward_struct_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				);

				let mut field_map = BTreeMap::new();
				for field in fields {
					let Ok(field_type) =
						self.resolve_ast_type(&field.0.type_.0, field.0.type_.1, &struct_ctx)
					else {
						return ctx.clone();
					};

					field_map.insert(field.0.name.0.clone(), field_type.clone());
					struct_ctx.insert_entry(
						field.0.name.0.clone(),
						ContextEntry::Value(ContextValue {
							type_: field_type,
							mutable: false,
							visibility: Visibility::Private,
						}),
					);
				}

				let struct_type = Type::Struct {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					fields: Arc::new(field_map.clone()),
					members: Arc::new(BTreeMap::new()),
					impls: Arc::new(BTreeMap::new()),
					def_key: None,
				};

				let constructor_params = fields
					.iter()
					.filter_map(|field| {
						field_map
							.get(&field.0.name.0)
							.cloned()
							.map(|field_type| (Some(field.0.name.0.clone()), field_type))
					})
					.collect();

				ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: Type::Function {
							generics: Arc::new(generic_params),
							params: constructor_params,
							has_spread: false,
							return_type: Box::new(struct_type),
							constructor: true,
						},
						mutable: false,
						visibility: Visibility::Public,
					}),
				)
			}
			Declaration::Enum {
				visibility: _,
				name,
				generics,
				variants,
				members: _,
			} => {
				let Ok((generic_params, mut enum_ctx)) = self.resolve_generic_params(generics, ctx) else {
					return ctx.clone();
				};

				let forward_enum_type = Type::Enum {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					variants: Arc::new(BTreeMap::new()),
					members: Arc::new(BTreeMap::new()),
					impls: Arc::new(BTreeMap::new()),
					def_key: None,
				};
				enum_ctx = enum_ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: forward_enum_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				);

				let mut variants_map = BTreeMap::new();
				for variant in variants {
					let mut variant_fields = BTreeMap::new();
					for field in &variant.0.fields {
						let Ok(field_type) =
							self.resolve_ast_type(&field.0.type_.0, field.0.type_.1, &enum_ctx)
						else {
							return ctx.clone();
						};

						variant_fields.insert(field.0.name.0.clone(), field_type);
					}
					variants_map.insert(variant.0.name.0.clone(), variant_fields);
				}

				ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: Type::Enum {
							name: name.0.clone(),
							generics: Arc::new(generic_params),
							type_args: Vec::new(),
							variants: Arc::new(variants_map),
							members: Arc::new(BTreeMap::new()),
							impls: Arc::new(BTreeMap::new()),
							def_key: None,
						},
						mutable: false,
						visibility: Visibility::Public,
					}),
				)
			}
			Declaration::Interface {
				visibility: _,
				name,
				generics,
				super_interfaces: _,
				members: _,
			} => {
				let Ok((generic_params, _)) = self.resolve_generic_params(generics, ctx) else {
					return ctx.clone();
				};

				ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: Type::Interface {
							name: name.0.clone(),
							generics: Arc::new(generic_params),
							type_args: Vec::new(),
							members: Arc::new(BTreeMap::new()),
							impls: Arc::new(BTreeMap::new()),
							def_key: None,
						},
						mutable: false,
						visibility: Visibility::Public,
					}),
				)
			}
			Declaration::TypeAlias {
				visibility: _,
				meta: TypeAliasDeclaration { name, generics },
				value,
			} => {
				let Ok((_generic_params, alias_ctx)) = self.resolve_generic_params(generics, ctx) else {
					return ctx.clone();
				};

				let Ok(aliased_type) = self.resolve_ast_type(&value.0, value.1, &alias_ctx) else {
					return ctx.clone();
				};

				ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: aliased_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				)
			}
			Declaration::Namespace {
				visibility: _,
				name,
				members: _,
			} => ctx.with_new_entry(
				name.0.clone(),
				ContextEntry::Value(ContextValue {
					type_: Type::Module {
						name: name.0.clone(),
						members: Arc::new(BTreeMap::new()),
					},
					mutable: false,
					visibility: Visibility::Public,
				}),
			),
			_ => ctx.clone(),
		}
	}

	fn accumulate_type_error(&self, db: &dyn Db, file: SourceFile, err: TypeError) {
		let err_span = err.span.clone();
		let err_file = err
			.file_path()
			.unwrap_or_else(|| EcoString::from(file.path(db).as_str()));

		Diagnostics(Diagnostic {
			file_path: err_file,
			span: Span::new(err_span.start, err_span.end),
			message: err.to_string(),
			kind: DiagnosticKind::TypeError,
		})
		.accumulate(db);
		TypeErrors(err).accumulate(db);
	}

	pub fn check_file_salsa(
		&mut self,
		db: &dyn Db,
		file: SourceFile,
		config: ProjectConfig,
	) -> Context {
		let source_path = PathBuf::from(file.path(db).as_str());
		let cache_key = source_path
			.canonicalize()
			.unwrap_or_else(|_| source_path.clone());

		if let Some(status) = self.module_cache.get(&cache_key) {
			return match status {
				ModuleStatus::InProgress(ctx) | ModuleStatus::Complete(ctx) => ctx.clone(),
			};
		}

		let parse_result = queries::parse_file(db, file);
		let Some(module) = parse_result.module else {
			self
				.module_cache
				.insert(cache_key, ModuleStatus::Complete(Context::default()));
			return Context::default();
		};

		let predeclared_ctx = self.predeclare_module(&module.0);
		self.module_cache.insert(
			cache_key.clone(),
			ModuleStatus::InProgress(predeclared_ctx.clone()),
		);

		let prev_file = self.current_file.replace(source_path);
		let mut current_ctx = if self.should_inject_implicit_prelude(&module.0) {
			self.inject_implicit_prelude_entries_salsa(db, config, &predeclared_ctx)
		} else {
			predeclared_ctx.clone()
		};
		current_ctx = self.inject_builtin_range_entries_salsa(db, config, &current_ctx);

		for decl in &module.0.members {
			let result = match decl {
				Declaration::Import { root, path, idents } => self.check_import_salsa(
					db,
					file,
					config,
					root,
					path,
					idents.as_ref().map(Vec::as_slice),
					&current_ctx,
				),
				_ => self.check_declaration(decl, &current_ctx),
			};

			match result {
				Ok(ctx) => current_ctx = ctx,
				Err(err) => self.accumulate_type_error(db, file, err),
			}
		}

		self.current_file = prev_file;
		current_ctx.next_type_var_id = self.next_type_var_id;
		self
			.module_cache
			.insert(cache_key, ModuleStatus::Complete(current_ctx.clone()));

		current_ctx
	}

	/// Check an interface declaration and extract its members
	fn process_interface(
		&mut self,
		name: &EcoString,
		super_interfaces: &[Spanned<(Ident, Vec<Spanned<ast::types::GenericArg>>)>],
		members: &[Spanned<InterfaceMember>],
		ctx: &Context,
	) -> Result<(BTreeMap<EcoString, StructMember>, BTreeMap<EcoString, Type>), TypeError> {
		let mut interface_members = BTreeMap::new();
		let mut implied_interfaces = BTreeMap::new();

		let this_type = ctx
			.self_type
			.clone()
			.unwrap_or_else(|| self.fresh_var("self", None));
		let member_ctx = ctx.with_new_entry(
			EcoString::from("this"),
			ContextEntry::Value(ContextValue {
				type_: this_type.clone(),
				mutable: false,
				visibility: Visibility::Private,
			}),
		);

		for super_interface in super_interfaces {
			let Spanned((interface_name, interface_generics), interface_span) = super_interface;
			let interface_ty =
				self.resolve_qualified_type(&interface_name.0, interface_generics, *interface_span, ctx)?;
			implied_interfaces.insert(interface_name.0.clone(), interface_ty);
		}

		// Pass 1: collect signatures and implied interfaces so default bodies can see the full contract.
		for member_spanned in members {
			match &member_spanned.0 {
				InterfaceMember::Element(elem_spanned) => {
					if let Some((name, member)) =
						self.check_interface_element(&elem_spanned.0, &member_ctx)?
					{
						interface_members.insert(name, member);
					}
				}
				InterfaceMember::Namespace(impl_members) => {
					// Namespace members in interface (static members — no `this`)
					for impl_member in impl_members {
						if let Some((name, struct_member)) =
							self.collect_impl_member_signature(
								impl_member,
								ctx,
								StructMemberKind::Namespace,
							)?
						{
							interface_members.insert(name, struct_member);
						}
					}
				}
				InterfaceMember::ImplMut(elements) => {
					// Mutable interface elements
					let mutable_ctx = ctx.with_new_entry(
						EcoString::from("this"),
						ContextEntry::Value(ContextValue {
							type_: ctx
								.self_type
								.clone()
								.unwrap_or_else(|| self.fresh_var("self", None)),
							mutable: true,
							visibility: Visibility::Private,
						}),
					);
					for elem in elements {
						if let Some((name, member)) = self.check_interface_element(&elem.0, &mutable_ctx)? {
							interface_members.insert(
								name,
								StructMember {
									type_: member.type_,
									kind: StructMemberKind::Mutable,
									required: member.required,
								},
							);
						}
					}
				}
				InterfaceMember::Impl {
					interface: (interface_ident, interface_generics),
					generics,
					members: impl_members,
				} => {
					let (_generic_params, impl_ctx) = self.resolve_generic_params(generics, &member_ctx)?;
					let interface_ty = self.resolve_qualified_type(
						&interface_ident.0,
						interface_generics,
						interface_ident.1,
						&impl_ctx,
					)?;
					let provided_members = self.collect_impl_member_signatures(
						impl_members,
						&impl_ctx,
						StructMemberKind::Immutable,
					)?;
					for (member_name, member) in provided_members {
						interface_members.insert(member_name, member);
					}
					if let Type::Interface { name, .. } = &interface_ty {
						implied_interfaces.insert(name.clone(), interface_ty);
					}
				}
			}
		}

		let full_interface_type = match this_type.clone() {
			Type::Variable { id, name: self_name, .. } => Type::Variable {
				id,
				name: self_name,
				constraint: Some(Box::new(Type::Interface {
					name: name.clone(),
					generics: match &ctx.self_type {
						Some(Type::Variable {
							constraint: Some(constraint),
							..
						}) => match constraint.as_ref() {
							Type::Interface { generics, .. } => generics.clone(),
							_ => Arc::new(Vec::new()),
						},
						_ => Arc::new(Vec::new()),
					},
					type_args: match &ctx.self_type {
						Some(Type::Variable {
							constraint: Some(constraint),
							..
						}) => match constraint.as_ref() {
							Type::Interface { type_args, .. } => type_args.clone(),
							_ => Vec::new(),
						},
						_ => Vec::new(),
					},
					members: Arc::new(interface_members.clone()),
					impls: Arc::new(implied_interfaces.clone()),
					def_key: None,
				})),
			},
			other => other,
		};
		let body_ctx = ctx
			.with_self_type(full_interface_type.clone())
			.with_new_entry(
				EcoString::from("this"),
				ContextEntry::Value(ContextValue {
					type_: full_interface_type.clone(),
					mutable: false,
					visibility: Visibility::Private,
				}),
			);
		let mutable_body_ctx = ctx
			.with_self_type(full_interface_type.clone())
			.with_new_entry(
				EcoString::from("this"),
				ContextEntry::Value(ContextValue {
					type_: full_interface_type.clone(),
					mutable: true,
					visibility: Visibility::Private,
				}),
			);
		let _namespace_ctx = ctx.with_self_type(full_interface_type.clone());

		for member_spanned in members {
			match &member_spanned.0 {
				InterfaceMember::Element(elem_spanned) => {
					if let InterfaceElement::Func { meta, body } = &elem_spanned.0
						&& meta.return_type.is_none()
						&& body.is_some()
					{
						interface_members.insert(
							meta.name.0.clone(),
							self.collect_interface_func_signature(
								meta,
								body.as_ref(),
								&body_ctx,
								true,
							)?,
						);
					}
				}
				InterfaceMember::ImplMut(elements) => {
					for elem in elements {
						if let InterfaceElement::Func { meta, body } = &elem.0
							&& meta.return_type.is_none()
							&& body.is_some()
						{
							let mut member = self.collect_interface_func_signature(
								meta,
								body.as_ref(),
								&mutable_body_ctx,
								true,
							)?;
							member.kind = StructMemberKind::Mutable;
							interface_members.insert(meta.name.0.clone(), member);
						}
					}
				}
				InterfaceMember::Namespace(_) | InterfaceMember::Impl { .. } => {}
			}
		}

		let full_interface_type = match this_type.clone() {
			Type::Variable { id, name: self_name, .. } => Type::Variable {
				id,
				name: self_name,
				constraint: Some(Box::new(Type::Interface {
					name: name.clone(),
					generics: match &ctx.self_type {
						Some(Type::Variable {
							constraint: Some(constraint),
							..
						}) => match constraint.as_ref() {
							Type::Interface { generics, .. } => generics.clone(),
							_ => Arc::new(Vec::new()),
						},
						_ => Arc::new(Vec::new()),
					},
					type_args: match &ctx.self_type {
						Some(Type::Variable {
							constraint: Some(constraint),
							..
						}) => match constraint.as_ref() {
							Type::Interface { type_args, .. } => type_args.clone(),
							_ => Vec::new(),
						},
						_ => Vec::new(),
					},
					members: Arc::new(interface_members.clone()),
					impls: Arc::new(implied_interfaces.clone()),
					def_key: None,
				})),
			},
			other => other,
		};
		let body_ctx = ctx
			.with_self_type(full_interface_type.clone())
			.with_new_entry(
				EcoString::from("this"),
				ContextEntry::Value(ContextValue {
					type_: full_interface_type.clone(),
					mutable: false,
					visibility: Visibility::Private,
				}),
			);
		let mutable_body_ctx = ctx
			.with_self_type(full_interface_type.clone())
			.with_new_entry(
				EcoString::from("this"),
				ContextEntry::Value(ContextValue {
					type_: full_interface_type.clone(),
					mutable: true,
					visibility: Visibility::Private,
				}),
			);
		let namespace_ctx = ctx.with_self_type(full_interface_type.clone());

		// Pass 2: type-check default bodies against the completed interface contract.
		for member_spanned in members {
			match &member_spanned.0 {
				InterfaceMember::Element(elem_spanned) => {
					self.check_interface_element_body(&elem_spanned.0, &body_ctx)?;
				}
				InterfaceMember::Namespace(impl_members) => {
					for impl_member in impl_members {
						self.check_impl_member_body(impl_member, &namespace_ctx)?;
					}
				}
				InterfaceMember::ImplMut(elements) => {
					for elem in elements {
						self.check_interface_element_body(&elem.0, &mutable_body_ctx)?;
					}
				}
				InterfaceMember::Impl {
					interface: (interface_ident, interface_generics),
					generics,
					members: impl_members,
				} => {
					let (generic_params, impl_ctx) =
						self.resolve_generic_params(generics, &body_ctx)?;
					let interface_ty = self.resolve_qualified_type(
						&interface_ident.0,
						interface_generics,
						interface_ident.1,
						&impl_ctx,
					)?;
					let provided_members = self.collect_impl_member_signatures(
						impl_members,
						&impl_ctx,
						StructMemberKind::Immutable,
					)?;
					self.validate_interface_impl(
						&full_interface_type,
						&interface_ty,
						&provided_members,
						member_spanned.1,
						&impl_ctx,
					)?;

					let impl_body_ctx = impl_ctx.with_impl_record(ImplRecord {
						generics: Arc::new(generic_params),
						receiver: full_interface_type.clone(),
						interface: interface_ty,
						span: span_to_range(member_spanned.1),
					});
					for impl_member in impl_members {
						self.check_impl_member_body(impl_member, &impl_body_ctx)?;
					}
				}
			}
		}

		Ok((interface_members, implied_interfaces))
	}

	/// Collect an interface element signature without type-checking its default body.
	fn collect_interface_func_signature(
		&mut self,
		meta: &FuncDeclaration,
		body: Option<&Spanned<Expr>>,
		ctx: &Context,
		infer_unannotated_return: bool,
	) -> Result<StructMember, TypeError> {
		let mut func_ctx = ctx.clone();
		let mut generic_params = Vec::with_capacity(meta.generics.len());

		for generic in &meta.generics {
			let constraint = match &generic.0.constraint {
				Some(c) => Some(self.resolve_ast_type(&c.0, c.1, ctx)?),
				None => None,
			};
			let id = self.fresh_type_var_id();
			generic_params.push(GenericParamInfo {
				id,
				name: generic.0.name.0.clone(),
				constraint: constraint.clone(),
				default: None,
			});
			func_ctx.insert_entry(
				generic.0.name.0.clone(),
				ContextEntry::Value(ContextValue {
					type_: Type::Variable {
						id,
						name: generic.0.name.0.clone(),
						constraint: constraint.map(Box::new),
					},
					mutable: false,
					visibility: Visibility::Private,
				}),
			);
		}

		let mut param_types = Vec::new();
		for param in &meta.params {
			let param_type = self.resolve_func_param_type(param, &func_ctx)?;
			let param_name = match &param.0.name.0 {
				Pattern::Binding { name, .. } => Some(name.0.clone()),
				Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
					Some(path[0].0.clone())
				}
				_ => None,
			};

			param_types.push((param_name.clone(), param_type.clone()));

			if let Some(name) = param_name {
				func_ctx.insert_entry(
					name,
					ContextEntry::Value(ContextValue {
						type_: param_type,
						mutable: param.0.mutable,
						visibility: Visibility::Private,
					}),
				);
			}
		}

		let return_type = match (&meta.return_type, body, infer_unannotated_return) {
			(Some(t), _, _) => self.resolve_ast_type(&t.0, t.1, ctx)?,
			(None, Some(body_expr), true) => self.infer(body_expr, &func_ctx)?,
			(None, Some(_), false) => self.fresh_var("_infer", None),
			(None, None, _) => Type::Void,
		};

		Ok(StructMember {
			type_: Box::new(Type::Function {
				generics: Arc::new(generic_params),
				params: param_types,
				has_spread: meta.params.last().is_some_and(|p| p.0.spread),
				return_type: Box::new(return_type),
				constructor: false,
			}),
			kind: StructMemberKind::Immutable,
			required: body.is_none(),
		})
	}

	fn check_interface_element(
		&mut self,
		element: &InterfaceElement,
		ctx: &Context,
	) -> Result<Option<(EcoString, StructMember)>, TypeError> {
		match element {
			InterfaceElement::Func { meta, body } => Ok(Some((
				meta.name.0.clone(),
				self.collect_interface_func_signature(meta, body.as_ref(), ctx, false)?,
			))),
			InterfaceElement::Let { meta, value } => {
				let let_type = match &meta.type_ {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => {
						// Infer from default value if present
						if let Some(val) = value {
							self.infer(val, ctx)?
						} else {
							self.fresh_var("_infer", None)
						}
					}
				};

				let name = match &meta.name.0 {
					Pattern::Binding { name, .. } => name.0.clone(),
					_ => return Ok(None),
				};

				Ok(Some((
					name,
					StructMember {
						type_: Box::new(let_type),
						kind: if meta.mutable {
							StructMemberKind::Mutable
						} else {
							StructMemberKind::Immutable
						},
						required: value.is_none(),
					},
				)))
			}
		}
	}

	fn check_interface_element_body(
		&mut self,
		element: &InterfaceElement,
		ctx: &Context,
	) -> Result<(), TypeError> {
		match element {
			InterfaceElement::Func { meta, body } => {
				let Some(body_expr) = body else {
					return Ok(());
				};

				let mut func_ctx = ctx.clone();
				for generic in &meta.generics {
					let constraint = match &generic.0.constraint {
						Some(c) => Some(Box::new(self.resolve_ast_type(&c.0, c.1, ctx)?)),
						None => None,
					};
					let id = self.fresh_type_var_id();
					func_ctx.insert_entry(
						generic.0.name.0.clone(),
						ContextEntry::Value(ContextValue {
							type_: Type::Variable {
								id,
								name: generic.0.name.0.clone(),
								constraint,
							},
							mutable: false,
							visibility: Visibility::Private,
						}),
					);
				}

				for param in &meta.params {
					let param_type = self.resolve_func_param_type(param, &func_ctx)?;
					let param_name = match &param.0.name.0 {
						Pattern::Binding { name, .. } => Some(name.0.clone()),
						Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
							Some(path[0].0.clone())
						}
						_ => None,
					};

					if let Some(name) = param_name {
						func_ctx.insert_entry(
							name,
							ContextEntry::Value(ContextValue {
								type_: param_type,
								mutable: param.0.mutable,
								visibility: Visibility::Private,
							}),
						);
					}
				}

				let return_type = match &meta.return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => self.infer(body_expr, &func_ctx)?,
				};
				let checked_return = self.check_expr(body_expr, &return_type, &func_ctx)?;
				if !checked_return.assignable_to(&return_type, &func_ctx) {
					return Err(TypeError {
						kind: TypeErrorKind::TypeMismatch {
							expected: return_type.into(),
							found: checked_return.into(),
						},
						span: span_to_range(body_expr.1),
					});
				}
				Ok(())
			}
			InterfaceElement::Let { meta, value } => {
				let Some(val) = value else {
					return Ok(());
				};

				let let_type = match &meta.type_ {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => self.infer(val, ctx)?,
				};
				let val_type = self.infer(val, ctx)?;
				if !val_type.assignable_to(&let_type, ctx) {
					return Err(TypeError {
						kind: TypeErrorKind::TypeMismatch {
							expected: let_type.into(),
							found: val_type.into(),
						},
						span: span_to_range(val.1),
					});
				}
				Ok(())
			}
		}
	}

	/// Collect the signature of an ImplMember without type-checking its body.
	/// For functions, this resolves param types and return type annotation.
	/// For let bindings, this resolves the type annotation (or infers from value if no annotation).
	fn collect_impl_member_signature(
		&mut self,
		member: &Spanned<ImplMember>,
		member_ctx: &Context,
		member_kind: StructMemberKind,
	) -> Result<Option<(EcoString, StructMember)>, TypeError> {
		match &member.0 {
			ImplMember::Let {
				visibility: _,
				meta: LetDeclaration {
					name,
					type_,
					mutable,
				},
				value,
			} => {
				let final_type = match type_ {
					Some(t) => self.resolve_ast_type(&t.0, t.1, member_ctx)?,
					None => self.infer(value, member_ctx)?,
				};

				if let Pattern::Binding {
					name: binding_name, ..
				} = &name.0
				{
					let kind = if *mutable {
						StructMemberKind::Mutable
					} else {
						member_kind
					};
					Ok(Some((
						binding_name.0.clone(),
						StructMember {
							type_: Box::new(final_type),
							kind,
							required: false,
						},
					)))
				} else {
					Ok(None)
				}
			}
			ImplMember::ExternalLet(
				_visibility,
				_external_name,
				LetDeclaration {
					name,
					type_,
					mutable,
				},
			) => {
				let let_type = match type_ {
					Some(t) => self.resolve_ast_type(&t.0, t.1, member_ctx)?,
					None => self.fresh_var("_infer", None),
				};

				if let Pattern::Binding {
					name: binding_name, ..
				} = &name.0
				{
					let kind = if *mutable {
						StructMemberKind::Mutable
					} else {
						member_kind
					};
					Ok(Some((
						binding_name.0.clone(),
						StructMember {
							type_: Box::new(let_type),
							kind,
							required: false,
						},
					)))
				} else {
					Ok(None)
				}
			}
			ImplMember::Func {
				visibility: _,
				meta:
					FuncDeclaration {
						name: Spanned(func_name, _),
						generics,
						params,
						return_type,
					},
				body: _,
			}
			| ImplMember::ExternalFunc(
				_,
				_,
				FuncDeclaration {
					name: Spanned(func_name, _),
					generics,
					params,
					return_type,
				},
			) => {
				let mut func_ctx = member_ctx.clone();

				for generic in generics {
					let constraint = match &generic.0.constraint {
						Some(c) => Some(Box::new(self.resolve_ast_type(&c.0, c.1, member_ctx)?)),
						None => None,
					};
					let id = self.fresh_type_var_id();
					func_ctx.insert_entry(
						generic.0.name.0.clone(),
						ContextEntry::Value(ContextValue {
							type_: Type::Variable {
								id,
								name: generic.0.name.0.clone(),
								constraint,
							},
							mutable: false,
							visibility: Visibility::Private,
						}),
					);
				}

				let mut param_types = Vec::new();
				for param in params {
					let param_type = {
						let ty = self.resolve_ast_type(&param.0.type_.0, param.0.type_.1, &func_ctx)?;
						if param.0.spread {
							Type::List { item: Box::new(ty) }
						} else {
							ty
						}
					};
					let param_name = match &param.0.name.0 {
						Pattern::Binding { name, .. } => Some(name.0.clone()),
						Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
							Some(path[0].0.clone())
						}
						_ => None,
					};
					param_types.push((param_name.clone(), param_type.clone()));

					if let Some(name) = param_name {
						func_ctx.insert_entry(
							name,
							ContextEntry::Value(ContextValue {
								type_: param_type,
								mutable: param.0.mutable,
								visibility: Visibility::Private,
							}),
						);
					}
				}

				let return_ty = match return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, &func_ctx)?,
					None => self.fresh_var("_infer", None),
				};

				let func_type = Type::Function {
					generics: Arc::new(Vec::new()),
					params: param_types,
					has_spread: params.last().is_some_and(|p| p.0.spread),
					return_type: Box::new(return_ty),
					constructor: false,
				};

				Ok(Some((
					func_name.clone(),
					StructMember {
						type_: Box::new(func_type),
						kind: member_kind,
						required: false,
					},
				)))
			}
		}
	}

	/// Type-check a single ImplMember body against the context (which should already have
	/// all sibling member signatures). Returns the checked type for functions without
	/// explicit return type annotations.
	fn check_impl_member_body(
		&mut self,
		member: &Spanned<ImplMember>,
		member_ctx: &Context,
	) -> Result<(), TypeError> {
		match &member.0 {
			ImplMember::Let {
				visibility: _,
				meta: LetDeclaration { type_: Some(t), .. },
				value,
			} => {
				let expected = self.resolve_ast_type(&t.0, t.1, member_ctx)?;
				self.check_expr(value, &expected, member_ctx)?;
				Ok(())
			}
			ImplMember::Let { value, .. } => {
				self.infer(value, member_ctx)?;
				Ok(())
			}
			ImplMember::Func {
				visibility: _,
				meta: FuncDeclaration {
					generics,
					params,
					return_type,
					..
				},
				body,
			} => {
				let mut func_ctx = member_ctx.clone();

				for generic in generics {
					let constraint = match &generic.0.constraint {
						Some(c) => Some(Box::new(self.resolve_ast_type(&c.0, c.1, member_ctx)?)),
						None => None,
					};
					let id = self.fresh_type_var_id();
					func_ctx.insert_entry(
						generic.0.name.0.clone(),
						ContextEntry::Value(ContextValue {
							type_: Type::Variable {
								id,
								name: generic.0.name.0.clone(),
								constraint,
							},
							mutable: false,
							visibility: Visibility::Private,
						}),
					);
				}

				for param in params {
					let param_type = {
						let ty = self.resolve_ast_type(&param.0.type_.0, param.0.type_.1, &func_ctx)?;
						if param.0.spread {
							Type::List { item: Box::new(ty) }
						} else {
							ty
						}
					};
					let param_name = match &param.0.name.0 {
						Pattern::Binding { name, .. } => Some(name.0.clone()),
						Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
							Some(path[0].0.clone())
						}
						_ => None,
					};
					if let Some(name) = param_name {
						func_ctx.insert_entry(
							name,
							ContextEntry::Value(ContextValue {
								type_: param_type,
								mutable: param.0.mutable,
								visibility: Visibility::Private,
							}),
						);
					}
				}

				let expected_return = match return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, member_ctx)?,
					None => self.infer(body, &func_ctx)?,
				};
				let inferred_return = self.check_expr(body, &expected_return, &func_ctx)?;

				if !inferred_return.assignable_to(&expected_return, &func_ctx) {
					return Err(TypeError {
						kind: TypeErrorKind::TypeMismatch {
							expected: expected_return.into(),
							found: inferred_return.into(),
						},
						span: span_to_range(body.1),
					});
				}

				Ok(())
			}
			ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => Ok(()),
		}
	}

	/// Type-check a single ImplMember and return its name, type, and member kind.
	/// This is used for standalone impl blocks where forward-referencing is not needed.
	fn check_impl_member(
		&mut self,
		member: &Spanned<ImplMember>,
		member_ctx: &Context,
		member_kind: StructMemberKind,
	) -> Result<Option<(EcoString, StructMember)>, TypeError> {
		let sig = self.collect_impl_member_signature(member, member_ctx, member_kind)?;
		self.check_impl_member_body(member, member_ctx)?;
		Ok(sig)
	}

	fn collect_impl_member_signatures(
		&mut self,
		members: &[Spanned<ImplMember>],
		member_ctx: &Context,
		member_kind: StructMemberKind,
	) -> Result<BTreeMap<EcoString, StructMember>, TypeError> {
		let mut signatures = BTreeMap::new();
		for member in members {
			if let Some((name, signature)) =
				self.collect_impl_member_signature(member, member_ctx, member_kind)?
			{
				signatures.insert(name, signature);
			}
		}
		Ok(signatures)
	}

	fn collect_interface_contract_members(
		&self,
		receiver: &Type,
		interface: &Type,
		members: &mut BTreeMap<EcoString, StructMember>,
	) {
		let normalized = self.substitute_self_type(interface, receiver);
		let Type::Interface {
			members: local_members,
			impls,
			..
		} = normalized
		else {
			return;
		};

		for (name, member) in local_members.iter() {
			members
				.entry(name.clone())
				.or_insert_with(|| member.clone());
		}

		for implied in impls.values() {
			self.collect_interface_contract_members(receiver, implied, members);
		}
	}

	fn validate_interface_impl(
		&self,
		receiver: &Type,
		interface: &Type,
		provided_members: &BTreeMap<EcoString, StructMember>,
		span: Span,
		ctx: &Context,
	) -> Result<(), TypeError> {
		let mut contract_members = BTreeMap::new();
		self.collect_interface_contract_members(receiver, interface, &mut contract_members);

		for (name, provided_member) in provided_members {
			let Some(expected_member) = contract_members.get(name) else {
				return Err(TypeError {
					kind: TypeErrorKind::UnknownMember {
						type_: interface.clone().into(),
						member: name.clone(),
						suggestion: find_similar_name(name, contract_members.keys()),
					},
					span: span_to_range(span),
				});
			};

			let provided_type = provided_member.type_.as_ref();
			let expected_type = expected_member.type_.as_ref();
			if !self.type_satisfies(provided_type, expected_type, ctx)
				|| !self.type_satisfies(expected_type, provided_type, ctx)
			{
				return Err(TypeError {
					kind: TypeErrorKind::IncompatibleImplMember {
						member: name.clone(),
						expected: expected_type.clone().into(),
						found: provided_type.clone().into(),
					},
					span: span_to_range(span),
				});
			}
		}

		let missing_members: Vec<_> = contract_members
			.iter()
			.filter(|(name, member)| member.required && !provided_members.contains_key(*name))
			.map(|(name, _)| name.clone())
			.collect();

		if missing_members.is_empty() {
			Ok(())
		} else {
			Err(TypeError {
				kind: TypeErrorKind::MissingImplMembers {
					type_: receiver.clone().into(),
					interface: interface.clone().into(),
					members: missing_members,
				},
				span: span_to_range(span),
			})
		}
	}

	fn types_overlap(&self, left: &Type, right: &Type) -> bool {
		let mut left_subst = HashMap::new();
		let mut right_subst = HashMap::new();
		self.types_overlap_inner(left, right, &mut left_subst, &mut right_subst)
	}

	fn types_overlap_inner(
		&self,
		left: &Type,
		right: &Type,
		left_subst: &mut HashMap<TypeVarId, Type>,
		right_subst: &mut HashMap<TypeVarId, Type>,
	) -> bool {
		match (left, right) {
			(Type::Never, _) | (_, Type::Never) => false,
			(Type::Variable { id, .. }, _) => match left_subst.get(id).cloned() {
				Some(existing) => self.types_overlap_inner(&existing, right, left_subst, right_subst),
				None => {
					left_subst.insert(*id, right.clone());
					true
				}
			},
			(_, Type::Variable { id, .. }) => match right_subst.get(id).cloned() {
				Some(existing) => self.types_overlap_inner(left, &existing, left_subst, right_subst),
				None => {
					right_subst.insert(*id, left.clone());
					true
				}
			},
			(Type::Int, Type::Int)
			| (Type::Float, Type::Float)
			| (Type::Char, Type::Char)
			| (Type::String, Type::String)
			| (Type::Boolean, Type::Boolean)
			| (Type::Void, Type::Void) => true,
			(Type::List { item: left_item }, Type::List { item: right_item }) => {
				self.types_overlap_inner(left_item, right_item, left_subst, right_subst)
			}
			(Type::Tuple { items: left_items }, Type::Tuple { items: right_items }) => {
				left_items.len() == right_items.len()
					&& left_items
						.iter()
						.zip(right_items)
						.all(|(left_item, right_item)| {
							self.types_overlap_inner(left_item, right_item, left_subst, right_subst)
						})
			}
			(
				Type::Map {
					key: left_key,
					value: left_value,
				},
				Type::Map {
					key: right_key,
					value: right_value,
				},
			) => {
				self.types_overlap_inner(left_key, right_key, left_subst, right_subst)
					&& self.types_overlap_inner(left_value, right_value, left_subst, right_subst)
			}
			(
				Type::Function {
					params: left_params,
					return_type: left_return,
					..
				},
				Type::Function {
					params: right_params,
					return_type: right_return,
					..
				},
			) => {
				left_params.len() == right_params.len()
					&& left_params
						.iter()
						.zip(right_params)
						.all(|((_, left_param), (_, right_param))| {
							self.types_overlap_inner(left_param, right_param, left_subst, right_subst)
						}) && self.types_overlap_inner(left_return, right_return, left_subst, right_subst)
			}
			(
				Type::Struct {
					type_args: left_args,
					..
				},
				Type::Struct {
					type_args: right_args,
					..
				},
			)
			| (
				Type::Enum {
					type_args: left_args,
					..
				},
				Type::Enum {
					type_args: right_args,
					..
				},
			)
			| (
				Type::Interface {
					type_args: left_args,
					..
				},
				Type::Interface {
					type_args: right_args,
					..
				},
			) => {
				self.same_type_identity(left, right)
					&& left_args.len() == right_args.len()
					&& left_args
						.iter()
						.zip(right_args)
						.all(|(left_arg, right_arg)| {
							self.types_overlap_inner(left_arg, right_arg, left_subst, right_subst)
						})
			}
			(
				Type::EnumVariant {
					variant_name: left_variant,
					variant_of: left_of,
					..
				},
				Type::EnumVariant {
					variant_name: right_variant,
					variant_of: right_of,
					..
				},
			) => {
				left_variant == right_variant
					&& self.types_overlap_inner(left_of, right_of, left_subst, right_subst)
			}
			(Type::Intersection { first, second }, _) => {
				self.types_overlap_inner(first, right, left_subst, right_subst)
					&& self.types_overlap_inner(second, right, left_subst, right_subst)
			}
			(_, Type::Intersection { first, second }) => {
				self.types_overlap_inner(left, first, left_subst, right_subst)
					&& self.types_overlap_inner(left, second, left_subst, right_subst)
			}
			(Type::Module { .. }, Type::Module { .. }) => self.same_type_identity(left, right),
			_ => false,
		}
	}

	fn check_conflicting_impl_record(
		&self,
		record: &ImplRecord,
		existing_records: &[ImplRecord],
	) -> Result<(), TypeError> {
		if let Some(existing) = existing_records.iter().find(|existing| {
			self.types_overlap(&record.receiver, &existing.receiver)
				&& self.types_overlap(&record.interface, &existing.interface)
		}) {
			return Err(TypeError {
				kind: TypeErrorKind::ConflictingImpls {
					receiver: record.receiver.clone().into(),
					interface: record.interface.clone().into(),
					first_span: existing.span.clone(),
					second_span: record.span.clone(),
				},
				span: record.span.clone(),
			});
		}

		Ok(())
	}

	/// Type-check struct/enum inner members and return the processed members map
	/// Also returns a list of interface implementations
	fn process_struct_members(
		&mut self,
		members: &[Spanned<StructInnerMember>],
		self_type: &Type,
		base_ctx: &Context,
	) -> Result<ProcessMembersResult, TypeError> {
		let mut result_members = BTreeMap::new();
		let mut result_impls = BTreeMap::new();
		let mut result_impl_records = Vec::new();

		// Add constructor to context based on self_type, and set self_type in context
		let base_ctx_with_self = base_ctx.with_self_type(self_type.clone());
		let ctx_with_constructor = match self_type {
			Type::Struct {
				name,
				generics,
				fields,
				..
			} => {
				// Create a function type for the struct constructor
				let mut param_types = Vec::new();
				for (field_name, field_type) in fields.iter() {
					param_types.push((Some(field_name.clone()), field_type.clone()));
				}

				let constructor_type = Type::Function {
					generics: generics.clone(),
					params: param_types,
					has_spread: false,
					return_type: Box::new(self_type.clone()),
					constructor: true,
				};

				base_ctx_with_self.with_new_entry(
					name.clone(),
					ContextEntry::Value(ContextValue {
						type_: constructor_type,
						mutable: false,
						visibility: Visibility::Private,
					}),
				)
			}
			Type::Enum { .. } => {
				// For enums, each variant is a constructor
				// Within the enum's member methods, variant constructors should NOT be generic
				// functions - they should use the type variables from the enclosing scope
				self.inject_enum_variants(self_type, &base_ctx_with_self)
			}
			_ => base_ctx_with_self.clone(),
		};

		// Multi-pass approach to handle forward references between members:
		// Pass 1: Collect signatures for all members (functions with explicit return types get
		//         their type from the annotation; functions without need body inference)
		// Pass 2: For functions without return type annotations, infer from body with intermediate context
		// Pass 3: Check all member bodies with the complete self_type

		struct DeferredMember<'a> {
			member: &'a Spanned<ImplMember>,
			base_ctx: Context,
			kind: StructMemberKind,
			mutable_this: bool,
			has_this: bool,
		}
		let mut all_members: Vec<DeferredMember<'_>> = Vec::new();

		let sig_ctx = ctx_with_constructor.with_new_entry(
			EcoString::from("this"),
			ContextEntry::Value(ContextValue {
				type_: self_type.clone(),
				mutable: false,
				visibility: Visibility::Private,
			}),
		);

		// === Pass 1: collect signatures with explicit return types ===
		for member_spanned in members {
			match &member_spanned.0 {
				StructInnerMember::Member(impl_member) => {
					if let Some((name, struct_member)) = self.collect_impl_member_signature(
						impl_member,
						&sig_ctx,
						StructMemberKind::Immutable,
					)? {
						result_members.insert(name, struct_member);
					}
					all_members.push(DeferredMember {
						member: impl_member,
						base_ctx: ctx_with_constructor.clone(),
						kind: StructMemberKind::Immutable,
						mutable_this: false,
						has_this: true,
					});
				}
				StructInnerMember::Namespace(namespace_members) => {
					for impl_member in namespace_members {
						if let Some((name, struct_member)) = self.collect_impl_member_signature(
							impl_member,
							&ctx_with_constructor,
							StructMemberKind::Namespace,
						)? {
							result_members.insert(name, struct_member);
						}
						all_members.push(DeferredMember {
							member: impl_member,
							base_ctx: ctx_with_constructor.clone(),
							kind: StructMemberKind::Namespace,
							mutable_this: false,
							has_this: false,
						});
					}
				}
				StructInnerMember::ImplMut(mutable_members) => {
					let mutable_sig_ctx = base_ctx_with_self.with_new_entry(
						EcoString::from("this"),
						ContextEntry::Value(ContextValue {
							type_: self_type.clone(),
							mutable: true,
							visibility: Visibility::Private,
						}),
					);
					for impl_member in mutable_members {
						if let Some((name, struct_member)) = self.collect_impl_member_signature(
							impl_member,
							&mutable_sig_ctx,
							StructMemberKind::Mutable,
						)? {
							result_members.insert(name, struct_member);
						}
						all_members.push(DeferredMember {
							member: impl_member,
							base_ctx: base_ctx_with_self.clone(),
							kind: StructMemberKind::Mutable,
							mutable_this: true,
							has_this: true,
						});
					}
				}
				StructInnerMember::Impl {
					interface: (interface_ident, interface_generics),
					generics: impl_generics,
					members: impl_members,
				} => {
					let (impl_generic_params, impl_ctx) =
						self.resolve_generic_params(impl_generics, &sig_ctx)?;
					let interface_ty = self.resolve_qualified_type(
						&interface_ident.0,
						interface_generics,
						interface_ident.1,
						&impl_ctx,
					)?;
					let provided_members = self.collect_impl_member_signatures(
						impl_members,
						&impl_ctx,
						StructMemberKind::Immutable,
					)?;
					self.validate_interface_impl(
						self_type,
						&interface_ty,
						&provided_members,
						member_spanned.1,
						&impl_ctx,
					)?;

					let impl_record = ImplRecord {
						generics: Arc::new(impl_generic_params),
						receiver: self_type.clone(),
						interface: interface_ty.clone(),
						span: span_to_range(member_spanned.1),
					};
					self.check_conflicting_impl_record(&impl_record, &result_impl_records)?;
					result_impl_records.push(impl_record.clone());

					if let Type::Interface { name, .. } = &interface_ty {
						result_impls.insert(name.clone(), interface_ty.clone());
					}

					for impl_member in impl_members {
						if let Some((name, struct_member)) = self.collect_impl_member_signature(
							impl_member,
							&impl_ctx,
							StructMemberKind::Immutable,
						)? {
							result_members.insert(name, struct_member);
						}
						all_members.push(DeferredMember {
							member: impl_member,
							base_ctx: impl_ctx.with_impl_record(impl_record.clone()),
							kind: StructMemberKind::Immutable,
							mutable_this: false,
							has_this: true,
						});
					}
				}
			}
		}

		// Build a helper to create the self_type with current members
		let build_self_type =
			|members: &BTreeMap<EcoString, StructMember>, impls: &BTreeMap<EcoString, Type>| -> Type {
				match self_type {
					Type::Struct {
						name,
						def_key,
						generics,
						type_args,
						fields,
						..
					} => Type::Struct {
						name: name.clone(),
						def_key: *def_key,
						generics: generics.clone(),
						type_args: type_args.clone(),
						fields: fields.clone(),
						members: Arc::new(members.clone()),
						impls: Arc::new(impls.clone()),
					},
					Type::Enum {
						name,
						def_key,
						generics,
						type_args,
						variants,
						..
					} => Type::Enum {
						name: name.clone(),
						def_key: *def_key,
						generics: generics.clone(),
						type_args: type_args.clone(),
						variants: variants.clone(),
						members: Arc::new(members.clone()),
						impls: Arc::new(impls.clone()),
					},
					other => other.clone(),
				}
			};

		// === Pass 2: Infer return types for functions without explicit annotations ===
		// Rebuild self_type incrementally so each function sees previously inferred return types
		for dm in &all_members {
			if let ImplMember::Func {
				meta:
					FuncDeclaration {
						return_type: None,
						name: Spanned(func_name, _),
						generics,
						params,
					},
				body,
				..
			} = &dm.member.0
			{
				let current_self_type = build_self_type(&result_members, &result_impls);
				let member_ctx = if dm.has_this {
					dm.base_ctx
						.with_self_type(current_self_type.clone())
						.with_new_entry(
							EcoString::from("this"),
							ContextEntry::Value(ContextValue {
								type_: current_self_type,
								mutable: dm.mutable_this,
								visibility: Visibility::Private,
							}),
						)
				} else {
					dm.base_ctx.clone()
				};

				let mut func_ctx = member_ctx.clone();
				for generic in generics {
					let constraint = match &generic.0.constraint {
						Some(c) => Some(Box::new(self.resolve_ast_type(&c.0, c.1, &member_ctx)?)),
						None => None,
					};
					let id = self.fresh_type_var_id();
					func_ctx.insert_entry(
						generic.0.name.0.clone(),
						ContextEntry::Value(ContextValue {
							type_: Type::Variable {
								id,
								name: generic.0.name.0.clone(),
								constraint,
							},
							mutable: false,
							visibility: Visibility::Private,
						}),
					);
				}
				let mut param_types = Vec::new();
				for param in params {
					let param_type = self.resolve_func_param_type(param, &func_ctx)?;
					let param_name = match &param.0.name.0 {
						Pattern::Binding { name, .. } => Some(name.0.clone()),
						Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
							Some(path[0].0.clone())
						}
						_ => None,
					};
					param_types.push((param_name.clone(), param_type.clone()));
					if let Some(name) = param_name {
						func_ctx.insert_entry(
							name,
							ContextEntry::Value(ContextValue {
								type_: param_type,
								mutable: param.0.mutable,
								visibility: Visibility::Private,
							}),
						);
					}
				}

				let inferred_return = self.infer(body, &func_ctx)?;
				let func_type = Type::Function {
					generics: Arc::new(Vec::new()),
					params: param_types,
					has_spread: params.last().is_some_and(|p| p.0.spread),
					return_type: Box::new(inferred_return),
					constructor: false,
				};

				result_members.insert(
					func_name.clone(),
					StructMember {
						type_: Box::new(func_type),
						kind: dm.kind,
						required: false,
					},
				);
			}
		}

		// === Pass 3: Check all member bodies with the complete self_type ===
		let complete_self_type = build_self_type(&result_members, &result_impls);

		for dm in &all_members {
			let body_ctx = if dm.has_this {
				dm.base_ctx
					.with_self_type(complete_self_type.clone())
					.with_new_entry(
						EcoString::from("this"),
						ContextEntry::Value(ContextValue {
							type_: complete_self_type.clone(),
							mutable: dm.mutable_this,
							visibility: Visibility::Private,
						}),
					)
			} else {
				dm.base_ctx.clone()
			};
			self.check_impl_member_body(dm.member, &body_ctx)?;
		}

		Ok((result_members, result_impls, result_impl_records))
	}

	/// Check an impl declaration and register implementations
	/// If `ty` is an enum, inject its variant constructors into `ctx` and return the extended context.
	/// Otherwise, return `ctx` unchanged.
	fn enum_variant_context_entries(
		ty: &Type,
		visibility: Visibility,
	) -> Vec<(EcoString, ContextEntry)> {
		let Type::Enum { variants, .. } = ty else {
			return Vec::new();
		};

		variants
			.iter()
			.map(|(variant_name, variant_fields)| {
				let mut param_types = Vec::new();
				for (field_name, field_type) in variant_fields {
					param_types.push((Some(field_name.clone()), field_type.clone()));
				}

				(
					variant_name.clone(),
					ContextEntry::Value(ContextValue {
						type_: Type::Function {
							generics: Arc::new(Vec::new()),
							params: param_types,
							has_spread: false,
							return_type: Box::new(ty.clone()),
							constructor: false,
						},
						mutable: false,
						visibility,
					}),
				)
			})
			.collect()
	}

	fn inject_enum_variants(&self, ty: &Type, ctx: &Context) -> Context {
		if !matches!(ty, Type::Enum { .. }) {
			return ctx.clone();
		}

		let mut ctx = ctx.clone();
		for (name, entry) in Self::enum_variant_context_entries(ty, Visibility::Private) {
			ctx.insert_entry(name, entry);
		}
		ctx
	}

	fn process_impl(&mut self, impl_decl: &Declaration, ctx: &Context) -> Result<Context, TypeError> {
		match impl_decl {
			Declaration::ImplFor {
				visibility: _,
				generics,
				mutable,
				type_,
				for_interface: (interface_ident, interface_generics),
				members,
			} => {
				let (generic_params, impl_ctx) = self.resolve_generic_params(generics, ctx)?;
				let ty = self.resolve_ast_type(&type_.0, type_.1, &impl_ctx)?;
				let interface_ctx = impl_ctx.with_self_type(ty.clone());
				let interface_ty = self.resolve_qualified_type(
					&interface_ident.0,
					interface_generics,
					interface_ident.1,
					&interface_ctx,
				)?;

				let member_ctx = interface_ctx.with_new_entry(
					EcoString::from("this"),
					ContextEntry::Value(ContextValue {
						type_: ty.clone(),
						mutable: *mutable,
						visibility: Visibility::Private,
					}),
				);
				let member_ctx = self.inject_enum_variants(&ty, &member_ctx);

				let member_kind = if *mutable {
					StructMemberKind::Mutable
				} else {
					StructMemberKind::Immutable
				};
				let provided_members =
					self.collect_impl_member_signatures(members, &member_ctx, member_kind)?;
				self.validate_interface_impl(
					&ty,
					&interface_ty,
					&provided_members,
					interface_ident.1,
					&member_ctx,
				)?;

				let impl_record = ImplRecord {
					generics: Arc::new(generic_params),
					receiver: ty.clone(),
					interface: interface_ty,
					span: span_to_range(interface_ident.1),
				};
				self.check_conflicting_impl_record(&impl_record, &ctx.impl_records)?;

				let body_ctx = member_ctx.with_impl_record(impl_record.clone());
				for member in members {
					self.check_impl_member_body(member, &body_ctx)?;
				}

				Ok(ctx.with_impl_record(impl_record))
			}
			Declaration::Impl {
				visibility: _,
				generics,
				mutable,
				type_,
				members,
			} => {
				let (generic_params, impl_ctx) = self.resolve_generic_params(generics, ctx)?;
				let ty = self.resolve_ast_type(&type_.0, type_.1, &impl_ctx)?;

				// Set up context with self_type and this for member type-checking
				let member_ctx = impl_ctx.with_self_type(ty.clone()).with_new_entry(
					EcoString::from("this"),
					ContextEntry::Value(ContextValue {
						type_: ty.clone(),
						mutable: *mutable,
						visibility: Visibility::Private,
					}),
				);
				let member_ctx = self.inject_enum_variants(&ty, &member_ctx);

				let member_kind = if *mutable {
					StructMemberKind::Mutable
				} else {
					StructMemberKind::Immutable
				};
				let provided_members =
					self.collect_impl_member_signatures(members, &member_ctx, member_kind)?;

				if matches!(ty, Type::Interface { .. }) {
					let extension = InterfaceExtensionRecord {
						generics: Arc::new(generic_params),
						interface: ty.clone(),
						members: provided_members,
						span: span_to_range(type_.1),
					};
					let body_ctx = member_ctx.with_interface_extension(extension.clone());
					for member in members {
						self.check_impl_member_body(member, &body_ctx)?;
					}
					Ok(ctx.with_interface_extension(extension))
				} else {
					let extension = TypeExtensionRecord {
						generics: Arc::new(generic_params),
						receiver: ty.clone(),
						members: provided_members,
						span: span_to_range(type_.1),
					};
					let body_ctx = member_ctx.with_type_extension(extension.clone());
					for member in members {
						self.check_impl_member_body(member, &body_ctx)?;
					}

					Ok(ctx.with_type_extension(extension))
				}
			}
			_ => Ok(ctx.clone()),
		}
	}

	pub fn check_declaration(
		&mut self,
		declaration: &Declaration,
		ctx: &Context,
	) -> Result<Context, TypeError> {
		match declaration {
			Declaration::Let {
				visibility: _,
				meta: LetDeclaration {
					name,
					type_,
					mutable,
				},
				value,
			} => {
				let final_type = match type_ {
					Some(t) => {
						let expected = self.resolve_ast_type(&t.0, t.1, ctx)?;
						self.check_expr(value, &expected, ctx)?
					}
					None => self.infer(value, ctx)?,
				};

				let binding_name = match &name.0 {
					Pattern::Binding { name, .. } => Some(name.0.clone()),
					Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
						Some(path[0].0.clone())
					}
					_ => None,
				};

				if let Some(binding_name) = binding_name {
					Ok(ctx.with_new_entry(
						binding_name,
						ContextEntry::Value(ContextValue {
							type_: final_type,
							mutable: *mutable,
							visibility: Visibility::Private,
						}),
					))
				} else {
					Ok(ctx.clone())
				}
			}
			Declaration::Func {
				visibility: _,
				meta:
					FuncDeclaration {
						name: Spanned(func_name, _),
						generics,
						params,
						return_type,
					},
				body,
			} => {
				let (generic_params, mut func_ctx) = self.resolve_generic_params(generics, ctx)?;

				// Add parameters to context and collect parameter types
				let mut param_types = Vec::new();
				for param in params {
					let param_type = self.resolve_func_param_type(param, &func_ctx)?;

					// Extract parameter name from pattern
					let param_name = match &param.0.name.0 {
						Pattern::Binding { name, .. } => Some(name.0.clone()),
						// Handle bare identifiers parsed as struct patterns with no fields
						Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
							Some(path[0].0.clone())
						}
						_ => None,
					};

					param_types.push((param_name.clone(), param_type.clone()));

					if let Some(name) = param_name {
						func_ctx.insert_entry(
							name,
							ContextEntry::Value(ContextValue {
								type_: param_type,
								mutable: param.0.mutable,
								visibility: Visibility::Private,
							}),
						);
					}
				}

				// Check body
				let expected_return = match return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => self.infer(body, &func_ctx)?,
				};
				let inferred_return = self.check_expr(body, &expected_return, &func_ctx)?;

				if !inferred_return.assignable_to(&expected_return, &func_ctx) {
					return Err(TypeError {
						kind: TypeErrorKind::TypeMismatch {
							expected: expected_return.into(),
							found: inferred_return.into(),
						},
						span: 0..0,
					});
				}

				let func_type = Type::Function {
					generics: Arc::new(generic_params),
					params: param_types,
					has_spread: params.last().map(|p| p.0.spread).unwrap_or(false),
					return_type: Box::new(expected_return),
					constructor: false,
				};

				Ok(ctx.with_new_entry(
					func_name.clone(),
					ContextEntry::Value(ContextValue {
						type_: func_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				))
			}
			Declaration::Struct {
				visibility: _,
				name,
				generics,
				fields,
				members,
			} => {
				if fields.is_empty() {
					return Err(TypeError {
						kind: TypeErrorKind::EmptyStruct {
							name: name.0.clone(),
						},
						span: span_to_range(name.1),
					});
				}

				let (generic_params, mut struct_ctx) = self.resolve_generic_params(generics, ctx)?;

				// Register a forward-declaration so fields can self-reference the struct
				let forward_struct_type = Type::Struct {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					fields: Arc::new(BTreeMap::new()),
					members: Arc::new(BTreeMap::new()),
					impls: Arc::new(BTreeMap::new()),
					def_key: None,
				};
				struct_ctx = struct_ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: forward_struct_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				);

				let mut field_map = BTreeMap::new();
				for field in fields {
					let field_type = self.resolve_ast_type(&field.0.type_.0, field.0.type_.1, &struct_ctx)?;

					// Type-check the default value if present
					if let Some(default_expr) = &field.0.default {
						let default_type = self.infer(default_expr, &struct_ctx)?;
						if !default_type.assignable_to(&field_type, &struct_ctx) {
							return Err(TypeError {
								kind: TypeErrorKind::TypeMismatch {
									expected: field_type.into(),
									found: default_type.into(),
								},
								span: span_to_range(default_expr.1),
							});
						}
					}

					field_map.insert(field.0.name.0.clone(), field_type.clone());

					// Also add field to context so members can access fields directly
					struct_ctx.insert_entry(
						field.0.name.0.clone(),
						ContextEntry::Value(ContextValue {
							type_: field_type,
							mutable: false,
							visibility: Visibility::Private,
						}),
					);
				}

				// Create a preliminary struct type for self-reference in members
				let preliminary_struct_type = Type::Struct {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					fields: Arc::new(field_map.clone()),
					members: Arc::new(BTreeMap::new()),
					impls: Arc::new(BTreeMap::new()),
					def_key: None,
				};

				// Add preliminary type to context so members can self-reference by name
				let struct_ctx_with_self = struct_ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: preliminary_struct_type.clone(),
						mutable: false,
						visibility: Visibility::Public,
					}),
				);

				// Process struct members (methods, computed properties, etc.)
				let (processed_members, processed_impls, processed_impl_records) =
					self.process_struct_members(members, &preliminary_struct_type, &struct_ctx_with_self)?;

				let struct_type = Type::Struct {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					fields: Arc::new(field_map.clone()),
					members: Arc::new(processed_members),
					impls: Arc::new(processed_impls),
					def_key: None,
				};

				// Build the constructor function type for the struct
				// Use `fields` (source order) instead of `field_map` (alphabetical BTreeMap order)
				let constructor_params: Vec<(Option<EcoString>, Type)> = fields
					.iter()
					.map(|field| {
						let field_type = field_map[&field.0.name.0].clone();
						(Some(field.0.name.0.clone()), field_type)
					})
					.collect();

				let constructor_type = Type::Function {
					generics: Arc::new(generic_params),
					params: constructor_params,
					has_spread: false,
					return_type: Box::new(struct_type.clone()),
					constructor: true,
				};

				let mut next_ctx = ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: constructor_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				);
				for record in processed_impl_records {
					next_ctx = next_ctx.with_impl_record(record);
				}
				Ok(next_ctx)
			}
			Declaration::Enum {
				visibility: _,
				name,
				generics,
				variants,
				members,
			} => {
				let (generic_params, mut enum_ctx) = self.resolve_generic_params(generics, ctx)?;

				// Register a forward-declaration so variant fields can self-reference the enum
				let forward_enum_type = Type::Enum {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					variants: Arc::new(BTreeMap::new()),
					members: Arc::new(BTreeMap::new()),
					impls: Arc::new(BTreeMap::new()),
					def_key: None,
				};
				enum_ctx = enum_ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: forward_enum_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				);

				let mut variants_map = BTreeMap::new();
				for variant in variants {
					let mut variant_fields = BTreeMap::new();
					for field in &variant.0.fields {
						let field_type = self.resolve_ast_type(&field.0.type_.0, field.0.type_.1, &enum_ctx)?;

						// Type-check the default value if present
						if let Some(default_expr) = &field.0.default {
							let default_type = self.infer(default_expr, &enum_ctx)?;
							if !default_type.assignable_to(&field_type, &enum_ctx) {
								return Err(TypeError {
									kind: TypeErrorKind::TypeMismatch {
										expected: field_type.into(),
										found: default_type.into(),
									},
									span: span_to_range(default_expr.1),
								});
							}
						}

						variant_fields.insert(field.0.name.0.clone(), field_type);
					}
					variants_map.insert(variant.0.name.0.clone(), variant_fields);
				}

				// Create a preliminary enum type for self-reference in members
				let preliminary_enum_type = Type::Enum {
					name: name.0.clone(),
					generics: Arc::new(generic_params.clone()),
					type_args: Vec::new(),
					variants: Arc::new(variants_map.clone()),
					members: Arc::new(BTreeMap::new()),
					impls: Arc::new(BTreeMap::new()),
					def_key: None,
				};

				// Add preliminary type to context so members can self-reference by name
				let enum_ctx_with_self = enum_ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: preliminary_enum_type.clone(),
						mutable: false,
						visibility: Visibility::Public,
					}),
				);

				// Process enum members (methods, computed properties, etc.)
				let (processed_members, processed_impls, processed_impl_records) =
					self.process_struct_members(members, &preliminary_enum_type, &enum_ctx_with_self)?;

				let enum_type = Type::Enum {
					name: name.0.clone(),
					generics: Arc::new(generic_params),
					type_args: Vec::new(),
					variants: Arc::new(variants_map),
					members: Arc::new(processed_members),
					impls: Arc::new(processed_impls),
					def_key: None,
				};

				let mut next_ctx = ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: enum_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				);
				for record in processed_impl_records {
					next_ctx = next_ctx.with_impl_record(record);
				}
				Ok(next_ctx)
			}
			Declaration::Interface {
				visibility: _,
				name,
				generics,
				super_interfaces,
				members,
			} => {
				let (generic_params, generics_ctx) = self.resolve_generic_params(generics, ctx)?;
				let interface_type_args = generic_params
					.iter()
					.map(|param| Type::Variable {
						id: param.id,
						name: param.name.clone(),
						constraint: param.constraint.clone().map(Box::new),
					})
					.collect();

				// Create a self type variable for interface member resolution
				// In interface definitions, `self` is abstract (a type variable with the interface as constraint)
				let self_type = self.fresh_var(
					"self",
					Some(Type::Interface {
						name: name.0.clone(),
						generics: Arc::new(generic_params.clone()),
						type_args: interface_type_args,
						members: Arc::new(BTreeMap::new()),
						impls: Arc::new(BTreeMap::new()),
						def_key: None,
					}),
				);
				let interface_ctx = generics_ctx.with_self_type(self_type);

				// Process interface members with self type in scope
				let (interface_members, implied_interfaces) =
					self.process_interface(&name.0, super_interfaces, members, &interface_ctx)?;

				Ok(ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: Type::Interface {
							name: name.0.clone(),
							generics: Arc::new(generic_params),
							type_args: Vec::new(),
							members: Arc::new(interface_members),
							impls: Arc::new(implied_interfaces),
							def_key: None,
						},
						mutable: false,
						visibility: Visibility::Public,
					}),
				))
			}
			Declaration::ImplFor { .. } | Declaration::Impl { .. } => {
				// Process impl declarations and register implementations
				self.process_impl(declaration, ctx)
			}
			Declaration::Namespace {
				visibility: _,
				name,
				members: _,
			} => {
				// Namespaces are stored as types with static members
				Ok(ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: Type::Module {
							name: name.0.clone(),
							members: Arc::new(BTreeMap::new()),
						},
						mutable: false,
						visibility: Visibility::Public,
					}),
				))
			}
			Declaration::Import { root, path, idents } => {
				self.check_import(root, path, idents.as_ref().map(Vec::as_slice), ctx)
			}
			Declaration::ExternalLet(
				_visibility,
				_external_name,
				LetDeclaration {
					name,
					type_,
					mutable,
				},
			) => {
				let let_type = match type_ {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => {
						return Err(TypeError {
							kind: TypeErrorKind::ExternalDeclarationMissingType,
							span: span_to_range(name.1),
						});
					}
				};

				if let Pattern::Binding {
					name: binding_name, ..
				} = &name.0
				{
					Ok(ctx.with_new_entry(
						binding_name.0.clone(),
						ContextEntry::Value(ContextValue {
							type_: let_type,
							mutable: *mutable,
							visibility: Visibility::Public,
						}),
					))
				} else {
					Ok(ctx.clone())
				}
			}
			Declaration::ExternalFunc(
				_visibility,
				_external_name,
				FuncDeclaration {
					name: Spanned(func_name, _),
					generics,
					params,
					return_type,
				},
			) => {
				let (generic_params, func_ctx) = self.resolve_generic_params(generics, ctx)?;

				// Collect parameter types
				let mut param_types = Vec::new();
				for param in params {
					let param_type = self.resolve_func_param_type(param, &func_ctx)?;
					let param_name = match &param.0.name.0 {
						Pattern::Binding { name, .. } => Some(name.0.clone()),
						Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
							Some(path[0].0.clone())
						}
						_ => None,
					};
					param_types.push((param_name, param_type));
				}

				let return_ty = match return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, &func_ctx)?,
					None => Type::Void,
				};

				let func_type = Type::Function {
					generics: Arc::new(generic_params),
					params: param_types,
					has_spread: params.last().map(|p| p.0.spread).unwrap_or(false),
					return_type: Box::new(return_ty),
					constructor: false,
				};

				Ok(ctx.with_new_entry(
					func_name.clone(),
					ContextEntry::Value(ContextValue {
						type_: func_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				))
			}
			Declaration::TypeAlias {
				visibility: _,
				meta: TypeAliasDeclaration { name, generics },
				value,
			} => {
				let (_generic_params, alias_ctx) = self.resolve_generic_params(generics, ctx)?;

				let aliased_type = self.resolve_ast_type(&value.0, value.1, &alias_ctx)?;

				Ok(ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: aliased_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				))
			}
		}
	}

	/// Check an import declaration and add the imported items to context
	fn check_import(
		&mut self,
		root: &ImportRoot,
		path: &[ast::Ident],
		idents: Option<&[(ast::Ident, Option<ast::Ident>)]>,
		ctx: &Context,
	) -> Result<Context, TypeError> {
		if path.is_empty() {
			return Ok(ctx.clone());
		}

		// Calculate span from path
		let first_span = path.first().map(|i| i.1).unwrap_or(Span::new(0, 0));
		let last_span = path.last().map(|i| i.1).unwrap_or(first_span);
		let import_span = Span::new(first_span.start, last_span.end);

		// Resolve the import path to a file
		let module_file_path = self.resolve_import_path(root, path, import_span)?;

		// Load and type-check the module
		let module_ctx = self.load_module(&module_file_path, import_span, ctx)?;

		// Get the module name (last segment of path)
		let module_name = path.last().map(|s| s.0.clone()).unwrap_or_default();

		// Create the module type from the context
		let module_type = self.context_to_module_type(module_name.clone(), &module_ctx);

		// Start with current context
		let mut new_ctx = self.merge_module_effects(ctx, &module_ctx);

		// Add the module itself to context (for qualified access like `math.cos`)
		new_ctx = new_ctx.with_new_entry(
			module_name.clone(),
			ContextEntry::Value(ContextValue {
				type_: module_type,
				mutable: false,
				visibility: Visibility::Private,
			}),
		);

		// If there's a `with` clause, import those items directly
		if let Some(import_idents) = idents {
			for (Spanned(item_name, item_span), alias) in import_idents {
				// Look up the item in the module's context
				let entry = module_ctx
					.local_ctx
					.get(item_name)
					.ok_or_else(|| TypeError {
						kind: TypeErrorKind::ImportedItemNotFound {
							item: item_name.clone(),
							module: module_name.clone(),
							suggestion: find_similar_name(item_name, module_ctx.local_ctx.keys()),
						},
						span: span_to_range(*item_span),
					})?;

				// Check visibility
				let visibility = match entry {
					ContextEntry::Value(val) => val.visibility,
					ContextEntry::Impl { parent, .. } => parent.visibility,
				};

				if visibility == Visibility::Private {
					return Err(TypeError {
						kind: TypeErrorKind::ImportedItemNotFound {
							item: item_name.clone(),
							module: module_name.clone(),
							suggestion: None,
						},
						span: span_to_range(*item_span),
					});
				}

				// Use alias if provided, otherwise use original name
				let local_name = alias
					.as_ref()
					.map(|Spanned(name, _)| name.clone())
					.unwrap_or_else(|| item_name.clone());

				new_ctx = new_ctx.with_new_entry(local_name, entry.clone());
			}
		}

		Ok(new_ctx)
	}

	/// Check an import declaration using salsa queries for module resolution and type-checking
	#[allow(clippy::too_many_arguments)]
	pub fn check_import_salsa(
		&mut self,
		db: &dyn Db,
		file: SourceFile,
		config: ProjectConfig,
		root: &ImportRoot,
		path: &[ast::Ident],
		idents: Option<&[(ast::Ident, Option<ast::Ident>)]>,
		ctx: &Context,
	) -> Result<Context, TypeError> {
		use crate::db::{ImportSpec, ImportedIdent};

		if path.is_empty() {
			return Ok(ctx.clone());
		}

		let first_span = path.first().map(|i| i.1).unwrap_or(Span::new(0, 0));
		let last_span = path.last().map(|i| i.1).unwrap_or(first_span);
		let import_span = Span::new(first_span.start, last_span.end);

		let path_strings: Vec<String> = path.iter().map(|seg| seg.0.to_string()).collect();

		let imported_idents = idents.map(|ids| {
			ids
				.iter()
				.map(|(name, alias)| ImportedIdent {
					name: name.0.to_string(),
					alias: alias.as_ref().map(|a| a.0.to_string()),
					span: name.1,
				})
				.collect()
		});

		let import_spec = ImportSpec {
			root: root.clone(),
			path: path_strings,
			idents: imported_idents,
			span: import_span,
		};

		let resolved_path = queries::resolve_import(db, file, config, import_spec);

		let Some(module_file_path) = resolved_path else {
			return Ok(ctx.clone());
		};

		let imported_file =
			queries::load_source_file(db, module_file_path.to_string_lossy().to_string());
		let module_ctx = self.check_file_salsa(db, imported_file, config);

		let module_name = path.last().map(|s| s.0.clone()).unwrap_or_default();
		let module_type = self.context_to_module_type(module_name.clone(), &module_ctx);

		let mut new_ctx = self.merge_module_effects(ctx, &module_ctx);

		new_ctx = new_ctx.with_new_entry(
			module_name.clone(),
			ContextEntry::Value(ContextValue {
				type_: module_type,
				mutable: false,
				visibility: Visibility::Private,
			}),
		);

		if let Some(import_idents) = idents {
			for (Spanned(item_name, item_span), alias) in import_idents {
				let entry = module_ctx
					.local_ctx
					.get(item_name)
					.ok_or_else(|| TypeError {
						kind: TypeErrorKind::ImportedItemNotFound {
							item: item_name.clone(),
							module: module_name.clone(),
							suggestion: find_similar_name(item_name, module_ctx.local_ctx.keys()),
						},
						span: span_to_range(*item_span),
					})?;

				let visibility = match entry {
					ContextEntry::Value(val) => val.visibility,
					ContextEntry::Impl { parent, .. } => parent.visibility,
				};

				if visibility == Visibility::Private {
					return Err(TypeError {
						kind: TypeErrorKind::ImportedItemNotFound {
							item: item_name.clone(),
							module: module_name.clone(),
							suggestion: None,
						},
						span: span_to_range(*item_span),
					});
				}

				let local_name = alias
					.as_ref()
					.map(|Spanned(name, _)| name.clone())
					.unwrap_or_else(|| item_name.clone());

				new_ctx = new_ctx.with_new_entry(local_name, entry.clone());
			}
		}

		Ok(new_ctx)
	}
}

pub fn type_check(module: &Module) -> Result<(), TypeError> {
	let mut checker = TypeChecker::default();
	let ctx = Context::default();
	checker.check_module(module, &ctx)?;
	Ok(())
}

/// Type-check a module with file path context for import resolution
pub fn type_check_with_path(module: &Module, file_path: PathBuf) -> Result<(), TypeError> {
	let mut checker = TypeChecker::new(Some(file_path));
	let ctx = Context::default();
	checker.check_module(module, &ctx)?;
	Ok(())
}

/// Find the closest matching parameter name using Levenshtein distance.
/// Returns `Some(name)` if a sufficiently similar name is found.
fn find_similar_name<'a>(
	name: &str,
	candidates: impl Iterator<Item = &'a EcoString>,
) -> Option<EcoString> {
	let max_dist = match name.len() {
		0..=2 => 0,
		3..=5 => 1,
		_ => 2,
	};

	candidates
		.map(|c| (c, strsim::levenshtein(name, c)))
		.filter(|(_, d)| *d <= max_dist)
		.min_by_key(|(_, d)| *d)
		.map(|(c, _)| c.clone())
}

/// Convert a TypeError to an ariadne Report for display
pub fn type_error_to_report(
	filename: EcoString,
	error: &TypeError,
) -> ariadne::Report<'_, (EcoString, std::ops::Range<usize>)> {
	use ariadne::{Color, Label, Report, ReportKind};

	let error_file = error.file_path().unwrap_or(filename);
	let span = error.span.clone();
	let mut report = Report::build(ReportKind::Error, (error_file.clone(), span.clone()))
		.with_config(ariadne::Config::new().with_tab_width(2))
		.with_message(error.to_string())
		.with_label(
			Label::new((error_file, span))
				.with_message(error)
				.with_color(Color::Red),
		);

	let suggestion = match &error.kind {
		TypeErrorKind::UnknownIdentifier { suggestion, .. }
		| TypeErrorKind::UnknownType { suggestion, .. }
		| TypeErrorKind::UnknownMember { suggestion, .. }
		| TypeErrorKind::UnknownNamedArgument { suggestion, .. }
		| TypeErrorKind::ImportedItemNotFound { suggestion, .. } => suggestion.as_ref(),
		_ => None,
	};
	let anonymous_function_help = match &error.kind {
		TypeErrorKind::CannotInferAnonymousFunction { placeholders } => Some(format!(
			"anonymous placeholders like {} need an expected function type; add one or rewrite the expression as an explicit closure such as {}",
			format_anonymous_placeholders(placeholders),
			closure_help_example(anonymous_placeholder_arity(placeholders)),
		)),
		_ => None,
	};

	if let Some(suggestion) = suggestion {
		report = report.with_help(format!("did you mean '{suggestion}'?"));
	} else if let Some(help) = anonymous_function_help {
		report = report.with_help(help);
	}

	report.finish()
}

fn format_anonymous_placeholders(placeholders: &[Option<u32>]) -> String {
	placeholders
		.iter()
		.map(|index| match index {
			None => "$".to_string(),
			Some(index) => format!("${index}"),
		})
		.collect::<Vec<_>>()
		.join(", ")
}

fn anonymous_placeholder_arity(placeholders: &[Option<u32>]) -> usize {
	placeholders
		.iter()
		.map(|index| index.unwrap_or(0) as usize)
		.max()
		.map_or(1, |index| index + 1)
}

fn closure_help_example(arity: usize) -> String {
	let params = (0..arity.max(1))
		.map(|index| format!("arg{index}: int"))
		.collect::<Vec<_>>()
		.join(", ");
	format!("`({params}) -> ...`")
}
