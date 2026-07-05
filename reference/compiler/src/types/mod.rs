pub mod error;

#[cfg(test)]
mod tests;

use std::{
	collections::{BTreeMap, HashMap, HashSet},
	fmt::Display,
	fs,
	hash::Hash,
	path::{Path, PathBuf},
	sync::Arc,
};

use ecow::EcoString;
use itertools::Itertools;

use crate::ast::Span;
use crate::{
	ast::{
		self, Spanned,
		declaration::{
			Declaration, FuncDeclaration, ImplMember, ImportRoot, InterfaceElement, InterfaceMember,
			LetDeclaration, Module, StructInnerMember, TypeAliasDeclaration, Visibility,
		},
		expr::{Expr, ListItem, Pattern, Statement},
		ops::{BinaryOperator, PrefixOperator, TypeOperator},
	},
	db::{Db, DefKey, DiagnosticKind, Diagnostics, NymphDatabase, ProjectConfig, SourceFile},
	queries,
	types::error::TypeError,
};

/// Extract a `Range<usize>` from a `Span`
fn span_to_range(span: Span) -> std::ops::Range<usize> {
	span.start..span.end
}

/// Unique identifier for type variables, to distinguish variables with the same name in different scopes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeVarId(u64);

/// Result type for processing struct/enum members
type ProcessMembersResult = (BTreeMap<EcoString, StructMember>, BTreeMap<EcoString, Type>);

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructMemberKind {
	Namespace,
	Mutable,
	Immutable,
}

impl Type {
	fn assignable_to(&self, target: &Self, _ctx: &Context) -> bool {
		match (self, target) {
			(a, b) if a == b => true,
			(Type::Never, _) => true,
			(_, Type::Never) => false,
			(_, Type::Variable { .. }) | (Type::Variable { .. }, _) => true,
			(Type::Intersection { first, second }, target) => {
				first.assignable_to(target, _ctx) && second.assignable_to(target, _ctx)
			}
			(source, Type::Intersection { first, second }) => {
				source.assignable_to(first, _ctx) || source.assignable_to(second, _ctx)
			}
			(Type::List { item: item_a }, Type::List { item: item_b }) => {
				item_a.assignable_to(item_b, _ctx)
			}
			(Type::Tuple { items: items_a }, Type::Tuple { items: items_b }) => {
				items_a.len() == items_b.len()
					&& items_a
						.iter()
						.zip(items_b)
						.all(|(a, b)| a.assignable_to(b, _ctx))
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
			) => key_a.assignable_to(key_b, _ctx) && value_a.assignable_to(value_b, _ctx),
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
				return_a.assignable_to(return_b, _ctx)
					&& params_a.len() == params_b.len()
					&& params_a
						.iter()
						.zip(params_b)
						.all(|((_, a), (_, b))| b.assignable_to(a, _ctx))
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
				_ => Type::Intersection {
					first: Box::new(self.clone()),
					second: Box::new(other.clone()),
				},
			}
		}
	}

	#[allow(dead_code)]
	fn meet(&self, other: &Self) -> Option<Self> {
		if self == other {
			return Some(self.clone());
		}
		match (self, other) {
			(Type::Never, t) | (t, Type::Never) => Some(t.clone()),
			(Type::Intersection { first, second }, t) => first.meet(t).or_else(|| second.meet(t)),
			(t, Type::Intersection { first, second }) => t.meet(first).or_else(|| t.meet(second)),
			_ => None,
		}
	}
}

impl Display for Type {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Type::Int => write!(f, "int"),
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
	/// Maps type names to their implementations (list of interface types)
	pub impls: HashMap<EcoString, Vec<Type>>,
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

	/// Register that a type implements an interface
	pub fn with_impl(&self, type_name: EcoString, interface_type: Type) -> Self {
		let mut new_ctx = self.clone();
		new_ctx
			.impls
			.entry(type_name)
			.or_default()
			.push(interface_type);
		new_ctx
	}

	/// Get all interfaces implemented by a type
	pub fn get_impls(&self, type_name: &EcoString) -> Option<&Vec<Type>> {
		self.impls.get(type_name)
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
	/// Module is currently being processed (for cycle detection)
	InProgress,
	/// Module has been fully processed
	Complete(Context),
}

#[derive(Clone, Debug, Default)]
pub struct TypeChecker {
	/// Counter for generating unique type variable IDs
	pub next_type_var_id: u64,
	/// Cache of processed modules (absolute path -> status) — used in non-salsa mode
	module_cache: HashMap<PathBuf, ModuleStatus>,
	/// The project root directory (where nymph.toml is located)
	project_root: Option<PathBuf>,
	/// The current file being processed (for resolving relative imports)
	current_file: Option<PathBuf>,
}

impl TypeChecker {
	/// Create a new TypeChecker with the given file path (non-salsa mode)
	pub fn new(file_path: Option<PathBuf>) -> Self {
		let project_root = file_path.as_ref().and_then(|p| Self::find_project_root(p));
		Self {
			next_type_var_id: 0,
			module_cache: HashMap::new(),
			project_root,
			current_file: file_path,
		}
	}

