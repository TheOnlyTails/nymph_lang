//! The semantic type representation the checker reasons about.
//!
//! This is deliberately distinct from the *syntactic* [`nymph_ast::ty::Type`] the
//! parser produces. `lower.rs` bridges the two. Semantic types are **interned**:
//! a [`Ty`] is a cheap `Copy` handle (an index into an [`Interner`]), so equality
//! is an integer comparison and there is no structural-`Hash`-as-map-key trap the
//! old checker fell into. Nominal identity always lives in a [`DefId`], never in
//! structural equality.

pub mod fold;

use ecow::EcoString;
use rustc_hash::FxHashMap;

use crate::ids::{DefId, InferVar, ParamIdx};

/// An interned semantic type: an index into the [`Interner`] that produced it.
///
/// Handles are only meaningful relative to the interner that minted them. Two
/// structurally equal types interned in the same interner are the *same* `Ty`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Ty(u32);

/// The structure of a type. Compound variants hold [`Ty`] handles rather than boxed
/// types, so the whole graph is flat and shared through the interner.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TyKind {
	// ── Primitives ───────────────────────────────────────────────────────────
	Int,
	UInt,
	Float,
	Char,
	String,
	Boolean,
	/// `void`, equivalent to the empty tuple.
	Void,
	/// `never`, the type of a diverging expression; the bottom type.
	Never,
	/// `self`, the type currently being declared or implemented. Resolved to a
	/// concrete type when a signature is instantiated for a specific receiver.
	SelfTy,

	// ── Compound ─────────────────────────────────────────────────────────────
	/// `#[T]`
	List(Ty),
	/// `#(A, B, C)` — fixed-size, heterogeneous.
	Tuple(Vec<Ty>),
	/// `#{K: V}`
	Map(Ty, Ty),
	/// `(A, B) -> C`. Parameter labels do not participate in the type; they are
	/// carried by the call signature, not here.
	Fn {
		params: Vec<Ty>,
		ret: Ty,
	},
	/// A nominal `struct` or `enum` applied to generic arguments.
	Adt(DefId, GenericArgs),
	/// The intersection `A + B` of interface bounds — a *conjunction*: a value of
	/// this type satisfies every listed interface. Treated as logical AND
	/// everywhere; treating it as OR accepts values missing required bounds.
	Intersection(Vec<Ty>),
	/// `mut T` — a mutable *view* of `T`. Compile-time-only: codegen ignores it,
	/// since JS values are already mutable. `mut T` is implicitly assignable to
	/// `T` (one-way; see `subtype`), never the reverse. The field's declared type
	/// is the sole authority for whether that field's slot is mutable.
	Mut(Ty),

	// ── Variables ────────────────────────────────────────────────────────────
	/// A rigid generic parameter (skolem) — may not be unified away.
	Param(ParamIdx),
	/// A flexible inference variable — a hole the unifier may solve.
	Infer(InferVar),

	/// The poison type produced by a type error. Unifies with anything so a single
	/// mistake doesn't cascade; lets checking of the rest of a body continue.
	Error,
}

/// Generic arguments applied to a nominal type or interface reference.
///
/// Nymph unifies ordinary type parameters and associated types into one mechanism:
/// a named generic parameter that may be supplied positionally (`Option<T>`) or by
/// label (`Plus<Other = float, Output = float>`). Both forms are retained so the
/// solver can bind associated outputs by name.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct GenericArgs {
	pub positional: Vec<Ty>,
	/// Label → type, e.g. `Output = …`. Kept sorted by label so structurally equal
	/// argument sets intern to the same type regardless of source order.
	pub named: Vec<(EcoString, Ty)>,
}

impl GenericArgs {
	pub fn none() -> Self {
		Self::default()
	}

	pub fn new(positional: Vec<Ty>, mut named: Vec<(EcoString, Ty)>) -> Self {
		named.sort_by(|a, b| a.0.cmp(&b.0));
		Self { positional, named }
	}

	pub fn is_empty(&self) -> bool {
		self.positional.is_empty() && self.named.is_empty()
	}
}

/// Interns [`TyKind`]s into cheap [`Ty`] handles, de-duplicating structurally equal
/// types. Common primitives are pre-interned so hot paths never hash.
#[derive(Debug, Clone)]
pub struct Interner {
	kinds: Vec<TyKind>,
	dedup: FxHashMap<TyKind, Ty>,
	common: Common,
}

/// Pre-interned handles for the nullary types, so callers avoid a hash lookup.
#[derive(Debug, Clone, Copy)]
struct Common {
	int: Ty,
	uint: Ty,
	float: Ty,
	char: Ty,
	string: Ty,
	boolean: Ty,
	void: Ty,
	never: Ty,
	self_ty: Ty,
	error: Ty,
}

