//! Item-level name resolution and the lowered signatures of top-level definitions.
//!
//! [`build_def_map`] is the *separate* resolution pass the old checker lacked: a
//! single walk over a module that assigns every top-level definition (and every enum
//! variant) a [`DefId`] and records where it came from. Type checking consumes the
//! resulting [`DefMap`] and never re-resolves top-level names inline.
//!
//! The lowered [`Signatures`] (field/parameter/return types expressed as semantic
//! [`Ty`]s) are computed once in `lower.rs` and are the global data that body
//! inference reads but never mutates — the incrementality boundary from the plan.

use crate::errors::TypeError;
use ecow::EcoString;
use nymph_ast::{
	Ident, Span, Spanned,
	decl::{Declaration, Module},
	expr::Pattern,
};
use nymph_diagnostics::{Diagnostic, IntoDiagnostic};
use rustc_hash::FxHashMap;

use crate::{DefId, DefinitionId, ModuleIdentity, Ty};

/// The resolved top-level items of a module.
#[derive(Debug, Default, Clone)]
pub struct DefMap {
	pub defs: Vec<DefData>,
	by_stable: FxHashMap<DefinitionId, DefId>,
	/// Top-level names (types, functions, lets, namespaces) in a single value/type
	/// namespace. Enum variants are *not* here — they live in [`DefMap::variants`], so
	/// two enums may share a variant name and a struct may share a name with a variant.
	pub by_name: FxHashMap<EcoString, DefId>,
	/// Enum variants by bare name; a name maps to every variant declared with it (across
	/// enums). A bare use is resolved against this and is ambiguous only if more than one
	/// candidate exists — a qualified `Enum.Variant` always disambiguates.
	pub variants: FxHashMap<EcoString, Vec<DefId>>,
}

#[derive(Debug, Clone)]
pub struct DefData {
	pub name: EcoString,
	/// Checker-compatibility spelling used only by diagnostics which historically
	/// exposed rewritten project symbols. Semantic identity and lookup always use
	/// [`Self::name`] and [`Self::stable`].
	pub diagnostic_display_name: Option<EcoString>,
	/// The defining occurrence's span. Reserved for go-to-definition (LSP) and
	/// richer diagnostics; not read by Milestone-A checking itself.
	#[allow(dead_code)]
	pub span: Span,
	pub kind: DefKind,
	pub origin: DefOrigin,
	pub stable: Option<DefinitionId>,
}

/// Where a definition entered this semantic map. Only local definitions may be used
/// to schedule jobs that read the current module's AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefOrigin {
	Local { member: usize },
	Imported { module: ModuleIdentity },
}

/// What a [`DefId`] refers to, independent of source-AST provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
	Func,
	Struct,
	Enum,
	/// An enum variant, referenced by bare name as a constructor/pattern.
	Variant {
		enum_def: DefId,
		variant: usize,
	},
	Let,
	TypeAlias,
	Namespace,
	Interface,
}

impl DefMap {
	pub(crate) fn clear_lexical_imports(&mut self) {
		self.by_name.clear();
		self.variants.clear();
	}

	pub(crate) fn expose_name(&mut self, name: EcoString, id: DefId) {
		self.by_name.insert(name, id);
	}

	pub fn get(&self, name: &str) -> Option<DefId> {
		self.by_name.get(name).copied()
	}

	pub fn data(&self, def: DefId) -> &DefData {
		&self.defs[def.0 as usize]
	}

	pub fn diagnostic_name(&self, def: DefId) -> &EcoString {
		let data = self.data(def);
		data.diagnostic_display_name.as_ref().unwrap_or(&data.name)
	}

	pub(crate) fn set_imported_diagnostic_module_tags(
		&mut self,
		tags: &FxHashMap<ModuleIdentity, usize>,
	) {
		for data in &mut self.defs {
			if let DefOrigin::Imported { module } = &data.origin
				&& let Some(tag) = tags.get(module)
			{
				data.diagnostic_display_name = Some(format!("$m{tag}${}", data.name).into());
			}
		}
	}

	pub fn local_member(&self, def: DefId) -> Option<usize> {
		match self.data(def).origin {
			DefOrigin::Local { member } => Some(member),
			DefOrigin::Imported { .. } => None,
		}
	}