	/// Create a new TypeChecker for use with salsa queries
	pub fn with_salsa(file_path: PathBuf, project_root: PathBuf, next_type_var_id: u64) -> Self {
		Self {
			next_type_var_id,
			module_cache: HashMap::new(),
			project_root: Some(project_root),
			current_file: Some(file_path),
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

	/// Resolve an import path to an absolute file path
	pub fn resolve_import_path(
		&self,
		root: &ImportRoot,
		path: &[ast::Ident],
		span: Span,
	) -> Result<PathBuf, TypeError> {
		let base_dir = match root {
			ImportRoot::Package(Spanned(pkg_name, pkg_span)) => {
				return Err(TypeError::ExternalDependencyNotSupported {
					package: pkg_name.clone(),
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
					TypeError::ProjectRootNotFound {
						searched_from: searched_from.into(),
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
				.ok_or_else(|| TypeError::ProjectRootNotFound {
					searched_from: "<unknown>".into(),
					span: span_to_range(span),
				})?,
			ImportRoot::Parent => self
				.current_file
				.as_ref()
				.and_then(|p| p.parent())
				.and_then(|p| p.parent())
				.map(|p| p.to_path_buf())
				.ok_or_else(|| TypeError::ProjectRootNotFound {
					searched_from: "<unknown>".into(),
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
			(true, true) => Err(TypeError::AmbiguousModule {
				path: path.iter().map(|s| s.0.as_str()).join("/").into(),
				file_path: file_path.display().to_string().into(),
				dir_path: dir_path.display().to_string().into(),
				span: span_to_range(span),
			}),
			(true, false) => Ok(file_path),
			(false, true) => Ok(dir_path),
			(false, false) => Err(TypeError::ModuleNotFound {
				path: path.iter().map(|s| s.0.as_str()).join("/").into(),
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
		let abs_path = module_path
			.canonicalize()
			.map_err(|_| TypeError::ModuleNotFound {
				path: module_path.display().to_string().into(),
				span: span_to_range(span),
			})?;

		// Check cache
		if let Some(status) = self.module_cache.get(&abs_path) {
			return match status {
				ModuleStatus::InProgress => {
					// Circular import - return empty context for now
					// The module will be available after the cycle completes
					Ok(Context::default())
				}
				ModuleStatus::Complete(ctx) => Ok(ctx.clone()),
			};
		}

		// Mark as in-progress for cycle detection
		self
			.module_cache
			.insert(abs_path.clone(), ModuleStatus::InProgress);

		// Read and parse the module
		let source = fs::read_to_string(&abs_path).map_err(|_| TypeError::ModuleNotFound {
			path: abs_path.display().to_string().into(),
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
			return Err(TypeError::ModuleParseError {
				module_path: filename,
				message: diag.message.clone().into(),
				span: diag.span.start..diag.span.end,
			});
		}

		let module = result.module.ok_or_else(|| TypeError::ModuleParseError {
			module_path: filename,
			message: "Failed to parse module".into(),
			span: 0..0,
		})?;

		// Save current file and set new one
		let prev_file = self.current_file.take();
		self.current_file = Some(abs_path.clone());

		// Type-check the module
		let module_ctx =
			self
				.check_module(module.inner(), base_ctx)
				.map_err(|e| TypeError::ModuleTypeError {
					module_path: abs_path.display().to_string().into(),
					error: Box::new(e),
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
		self.infer_expr(&expr.0, expr.1, ctx)
	}

	fn infer_expr(&mut self, expr: &Expr, span: Span, ctx: &Context) -> Result<Type, TypeError> {
		match expr {
			Expr::Int(_) => Ok(Type::Int),
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
					.ok_or_else(|| TypeError::UnknownIdentifier {
						name: name.clone(),
						suggestion: find_similar_name(name, ctx.local_ctx.keys()),
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
						ListItem::Spread(_) => Err(TypeError::SpreadNonFinalParam(span_to_range(item.1))),
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
			Expr::Range(_) => Ok(Type::List {
				item: Box::new(Type::Int),
			}),
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
										return Err(TypeError::UnknownNamedArgument {
											name: name.clone(),
											suggestion,
											span: span_to_range(name_ident.1),
										});
									}
								}
							}
							let min_args = std::cmp::min(args.len(), params.len());
							for i in 0..min_args {
								let arg_type = self.infer(&args[i].0.value, ctx)?;
								let (_, param_type) = &params[i];
								if !arg_type.assignable_to(param_type, ctx) {
									return Err(TypeError::TypeMismatch {
										expected: Box::new(param_type.clone()),
										found: Box::new(arg_type),
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
					other => Err(TypeError::NotCallable(other.into(), span_to_range(span))),
				}
			}
			Expr::MemberAccess {
				parent,
				member,
				optional,
			} => {
				let parent_type = self.infer(parent, ctx)?;
				let resolved = self.access_member(&parent_type, member)?;
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
					_ => Err(TypeError::NotIndexable(span_to_range(span))),
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
					Some(t) => self.resolve_ast_type(&t.0, t.1, &closure_ctx)?,
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
				let rhs_type = self.infer(rhs, ctx)?;
				Self::infer_binary_op(lhs_type, *op, rhs_type, span)
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
				let _rhs_type = self.infer(rhs, ctx)?;
				self.infer(lhs, ctx)
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
				.ok_or(TypeError::ThisOutsideStruct(span_to_range(span))),
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
							let inferred = self.infer(value, &block_ctx)?;
							let final_type = match type_ {
								Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
								None => inferred,
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

	/// Check that an expression has a specific type (bidirectional checking)
	fn check_expr(
		&mut self,
		expr: &Spanned<Expr>,
		expected: &Type,
		ctx: &Context,
	) -> Result<Type, TypeError> {
		let inferred = self.infer(expr, ctx)?;
		if inferred.assignable_to(expected, ctx) {
			Ok(expected.clone())
		} else {
			Err(TypeError::TypeMismatch {
				expected: expected.clone().into(),
				found: inferred.into(),
				span: span_to_range(expr.1),
			})
		}
	}

	fn access_member(&self, ty: &Type, member: &Spanned<EcoString>) -> Result<Type, TypeError> {
		match ty {
			Type::Struct {
				fields,
				members,
				impls,
				..
			} => {
				if let Some(field_type) = fields.get(&member.0) {
					Ok(field_type.clone())
				} else if let Some(member_def) = members.get(&member.0) {
					Ok(member_def.type_.as_ref().clone())
				} else if let Some(impl_member) = self.lookup_impl_member(impls, &member.0) {
					Ok(impl_member)
				} else {
					let candidates = fields.keys().chain(members.keys());
					Err(TypeError::UnknownMember {
						type_: ty.clone().into(),
						member: member.0.clone(),
						suggestion: find_similar_name(&member.0, candidates),
						span: span_to_range(member.1),
					})
				}
			}
			Type::Enum {
				members,
				impls,
				variants,
				generics,
				..
			} => {
				if let Some(member_def) = members.get(&member.0) {
					Ok(member_def.type_.as_ref().clone())
				} else if let Some(impl_member) = self.lookup_impl_member(impls, &member.0) {
					Ok(impl_member)
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
					let candidates = members.keys().chain(variants.keys());
					Err(TypeError::UnknownMember {
						type_: ty.clone().into(),
						member: member.0.clone(),
						suggestion: find_similar_name(&member.0, candidates),
						span: span_to_range(member.1),
					})
				}
			}
			Type::EnumVariant { fields, impls, .. } => {
				if let Some(field_type) = fields.get(&member.0) {
					Ok(field_type.clone())
				} else if let Some(impl_member) = self.lookup_impl_member(impls, &member.0) {
					Ok(impl_member)
				} else {
					Err(TypeError::UnknownMember {
						type_: ty.clone().into(),
						member: member.0.clone(),
						suggestion: find_similar_name(&member.0, fields.keys()),
						span: span_to_range(member.1),
					})
				}
			}
			Type::Interface { members, .. } => {
				if let Some(member_def) = members.get(&member.0) {
					Ok(member_def.type_.as_ref().clone())
				} else {
					Err(TypeError::UnknownMember {
						type_: ty.clone().into(),
						member: member.0.clone(),
						suggestion: find_similar_name(&member.0, members.keys()),
						span: span_to_range(member.1),
					})
				}
			}
			Type::Module { members, .. } => members
				.get(&member.0)
				.map(|entry| match entry {
					ContextEntry::Value(val) => val.type_.clone(),
					ContextEntry::Impl { parent, .. } => parent.type_.clone(),
				})
				.ok_or_else(|| TypeError::UnknownMember {
					type_: ty.clone().into(),
					member: member.0.clone(),
					suggestion: find_similar_name(&member.0, members.keys()),
					span: span_to_range(member.1),
				}),
			Type::Variable {
				constraint: Some(constraint),
				..
			} => self.access_member(constraint, member),
			_ => Err(TypeError::NotAccessible(span_to_range(member.1))),
		}
	}

	fn lookup_impl_member(
		&self,
		impls: &BTreeMap<EcoString, Type>,
		member_name: &EcoString,
	) -> Option<Type> {
		for interface_ty in impls.values() {
			if let Type::Interface { members, .. } = interface_ty
				&& let Some(member_def) = members.get(member_name)
			{
				return Some(member_def.type_.as_ref().clone());
			}
		}
		None
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
			_ => Err(TypeError::InvalidUnaryOp(span_to_range(span))),
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
				_ => Err(TypeError::InvalidBinaryOp(span_to_range(span))),
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
					Err(TypeError::InvalidBinaryOp(span_to_range(span)))
				}
			}
			BinaryOperator::BitAnd | BinaryOperator::BitOr | BinaryOperator::BitXor => {
				if matches!(lhs, Type::Int) && matches!(rhs, Type::Int) {
					Ok(Type::Int)
				} else {
					Err(TypeError::InvalidBinaryOp(span_to_range(span)))
				}
			}
			BinaryOperator::LeftShift | BinaryOperator::RightShift => {
				if matches!(lhs, Type::Int) && matches!(rhs, Type::Int) {
					Ok(Type::Int)
				} else {
					Err(TypeError::InvalidBinaryOp(span_to_range(span)))
				}
			}
			BinaryOperator::In | BinaryOperator::NotIn => match rhs {
				Type::List { .. } | Type::Map { .. } => Ok(Type::Boolean),
				_ => Err(TypeError::InvalidBinaryOp(span_to_range(span))),
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
								Err(TypeError::InvalidBinaryOp(span_to_range(span)))
							}
						} else {
							Err(TypeError::InvalidBinaryOp(span_to_range(span)))
						}
					}
					_ => Err(TypeError::InvalidBinaryOp(span_to_range(span))),
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
			ast::types::Type::Float => Ok(Type::Float),
			ast::types::Type::Char => Ok(Type::Char),
			ast::types::Type::String => Ok(Type::String),
			ast::types::Type::Boolean => Ok(Type::Boolean),
			ast::types::Type::Void => Ok(Type::Void),
			ast::types::Type::Never => Ok(Type::Never),
			ast::types::Type::Self_ => ctx
				.self_type
				.clone()
				.ok_or_else(|| TypeError::SelfTypeInGlobalScope(span_to_range(span))),
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
		let raw_type = ctx
			.lookup_type(name)
			.ok_or_else(|| TypeError::UnknownType {
				name: name.clone(),
				suggestion: find_similar_name(name, ctx.local_ctx.keys()),
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
			let constraint = match &param.0.constraint {
				Some(c) => Some(self.resolve_ast_type(&c.0, c.1, &resolve_ctx)?),
				None => None,
			};
			let default = match &param.0.default {
				Some(d) => Some(self.resolve_ast_type(&d.0, d.1, &resolve_ctx)?),
				None => None,
			};

			let id = self.fresh_type_var_id();
			let gp = GenericParamInfo {
				id,
				name: param.0.name.0.clone(),
				constraint,
				default,
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
			return Err(TypeError::GenericArgumentMismatch {
				expected: 0,
				found: generics.len(),
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
						self.check_constraint_at(&ty, &subst_constraint, span)?;
					}
					subst.insert(param.id, ty);
				}
				None => {
					return Err(TypeError::GenericArgumentMismatch {
						expected: type_params.len(),
						found: generics.len(),
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
						return Err(TypeError::InfiniteTypeInstantiation {
							var: name.clone(),
							ty: Box::new(replacement.clone()),
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
				generics,
				fields,
				members,
				impls,
				..
			} => {
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
								},
							)
						})
					})
					.collect();
				Ok(Type::Struct {
					name: name.clone(),
					generics: generics.clone(),
					type_args: if type_args.is_empty() {
						Vec::new()
					} else {
						type_args
					},
					fields: Arc::new(new_fields?),
					members: Arc::new(new_members?),
					impls: impls.clone(),
					def_key: None,
				})
			}
			Type::Enum {
				name,
				generics,
				variants,
				members,
				impls,
				..
			} => {
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
								},
							)
						})
					})
					.collect();
				Ok(Type::Enum {
					name: name.clone(),
					generics: generics.clone(),
					type_args: if type_args.is_empty() {
						Vec::new()
					} else {
						type_args
					},
					variants: Arc::new(new_variants?),
					members: Arc::new(new_members?),
					impls: impls.clone(),
					def_key: None,
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
					impls: impls.clone(),
				})
			}
			Type::Interface {
				name,
				generics,
				members,
				impls,
				..
			} => {
				let new_members: Result<BTreeMap<_, _>, _> = members
					.iter()
					.map(|(k, m)| {
						self.substitute(&m.type_, subst, span).map(|t| {
							(
								k.clone(),
								StructMember {
									type_: Box::new(t),
									kind: m.kind,
								},
							)
						})
					})
					.collect();
				Ok(Type::Interface {
					name: name.clone(),
					generics: generics.clone(),
					type_args: if type_args.is_empty() {
						Vec::new()
					} else {
						type_args
					},
					members: Arc::new(new_members?),
					impls: impls.clone(),
					def_key: None,
				})
			}
			Type::Int
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

	fn check_constraint_at(&self, ty: &Type, constraint: &Type, span: Span) -> Result<(), TypeError> {
		if ty.assignable_to(constraint, &Context::default()) {
			Ok(())
		} else {
			Err(TypeError::ConstraintViolation {
				type_: ty.clone().into(),
				constraint: constraint.clone().into(),
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
					return Err(TypeError::UnknownNamedArgument {
						name: name.clone(),
						suggestion,
						span: span_to_range(name_ident.1),
					});
				}
			}
		}

		let min_args = std::cmp::min(args.len(), func_params.len());
		for i in 0..min_args {
			let arg_type = self.infer(&args[i].0.value, ctx)?;
			let (_, param_type) = &func_params[i];

			self.unify_for_inference(param_type, &arg_type, &mut subst);
		}

		for param in func_generics {
			if !subst.contains_key(&param.id) {
				if let Some(default) = &param.default {
					let default_resolved = self.substitute(default, &subst, span)?;
					subst.insert(param.id, default_resolved);
				} else {
					return Err(TypeError::GenericArgumentMismatch {
						expected: func_generics.len(),
						found: explicit_generics.len(),
						span: span_to_range(span),
					});
				}
			}
		}

		for param in func_generics {
			if let Some(constraint) = &param.constraint {
				let subst_constraint = self.substitute(constraint, &subst, span)?;
				if let Some(arg_ty) = subst.get(&param.id) {
					self.check_constraint_at(arg_ty, &subst_constraint, span)?;
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
			_ => {}
		}
	}

	/// Check that a type satisfies a constraint (for type variables)
	#[allow(dead_code)]
	fn check_constraint(&self, ty: &Type, constraint: &Type) -> Result<(), TypeError> {
		match (ty, constraint) {
			// A type satisfies itself as a constraint
			(a, b) if a == b => Ok(()),
			// Never satisfies any constraint
			(Type::Never, _) => Ok(()),
			// Intersection: both parts must satisfy
			(Type::Intersection { first, second }, c) => {
				self.check_constraint(first, c)?;
				self.check_constraint(second, c)
			}
			// Otherwise, fail
			_ => Err(TypeError::ConstraintViolation {
				type_: ty.clone().into(),
				constraint: constraint.clone().into(),
				span: 0..0,
			}),
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
			| Pattern::Float(_)
			| Pattern::Char(_)
			| Pattern::String(_)
			| Pattern::Boolean(_)
			| Pattern::Placeholder => Ok(()),
			Pattern::Binding { name, inner } => {
				self.collect_pattern_identifiers(&inner.0, scrutinee.clone(), identifiers)?;
				if identifiers.insert(name.0.clone(), scrutinee).is_some() {
					return Err(TypeError::DuplicatePatternIdentifier {
						pattern: pattern.clone(),
						identifier: name.0.clone(),
						span: 0..0,
					});
				}
				Ok(())
			}
			Pattern::List(items) => {
				let item_type = match scrutinee {
					Type::List { item } => *item,
					_ => {
						return Err(TypeError::PatternTypeMismatch {
							pattern: pattern.clone(),
							scrutinee: scrutinee.into(),
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
								return Err(TypeError::DuplicatePatternIdentifier {
									pattern: pattern.clone(),
									identifier: name.0.clone(),
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
						return Err(TypeError::PatternTypeMismatch {
							pattern: pattern.clone(),
							scrutinee: scrutinee.into(),
							span: 0..0,
						});
					}
				};

				if items.len() > tuple_items.len() {
					return Err(TypeError::TuplePatternTooLong {
						pattern: pattern.clone(),
						tuple_items,
						span: 0..0,
					});
				}

				for (item, ty) in items.iter().zip(tuple_items.iter()) {
					match &item.0 {
						ast::expr::ListPatternEntry::Item(p) => {
							self.collect_pattern_identifiers(&p.0, ty.clone(), identifiers)?;
						}
						ast::expr::ListPatternEntry::Rest(_) => {
							return Err(TypeError::RestPatternNotAtEnd {
								pattern: pattern.clone(),
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
						return Err(TypeError::PatternTypeMismatch {
							pattern: pattern.clone(),
							scrutinee: scrutinee.into(),
							span: 0..0,
						});
					}
				}

				for entry in entries {
					match &entry.0 {
						ast::expr::MapPatternEntry::Entry(key, value) => {
							if !key.0.is_constant() {
								return Err(TypeError::NonConstantMapPatternKey {
									key_pattern: key.0.clone(),
									pattern: pattern.clone(),
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
									return Err(TypeError::DuplicatePatternIdentifier {
										pattern: pattern.clone(),
										identifier: ident.0.clone(),
										span: 0..0,
									});
								}
							}
							ast::expr::StructPatternField::Rest => {
								// `...` - no bindings
							}
						}
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
							return Err(TypeError::ConflictingUnionPatternIdentifiers {
								identifier: name,
								first_type: ty.into(),
								second_type: other_ty.clone().into(),
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
		let mut current_ctx = ctx.clone();

		for decl in &module.members {
			current_ctx = self.check_declaration(decl, &current_ctx)?;
		}

		Ok(current_ctx)
	}

	/// Check a module using salsa queries for import resolution.
	/// Note: `typecheck_file` now uses `context_after` for per-declaration incrementality.
	/// This method is retained for backward compatibility.
	#[allow(dead_code)]
	pub fn check_module_salsa(
		&mut self,
		db: &dyn Db,
		file: SourceFile,
		config: ProjectConfig,
		module: &Module,
	) -> Result<Context, TypeError> {
		let ctx = Context::default();
		let mut current_ctx = ctx;

		for decl in &module.members {
			current_ctx = match decl {
				Declaration::Import { root, path, idents } => {
					self.check_import_salsa(db, file, config, root, path, idents.as_ref(), &current_ctx)?
				}
				_ => self.check_declaration(decl, &current_ctx)?,
			};
		}

		Ok(current_ctx)
	}

	/// Check an interface declaration and extract its members
	fn process_interface(
		&mut self,
		_name: &EcoString,
		members: &[Spanned<InterfaceMember>],
		ctx: &Context,
	) -> Result<BTreeMap<EcoString, StructMember>, TypeError> {
		let mut interface_members = BTreeMap::new();

		// Add `this` to the context for default implementations, typed as the self type
		let this_type = ctx
			.self_type
			.clone()
			.unwrap_or_else(|| self.fresh_var("self", None));
		let member_ctx = ctx.with_new_entry(
			EcoString::from("this"),
			ContextEntry::Value(ContextValue {
				type_: this_type,
				mutable: false,
				visibility: Visibility::Private,
			}),
		);

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
							self.check_impl_member(impl_member, ctx, StructMemberKind::Namespace)?
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
								},
							);
						}
					}
				}
				InterfaceMember::Impl {
					interface: _,
					generics: _,
					members: impl_members,
				} => {
					// Impl blocks inside interface
					for impl_member in impl_members {
						if let Some((name, struct_member)) =
							self.check_impl_member(impl_member, &member_ctx, StructMemberKind::Immutable)?
						{
							interface_members.insert(name, struct_member);
						}
					}
				}
			}
		}

		Ok(interface_members)
	}

	/// Type-check an interface element (let or func with optional default implementation)
	fn check_interface_element(
		&mut self,
		element: &InterfaceElement,
		ctx: &Context,
	) -> Result<Option<(EcoString, StructMember)>, TypeError> {
		match element {
			InterfaceElement::Func { meta, body } => {
				let mut func_ctx = ctx.clone();

				// Add generics to context
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

				// Extract function signature
				let mut param_types = Vec::new();
				for param in &meta.params {
					let param_type = self.resolve_ast_type(&param.0.type_.0, param.0.type_.1, &func_ctx)?;

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

				let return_type = match &meta.return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => Type::Void,
				};

				// Type-check default body if present
				if let Some(body_expr) = body {
					let inferred_return = self.infer(body_expr, &func_ctx)?;
					if !inferred_return.assignable_to(&return_type, &func_ctx) {
						return Err(TypeError::TypeMismatch {
							expected: return_type.clone().into(),
							found: inferred_return.into(),
							span: span_to_range(body_expr.1),
						});
					}
				}

				let func_type = Type::Function {
					generics: Arc::new(Vec::new()),
					params: param_types,
					has_spread: meta.params.last().is_some_and(|p| p.0.spread),
					return_type: Box::new(return_type),
					constructor: false,
				};

				Ok(Some((
					meta.name.0.clone(),
					StructMember {
						type_: Box::new(func_type),
						kind: StructMemberKind::Immutable,
					},
				)))
			}
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

				// Type-check default value if present
				if let Some(val) = value {
					let val_type = self.infer(val, ctx)?;
					if !val_type.assignable_to(&let_type, ctx) {
						return Err(TypeError::TypeMismatch {
							expected: let_type.clone().into(),
							found: val_type.into(),
							span: span_to_range(val.1),
						});
					}
				}

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
					},
				)))
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
						},
					)))
				} else {
					Ok(None)
				}
			}
			ImplMember::ExternalLet(
				_visibility,
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

				let inferred_return = self.infer(body, &func_ctx)?;
				let expected_return = match return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, member_ctx)?,
					None => inferred_return.clone(),
				};

				if !inferred_return.assignable_to(&expected_return, &func_ctx) {
					return Err(TypeError::TypeMismatch {
						expected: expected_return.into(),
						found: inferred_return.into(),
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
					interface: (interface_ident, _generics),
					generics: impl_generics,
					members: impl_members,
				} => {
					let mut impl_ctx = sig_ctx.clone();
					for generic in impl_generics {
						let constraint = match &generic.0.constraint {
							Some(c) => Some(Box::new(self.resolve_ast_type(&c.0, c.1, &sig_ctx)?)),
							None => None,
						};
						let id = self.fresh_type_var_id();
						impl_ctx.insert_entry(
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

					let Spanned(interface_name, interface_span) = interface_ident;
					let interface_ty =
						base_ctx
							.lookup_type(interface_name)
							.ok_or_else(|| TypeError::UnknownType {
								name: interface_name.clone(),
								suggestion: find_similar_name(interface_name, base_ctx.local_ctx.keys()),
								span: span_to_range(*interface_span),
							})?;
					result_impls.insert(interface_name.clone(), interface_ty);

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
							base_ctx: impl_ctx.clone(),
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
					let param_type = self.resolve_ast_type(&param.0.type_.0, param.0.type_.1, &func_ctx)?;
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

		Ok((result_members, result_impls))
	}

	/// Check an impl declaration and register implementations
	/// If `ty` is an enum, inject its variant constructors into `ctx` and return the extended context.
	/// Otherwise, return `ctx` unchanged.
	fn inject_enum_variants(&self, ty: &Type, ctx: &Context) -> Context {
		if let Type::Enum { variants, .. } = ty {
			let mut ctx = ctx.clone();
			for (variant_name, variant_fields) in variants.iter() {
				let mut param_types = Vec::new();
				for (field_name, field_type) in variant_fields {
					param_types.push((Some(field_name.clone()), field_type.clone()));
				}

				let variant_type = Type::Function {
					generics: Arc::new(Vec::new()),
					params: param_types,
					has_spread: false,
					return_type: Box::new(ty.clone()),
					constructor: false,
				};

				ctx.insert_entry(
					variant_name.clone(),
					ContextEntry::Value(ContextValue {
						type_: variant_type,
						mutable: false,
						visibility: Visibility::Private,
					}),
				);
			}
			ctx
		} else {
			ctx.clone()
		}
	}

	fn process_impl(&mut self, impl_decl: &Declaration, ctx: &Context) -> Result<Context, TypeError> {
		match impl_decl {
			Declaration::ImplFor {
				visibility: _,
				generics,
				mutable,
				type_,
				for_interface: (interface_ident, _generics),
				members,
			} => {
				let mut impl_ctx = ctx.clone();
				for generic in generics {
					let constraint = match &generic.0.constraint {
						Some(c) => Some(Box::new(self.resolve_ast_type(&c.0, c.1, ctx)?)),
						None => None,
					};
					let id = self.fresh_type_var_id();
					impl_ctx.insert_entry(
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

				// Resolve the type being implemented
				let ty = self.resolve_ast_type(&type_.0, type_.1, &impl_ctx)?;

				// Resolve the interface
				let Spanned(interface_name, interface_span) = interface_ident;
				let interface_ty =
					impl_ctx
						.lookup_type(interface_name)
						.ok_or_else(|| TypeError::UnknownType {
							name: interface_name.clone(),
							suggestion: find_similar_name(interface_name, impl_ctx.local_ctx.keys()),
							span: span_to_range(*interface_span),
						})?;

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

				for member in members {
					self.check_impl_member(member, &member_ctx, member_kind)?;
				}

				// Register the implementation
				if let Type::Struct { name, .. } | Type::Enum { name, .. } = &ty {
					Ok(ctx.with_impl(name.clone(), interface_ty))
				} else if let Type::Module { name, .. } = &ty {
					Ok(ctx.with_impl(name.clone(), interface_ty))
				} else {
					Ok(ctx.clone())
				}
			}
			Declaration::Impl {
				visibility: _,
				generics,
				mutable,
				type_,
				members,
			} => {
				let mut impl_ctx = ctx.clone();
				for generic in generics {
					let constraint = match &generic.0.constraint {
						Some(c) => Some(Box::new(self.resolve_ast_type(&c.0, c.1, ctx)?)),
						None => None,
					};
					let id = self.fresh_type_var_id();
					impl_ctx.insert_entry(
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

				// Resolve the type being implemented on
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

				for member in members {
					self.check_impl_member(member, &member_ctx, member_kind)?;
				}

				Ok(ctx.clone())
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
				let inferred_type = self.infer(value, ctx)?;
				let final_type = match type_ {
					Some(t) => {
						let expected = self.resolve_ast_type(&t.0, t.1, ctx)?;
						self.check_expr(value, &expected, ctx)?
					}
					None => inferred_type,
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
					let param_type = self.resolve_ast_type(&param.0.type_.0, param.0.type_.1, &func_ctx)?;

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
				let inferred_return = self.infer(body, &func_ctx)?;
				let expected_return = match return_type {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => inferred_return.clone(),
				};

				if !inferred_return.assignable_to(&expected_return, &func_ctx) {
					return Err(TypeError::TypeMismatch {
						expected: expected_return.into(),
						found: inferred_return.into(),
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
					return Err(TypeError::EmptyStruct {
						name: name.0.clone(),
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
							return Err(TypeError::TypeMismatch {
								expected: field_type.into(),
								found: default_type.into(),
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
				let (processed_members, processed_impls) =
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
				let constructor_params: Vec<(Option<EcoString>, Type)> = field_map
					.iter()
					.map(|(field_name, field_type)| (Some(field_name.clone()), field_type.clone()))
					.collect();

				let constructor_type = Type::Function {
					generics: Arc::new(generic_params),
					params: constructor_params,
					has_spread: false,
					return_type: Box::new(struct_type.clone()),
					constructor: true,
				};

				Ok(ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: constructor_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				))
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
								return Err(TypeError::TypeMismatch {
									expected: field_type.into(),
									found: default_type.into(),
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
				let (processed_members, processed_impls) =
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

				Ok(ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: enum_type,
						mutable: false,
						visibility: Visibility::Public,
					}),
				))
			}
			Declaration::Interface {
				visibility: _,
				name,
				generics,
				super_interfaces: _,
				members,
			} => {
				let (generic_params, _generics_ctx) = self.resolve_generic_params(generics, ctx)?;

				// Create a self type variable for interface member resolution
				// In interface definitions, `self` is abstract (a type variable with the interface as constraint)
				let self_type = self.fresh_var(
					"self",
					Some(Type::Interface {
						name: name.0.clone(),
						generics: Arc::new(generic_params.clone()),
						type_args: Vec::new(),
						members: Arc::new(BTreeMap::new()),
						impls: Arc::new(BTreeMap::new()),
						def_key: None,
					}),
				);
				let interface_ctx = ctx.with_self_type(self_type);

				// Process interface members with self type in scope
				let interface_members = self.process_interface(&name.0, members, &interface_ctx)?;

				Ok(ctx.with_new_entry(
					name.0.clone(),
					ContextEntry::Value(ContextValue {
						type_: Type::Interface {
							name: name.0.clone(),
							generics: Arc::new(generic_params),
							type_args: Vec::new(),
							members: Arc::new(interface_members),
							impls: Arc::new(BTreeMap::new()),
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
				self.check_import(root, path, idents.as_ref(), ctx)
			}
			Declaration::ExternalLet(
				_visibility,
				LetDeclaration {
					name,
					type_,
					mutable,
				},
			) => {
				let let_type = match type_ {
					Some(t) => self.resolve_ast_type(&t.0, t.1, ctx)?,
					None => {
						return Err(TypeError::ExternalDeclarationMissingType(span_to_range(
							name.1,
						)));
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
					let param_type = self.resolve_ast_type(&param.0.type_.0, param.0.type_.1, &func_ctx)?;
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
		idents: Option<&Vec<(ast::Ident, Option<ast::Ident>)>>,
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
		let mut new_ctx = ctx.clone();

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
				let entry =
					module_ctx
						.local_ctx
						.get(item_name)
						.ok_or_else(|| TypeError::ImportedItemNotFound {
							item: item_name.clone(),
							module: module_name.clone(),
							suggestion: find_similar_name(item_name, module_ctx.local_ctx.keys()),
							span: span_to_range(*item_span),
						})?;

				// Check visibility
				let visibility = match entry {
					ContextEntry::Value(val) => val.visibility,
					ContextEntry::Impl { parent, .. } => parent.visibility,
				};

				if visibility == Visibility::Private {
					return Err(TypeError::ImportedItemNotFound {
						item: item_name.clone(),
						module: module_name.clone(),
						suggestion: None,
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
		idents: Option<&Vec<(ast::Ident, Option<ast::Ident>)>>,
		ctx: &Context,
	) -> Result<Context, TypeError> {
		use crate::db::{ImportSpec, ImportedIdent};

		if path.is_empty() {
			return Ok(ctx.clone());
		}

		let first_span = path.first().map(|i| i.1).unwrap_or(Span::new(0, 0));
		let last_span = path.last().map(|i| i.1).unwrap_or(first_span);
		let import_span = Span::new(first_span.start, last_span.end);

		let path_strings: Vec<String> = path.iter().map(|seg| seg.inner().to_string()).collect();

		let imported_idents = idents.map(|ids| {
			ids
				.iter()
				.map(|(name, alias)| ImportedIdent {
					name: name.inner().to_string(),
					alias: alias.as_ref().map(|a| a.inner().to_string()),
					span: name.span(),
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
		let result = queries::typecheck_file(db, imported_file, config);
		let module_ctx = result.ctx;

		let module_name = path.last().map(|s| s.0.clone()).unwrap_or_default();
		let module_type = self.context_to_module_type(module_name.clone(), &module_ctx);

		let mut new_ctx = ctx.clone();

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
				let entry =
					module_ctx
						.local_ctx
						.get(item_name)
						.ok_or_else(|| TypeError::ImportedItemNotFound {
							item: item_name.clone(),
							module: module_name.clone(),
							suggestion: find_similar_name(item_name, module_ctx.local_ctx.keys()),
							span: span_to_range(*item_span),
						})?;

				let visibility = match entry {
					ContextEntry::Value(val) => val.visibility,
					ContextEntry::Impl { parent, .. } => parent.visibility,
				};

				if visibility == Visibility::Private {
					return Err(TypeError::ImportedItemNotFound {
						item: item_name.clone(),
						module: module_name.clone(),
						suggestion: None,
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
	let span = error.span();
	let mut report = Report::build(ReportKind::Error, (error_file.clone(), span.clone()))
		.with_config(ariadne::Config::new().with_tab_width(2))
		.with_message(error.to_string())
		.with_label(
			Label::new((error_file, span))
				.with_message(error)
				.with_color(Color::Red),
		);

	let suggestion = match error {
		TypeError::UnknownIdentifier { suggestion, .. }
		| TypeError::UnknownType { suggestion, .. }
		| TypeError::UnknownMember { suggestion, .. }
		| TypeError::UnknownNamedArgument { suggestion, .. }
		| TypeError::ImportedItemNotFound { suggestion, .. } => suggestion.as_ref(),
		_ => None,
	};

	if let Some(suggestion) = suggestion {
		report = report.with_help(format!("did you mean '{suggestion}'?"));
	}

	report.finish()
}