impl Default for Interner {
	fn default() -> Self {
		Self::new()
	}
}

impl Interner {
	pub fn new() -> Self {
		let mut this = Self {
			kinds: Vec::new(),
			dedup: FxHashMap::default(),
			// Temporary; overwritten immediately below once the primitives exist.
			common: Common {
				int: Ty(0),
				uint: Ty(0),
				float: Ty(0),
				char: Ty(0),
				string: Ty(0),
				boolean: Ty(0),
				void: Ty(0),
				never: Ty(0),
				self_ty: Ty(0),
				error: Ty(0),
			},
		};
		this.common = Common {
			int: this.intern(TyKind::Int),
			uint: this.intern(TyKind::UInt),
			float: this.intern(TyKind::Float),
			char: this.intern(TyKind::Char),
			string: this.intern(TyKind::String),
			boolean: this.intern(TyKind::Boolean),
			void: this.intern(TyKind::Void),
			never: this.intern(TyKind::Never),
			self_ty: this.intern(TyKind::SelfTy),
			error: this.intern(TyKind::Error),
		};
		this
	}

	/// Hash-consing (structural interning): intern a kind, returning its handle.
	/// Structurally equal kinds share the same handle (cheap copy/equality checks).
	pub fn intern(&mut self, kind: TyKind) -> Ty {
		if let Some(&ty) = self.dedup.get(&kind) {
			return ty;
		}
		let ty = Ty(self.kinds.len() as u32);
		self.kinds.push(kind.clone());
		self.dedup.insert(kind, ty);
		ty
	}

	/// The structure behind a handle.
	pub fn kind(&self, ty: Ty) -> &TyKind {
		&self.kinds[ty.0 as usize]
	}

	// ── Cached primitives ────────────────────────────────────────────────────
	pub fn int(&self) -> Ty {
		self.common.int
	}
	pub fn uint(&self) -> Ty {
		self.common.uint
	}
	pub fn float(&self) -> Ty {
		self.common.float
	}
	pub fn char(&self) -> Ty {
		self.common.char
	}
	pub fn string(&self) -> Ty {
		self.common.string
	}
	pub fn boolean(&self) -> Ty {
		self.common.boolean
	}
	pub fn void(&self) -> Ty {
		self.common.void
	}
	pub fn never(&self) -> Ty {
		self.common.never
	}
	pub fn self_ty(&self) -> Ty {
		self.common.self_ty
	}
	pub fn error(&self) -> Ty {
		self.common.error
	}

	// ── Compound constructors ────────────────────────────────────────────────
	pub fn mk_list(&mut self, elem: Ty) -> Ty {
		self.intern(TyKind::List(elem))
	}
	pub fn mk_tuple(&mut self, elems: Vec<Ty>) -> Ty {
		self.intern(TyKind::Tuple(elems))
	}
	pub fn mk_map(&mut self, key: Ty, value: Ty) -> Ty {
		self.intern(TyKind::Map(key, value))
	}
	pub fn mk_fn(&mut self, params: Vec<Ty>, ret: Ty) -> Ty {
		self.intern(TyKind::Fn { params, ret })
	}
	pub fn mk_adt(&mut self, def: DefId, args: GenericArgs) -> Ty {
		self.intern(TyKind::Adt(def, args))
	}
	pub fn mk_param(&mut self, idx: ParamIdx) -> Ty {
		self.intern(TyKind::Param(idx))
	}
	pub fn mk_infer(&mut self, var: InferVar) -> Ty {
		self.intern(TyKind::Infer(var))
	}
	/// Idempotent: `mut mut T` collapses to `mut T` (a mutable view of a mutable
	/// view is just a mutable view) so `Mut` never nests. Callers elsewhere in
	/// the checker (e.g. `strip_mut`, one `match` peel) rely on this invariant.
	pub fn mk_mut(&mut self, inner: Ty) -> Ty {
		if matches!(self.kind(inner), TyKind::Mut(_)) {
			return inner;
		}
		self.intern(TyKind::Mut(inner))
	}

	/// Build an intersection, flattening nested intersections and collapsing the
	/// zero/one-element cases. An empty intersection is `void` (the trivial bound).
	pub fn mk_intersection(&mut self, parts: Vec<Ty>) -> Ty {
		let mut flat = Vec::new();
		for part in parts {
			match self.kind(part).clone() {
				TyKind::Intersection(inner) => flat.extend(inner),
				_ => flat.push(part),
			}
		}
		flat.sort();
		flat.dedup();
		match flat.len() {
			0 => self.void(),
			1 => flat[0],
			_ => self.intern(TyKind::Intersection(flat)),
		}
	}
}