	pub fn is_local(&self, def: DefId) -> bool {
		self.local_member(def).is_some()
	}

	pub fn stable(&self, def: DefId) -> Option<&DefinitionId> {
		self.data(def).stable.as_ref()
	}

	pub fn by_stable(&self, stable: &DefinitionId) -> Option<DefId> {
		self.by_stable.get(stable).copied()
	}

	fn define(
		&mut self,
		name: EcoString,
		span: Span,
		kind: DefKind,
		origin: DefOrigin,
		stable: Option<DefinitionId>,
	) -> DefId {
		let id = DefId(self.defs.len() as u32);
		if let Some(stable) = &stable {
			self.by_stable.insert(stable.clone(), id);
		}
		self.defs.push(DefData {
			name: name.clone(),
			diagnostic_display_name: None,
			span,
			kind,
			origin,
			stable,
		});
		self.by_name.insert(name, id);
		id
	}

	pub fn define_imported(
		&mut self,
		name: EcoString,
		kind: DefKind,
		module: ModuleIdentity,
		stable: Option<DefinitionId>,
	) -> DefId {
		self.define(
			name,
			Span::new(0, 0),
			kind,
			DefOrigin::Imported { module },
			stable,
		)
	}

	/// Allocates an imported stable definition once, optionally exposing this occurrence's
	/// name to compatibility bare-name lookup. Repeated stable IDs reuse their original
	/// checker-local ID while each exported spelling participates in later-wins lookup.
	pub(crate) fn allocate_imported(
		&mut self,
		name: EcoString,
		kind: DefKind,
		module: ModuleIdentity,
		stable: DefinitionId,
		bare_visible: bool,
	) -> DefId {
		if let Some(id) = self.by_stable(&stable) {
			if bare_visible {
				self.by_name.insert(name, id);
			}
			return id;
		}
		let id = DefId(self.defs.len() as u32);
		self.by_stable.insert(stable.clone(), id);
		self.defs.push(DefData {
			name: name.clone(),
			diagnostic_display_name: None,
			span: Span::new(0, 0),
			kind,
			origin: DefOrigin::Imported { module },
			stable: Some(stable),
		});
		if bare_visible {
			self.by_name.insert(name, id);
		}
		id
	}

	pub(crate) fn expose_imported_variant(&mut self, name: EcoString, id: DefId) {
		let candidates = self.variants.entry(name).or_default();
		if !candidates.contains(&id) {
			candidates.push(id);
		}
	}

	/// Define an enum variant: it gets a [`DefData`] and joins the bare-name → variants
	/// multimap, but never the single `by_name` namespace (so variant names don't clash).
	fn define_variant(
		&mut self,
		name: EcoString,
		span: Span,
		kind: DefKind,
		member: usize,
		stable: Option<DefinitionId>,
	) -> DefId {
		let id = DefId(self.defs.len() as u32);
		if let Some(stable) = &stable {
			self.by_stable.insert(stable.clone(), id);
		}
		self.defs.push(DefData {
			name: name.clone(),
			diagnostic_display_name: None,
			span,
			kind,
			origin: DefOrigin::Local { member },
			stable,
		});
		self.variants.entry(name).or_default().push(id);
		id
	}

	/// Resolve a bare variant name: `None` if unknown, `Some(Ok)` if a single variant
	/// matches, `Some(Err)` if several do (ambiguous — needs a qualified `Enum.Variant`).
	pub fn resolve_variant(&self, name: &str) -> Option<Result<(DefId, usize), ()>> {
		let ids = self.variants.get(name)?;
		match ids.as_slice() {
			[] => None,
			[id] => match self.data(*id).kind {
				DefKind::Variant { enum_def, variant } => Some(Ok((enum_def, variant))),
				_ => None,
			},
			_ => Some(Err(())),
		}
	}
}

/// The bound name of a plain binding pattern (a top-level `let x = …`).
fn binding_name(pattern: &Spanned<Pattern>) -> Option<&Ident> {
	match &pattern.0 {
		Pattern::Binding { name, .. } => Some(name),
		_ => None,
	}
}

/// Walk a module's members and assign a [`DefId`] to every top-level definition and
/// enum variant. Duplicate names are reported and the later definition wins.
pub fn build_def_map(module: &Module, diags: &mut Vec<Diagnostic>) -> DefMap {
	build_def_map_on(module, DefMap::default(), diags, None)
}

pub(crate) fn build_def_map_on(
	module: &Module,
	mut map: DefMap,
	diags: &mut Vec<Diagnostic>,
	headers: Option<&crate::DeclaredHeaders>,
) -> DefMap {
	let mut seen: FxHashMap<EcoString, Span> = FxHashMap::default();

	let mut declare = |map: &mut DefMap,
	                   diags: &mut Vec<Diagnostic>,
	                   name: &Ident,
	                   kind: DefKind,
	                   member: usize|
	 -> DefId {
		if let Some(&prev) = seen.get(&name.0) {
			diags.push(
				TypeError::Redefinition {
					name: name.0.clone(),
					redefined_span: name.1,
					prev,
				}
				.as_diagnostic(name.1),
			);
		}
		seen.insert(name.0.clone(), name.1);
		let stable = headers.and_then(|headers| headers.member_id(member));
		map.define(
			name.0.clone(),
			name.1,
			kind,
			DefOrigin::Local { member },
			stable,
		)
	};

	for (i, decl) in module.members.iter().enumerate() {
		match decl {
			Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => {
				declare(&mut map, diags, &meta.name, DefKind::Func, i);
			}
			Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => {
				if let Some(name) = binding_name(&meta.name) {
					declare(&mut map, diags, name, DefKind::Let, i);
				}
			}
			Declaration::Struct { name, .. } => {
				declare(&mut map, diags, name, DefKind::Struct, i);
			}
			Declaration::Enum { name, variants, .. } => {
				let enum_def = declare(&mut map, diags, name, DefKind::Enum, i);
				for (v, variant) in variants.iter().enumerate() {
					let stable = map.stable(enum_def).cloned().map(|owner| {
						DefinitionId::new(
							owner.module.clone(),
							crate::DeclarationKey::member(
								owner,
								crate::DeclarationCategory::Variant,
								variant.0.name.0.clone(),
							),
						)
					});
					// Variants share a separate namespace, so duplicates across enums are
					// fine and are never reported as redefinitions.
					map.define_variant(
						variant.0.name.0.clone(),
						variant.0.name.1,
						DefKind::Variant {
							enum_def,
							variant: v,
						},
						i,
						stable,
					);
				}
			}
			Declaration::TypeAlias { meta, .. } => {
				declare(&mut map, diags, &meta.name, DefKind::TypeAlias, i);
			}
			Declaration::Namespace { name, .. } => {
				declare(&mut map, diags, name, DefKind::Namespace, i);
			}
			Declaration::Interface { name, .. } => {
				declare(&mut map, diags, name, DefKind::Interface, i);
			}
			// Imports introduce no local name here; impl blocks are anonymous (their
			// contents are collected separately in `iface.rs`).
			Declaration::Import { .. } | Declaration::Impl { .. } | Declaration::ImplFor { .. } => {}
		}
	}

	map
}

// ── Lowered signatures ───────────────────────────────────────────────────────

/// A generic parameter list, storing just the parameter names; the `i`-th name
/// corresponds to `ParamIdx(i)` in the lowered types.
pub type Generics = Vec<EcoString>;

#[derive(Debug, Clone)]
pub struct StructSig {
	pub generics: Generics,
	pub fields: Vec<(EcoString, Ty)>,
	pub field_metadata: Vec<FieldSigMetadata>,
	/// The interface bounds declared on this struct's own generics (Slice 4G-b),
	/// e.g. `struct Range<Idx: Comparable<Idx>>` — one [`crate::iface::Bound`] per
	/// bound, with `ty = Param(i)` in this signature's own `0..generics.len()` index
	/// space (the same space `instantiate_struct` mints into), so a
	/// construction site can substitute them exactly like `fields`.
	pub bounds: Vec<crate::iface::Bound>,
}

#[derive(Debug, Clone)]
pub struct EnumSig {
	pub generics: Generics,
	pub variants: Vec<VariantSig>,
	/// The interface bounds declared on this enum's own generics (Slice 4G-b), same
	/// index space as [`StructSig::bounds`] (matching `instantiate_enum`).
	pub bounds: Vec<crate::iface::Bound>,
}

#[derive(Debug, Clone)]
pub struct VariantSig {
	pub target: Option<DefinitionId>,
	pub name: EcoString,
	pub fields: Vec<(EcoString, Ty)>,
	pub field_metadata: Vec<FieldSigMetadata>,
}

#[derive(Debug, Clone)]
pub struct FieldSigMetadata {
	pub target: Option<DefinitionId>,
	pub mutable: bool,
	pub has_default: bool,
}

#[derive(Debug, Clone)]
pub struct OwnedMemberSig {
	pub target: DefinitionId,
	pub kind: crate::MemberKind,
	pub generics: Generics,
	pub bounds: Vec<crate::iface::Bound>,
	pub params: Vec<FuncParamSig>,
	pub ret: Ty,
	pub has_default: bool,
	pub external: Option<crate::ExternalAbi>,
}

#[derive(Debug, Clone)]
pub struct FuncParamSig {
	/// The parameter's binding name, used for named-argument calls (Milestone B).
	#[allow(dead_code)]
	pub label: Option<EcoString>,
	pub ty: Ty,
	/// A `...rest` spread parameter (Milestone B).
	#[allow(dead_code)]
	pub spread: bool,
}

#[derive(Debug, Clone)]
pub struct FuncSig {
	pub generics: Generics,
	pub params: Vec<FuncParamSig>,
	pub ret: Ty,
	/// Whether the function has a `this` receiver (an inherent method). Always
	/// `false` in Milestone A, which has no method definitions.
	#[allow(dead_code)]
	pub has_self: bool,
	/// The interface bounds declared on this function's own generics (Slice 4G),
	/// e.g. `T: Area` or `T: Comparable<Other = T>` — one [`crate::iface::Bound`]
	/// per bound, with `ty = Param(i)` in this signature's own `0..generics.len()`
	/// index space (the same space the scheme instantiator mints into), so a call site can
	/// substitute them exactly like `params`/`ret`. Read by `fn_type_of` to defer a
	/// call-site obligation per bound per instantiation.
	pub bounds: Vec<crate::iface::Bound>,
}

/// An owned type-alias declaration. `target` uses this alias's generic parameter
/// index space and can therefore be instantiated without retaining its AST.
#[derive(Debug, Clone)]
pub struct AliasSig {
	pub generics: Generics,
	pub target: Ty,
	#[allow(dead_code)]
	pub bounds: Vec<crate::iface::Bound>,
}

/// An owned top-level value signature. Keeping binders and bounds here makes
/// generalized values importable without collapsing their parameter space.
#[derive(Debug, Clone)]
pub struct ValueSig {
	pub generics: Generics,
	pub ty: Ty,
	pub bounds: Vec<crate::iface::Bound>,
}

/// An owned member of a top-level `namespace` declaration.
#[derive(Debug, Clone)]
pub enum NamespaceMemberSig {
	Func {
		#[allow(dead_code)]
		target: Option<DefinitionId>,
		sig: FuncSig,
	},
	Value {
		#[allow(dead_code)]
		target: Option<DefinitionId>,
		ty: Ty,
		#[allow(dead_code)]
		mutable: bool,
	},
}

#[derive(Debug, Default, Clone)]
pub struct NamespaceSig {
	pub members: FxHashMap<EcoString, NamespaceMemberSig>,
}

/// The lowered signatures of every top-level definition. Built once, read-only
/// during body inference. Alias and namespace declarations are owned here too:
/// declaration AST is only consulted while collecting local signatures.
#[derive(Debug, Default, Clone)]
pub struct Signatures {
	pub structs: FxHashMap<DefId, StructSig>,
	pub enums: FxHashMap<DefId, EnumSig>,
	pub funcs: FxHashMap<DefId, FuncSig>,
	pub lets: FxHashMap<DefId, ValueSig>,
	pub aliases: FxHashMap<DefId, AliasSig>,
	pub namespaces: FxHashMap<DefId, NamespaceSig>,
}
