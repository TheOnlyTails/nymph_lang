//! Pattern typing and binding collection.
//!
//! A pattern is checked against an expected scrutinee type: literal patterns unify
//! with it, structural patterns decompose it, and binding patterns introduce
//! locals. `bind_pattern` (for `let`/parameters) and `check_pattern` (for `match`
//! and `is`) differ only in the mutability the introduced bindings get.
//!
//! Exhaustiveness of `match` is checked separately (`exhaustive.rs`), which is what
//! lets codegen assume totality and drop the final arm's test. Checking that union
//! arms bind identical names is still pending.

use nymph_ast::{
	Ident, Span, Spanned,
	expr::{ListPatternEntry, MapPatternEntry, Pattern, StructPatternField},
};

use crate::check::Checker;
use crate::def::DefKind;
use crate::errors::TypeError;
use crate::ids::DefId;
use crate::ty::Ty;

/// What a struct/enum pattern's path resolved to.
enum PatternTarget {
	Struct(DefId),
	Variant(DefId, usize),
}

impl Checker<'_> {
	/// Check a pattern in a `let`/parameter position, introducing bindings with the
	/// given mutability.
	pub(crate) fn bind_pattern(&mut self, pattern: &Spanned<Pattern>, ty: Ty, mutable: bool) {
		self.pattern(pattern, ty, mutable);
	}

	/// Check a pattern in a `match`/`is` position; bindings are immutable.
	pub(crate) fn check_pattern(&mut self, pattern: &Spanned<Pattern>, ty: Ty) {
		self.pattern(pattern, ty, false);
	}

	fn pattern(&mut self, pattern: &Spanned<Pattern>, ty: Ty, mutable: bool) {
		let span = pattern.1;
		// Matching against a scrutinee's SHAPE (a literal's type, a tuple/list/map/
		// struct constructor) must ignore any top-level `mut` the scrutinee's type
		// carries — `mut` is never itself a distinct shape to match against. Kept
		// separate from `ty`: a bare-name binding (`Pattern::Binding`, below) still
		// captures the scrutinee's own (possibly `mut`) type as-is, since field-
		// type-authority (not the scrutinee's mutability) is what governs any
		// FURTHER nested field/element type once this level's `subst`/fresh-var
		// substitution takes over.
		let shape = self.strip_mut(ty);
		match &pattern.0 {
			Pattern::Placeholder => {}
			Pattern::Binding { name, inner } => {
				// A bare name (`None`, `Red`) that names a nullary variant is a constructor
				// pattern, not a new binding — resolution, not capitalisation, decides.
				if matches!(inner.0, Pattern::Placeholder)
					&& let Some(adt) = self.nullary_variant_pattern(&name.0, shape, span)
				{
					self.unify(shape, adt, span);
				} else {
					self.define_local(name.0.clone(), ty, mutable);
					if !matches!(inner.0, Pattern::Placeholder) {
						self.pattern(inner, ty, mutable);
					}
				}
			}
			Pattern::Int(_) => {
				let t = self.interner.int();
				self.unify(shape, t, span);
			}
			Pattern::UInt(_) => {
				let t = self.interner.uint();
				self.unify(shape, t, span);
			}
			Pattern::Float(_) => {
				let t = self.interner.float();
				self.unify(shape, t, span);
			}
			Pattern::Char(_) => {
				let t = self.interner.char();
				self.unify(shape, t, span);
			}
			Pattern::Boolean(_) => {
				let t = self.interner.boolean();
				self.unify(shape, t, span);
			}
			Pattern::String(_) => {
				let t = self.interner.string();
				self.unify(shape, t, span);
			}
			Pattern::Grouped(inner) => self.pattern(inner, ty, mutable),
			Pattern::Union(a, b) => {
				self.pattern(a, ty, mutable);
				self.pattern(b, ty, mutable);
			}
			Pattern::Range(_) => {
				// A range pattern constrains an ordered scrutinee; Milestone A leaves
				// the type as-is (it is typically already known to be numeric).
			}
			Pattern::Tuple(entries)
				if !entries
					.iter()
					.any(|e| matches!(e.0, ListPatternEntry::Rest(_))) =>
			{
				let mut items = Vec::new();
				for entry in entries {
					if let ListPatternEntry::Item(p) = &entry.0 {
						items.push(p);
					}
				}
				let elem_tys: Vec<Ty> = items.iter().map(|_| self.fresh()).collect();
				let tuple = self.interner.mk_tuple(elem_tys.clone());
				self.unify(shape, tuple, span);
				for (p, elem) in items.iter().zip(&elem_tys) {
					self.pattern(p, *elem, mutable);
				}
			}
			// A tuple with `...rest`: unlike list rest (homogeneous elements), a tuple's
			// elements are heterogeneous, so `rest`'s type is a specific sub-tuple that
			// can only be built by slicing the scrutinee's ALREADY-KNOWN element types —
			// this is an inference inversion relative to the no-rest arm above (which
			// builds the tuple type FROM the pattern via fresh vars and unifies outward).
			Pattern::Tuple(entries) => {
				let mut prefix = Vec::new();
				let mut suffix = Vec::new();
				let mut rest_name: Option<Option<Ident>> = None;
				let mut seen_rest = false;
				for entry in entries {
					match &entry.0 {
						ListPatternEntry::Item(p) => {
							if seen_rest {
								suffix.push(p);
							} else {
								prefix.push(p);
							}
						}
						ListPatternEntry::Rest(name) => {
							seen_rest = true;
							rest_name = Some(name.clone());
						}
					}
				}
				match self.interner.kind(shape).clone() {
					crate::ty::TyKind::Tuple(elems) if elems.len() >= prefix.len() + suffix.len() => {
						let n = elems.len();
						for (i, p) in prefix.iter().enumerate() {
							self.pattern(p, elems[i], mutable);
						}
						let suf_start = n - suffix.len();
						for (j, p) in suffix.iter().enumerate() {
							self.pattern(p, elems[suf_start + j], mutable);
						}
						if let Some(Some(name)) = &rest_name {
							let mid = elems[prefix.len()..suf_start].to_vec();
							let rest_ty = self.interner.mk_tuple(mid);
							self.define_local(name.0.clone(), rest_ty, mutable);
						}
					}
					_ => {
						// Either the scrutinee isn't (yet) resolvable to a concrete tuple, or
						// the pattern names more fixed elements than any tuple could supply —
						// either way this is a genuine mismatch. Build the same-arity fresh
						// tuple the no-rest arm would (treating `rest` as contributing zero
						// elements) and let `unify` report the precise diagnostic, exactly as
						// the no-rest arm does for an ordinary arity mismatch.
						let elem_tys: Vec<Ty> = prefix
							.iter()
							.chain(suffix.iter())
							.map(|_| self.fresh())
							.collect();
						let tuple = self.interner.mk_tuple(elem_tys.clone());
						self.unify(shape, tuple, span);
						for (p, elem) in prefix.iter().chain(suffix.iter()).zip(&elem_tys) {
							self.pattern(p, *elem, mutable);
						}
						if let Some(Some(name)) = &rest_name {
							let rest_ty = self.fresh();
							self.define_local(name.0.clone(), rest_ty, mutable);
						}
					}
				}
			}
			Pattern::List(entries) => {
				let elem = self.fresh();
				let list = self.interner.mk_list(elem);
				self.unify(shape, list, span);
				for entry in entries {
					match &entry.0 {
						ListPatternEntry::Item(p) => self.pattern(p, elem, mutable),
						ListPatternEntry::Rest(Some(name)) => self.define_local(name.0.clone(), list, mutable),
						ListPatternEntry::Rest(None) => {}
					}
				}
			}
			Pattern::Map(entries) => {
				let key = self.fresh();
				let value = self.fresh();
				let map = self.interner.mk_map(key, value);
				self.unify(shape, map, span);
				for entry in entries {
					match &entry.0 {
						MapPatternEntry::Entry(k, v) => {
							self.pattern(k, key, mutable);
							self.pattern(v, value, mutable);
						}
						MapPatternEntry::Rest(Some(name)) => self.define_local(name.0.clone(), map, mutable),
						MapPatternEntry::Rest(None) => {}
					}
				}
			}
			Pattern::Struct { path, fields } => self.pattern_struct(path, fields, shape, span, mutable),
		}
	}

	fn pattern_struct(
		&mut self,
		path: &[Ident],
		fields: &[Spanned<StructPatternField>],
		ty: Ty,
		span: Span,
		mutable: bool,
	) {
		let target = self.resolve_pattern_path(path, ty, span);
		let field_tys = match target {
			Some(PatternTarget::Struct(def)) => {
				let (adt, subst) = self.instantiate_struct(def);
				self.unify(ty, adt, span);
				let sig = self.sigs.structs[&def].clone();
				sig
					.fields
					.iter()
					.map(|(n, t)| (n.clone(), self.subst(*t, &subst, None)))
					.collect::<Vec<_>>()
			}
			Some(PatternTarget::Variant(enum_def, variant)) => {
				// Record the variant this pattern resolved to (span-keyed) for lowering.
				let res = self.variant_resolution(enum_def, variant);
				self.annotations.record_pattern_variant(span, res);
				let (adt, subst) = self.instantiate_enum(enum_def);
				self.unify(ty, adt, span);
				let vsig = self.sigs.enums[&enum_def].variants[variant].clone();
				vsig
					.fields
					.iter()
					.map(|(n, t)| (n.clone(), self.subst(*t, &subst, None)))
					.collect::<Vec<_>>()
			}
			None => return,
		};

		for field in fields {
			match &field.0 {
				StructPatternField::Value { name, value } => {
					match field_tys.iter().find(|(n, _)| n == &name.0) {
						Some((_, fty)) => self.pattern(value, *fty, mutable),
						None => self.emit(
							name.1,
							TypeError::UnknownField {
								field: name.0.clone(),
							},
						),
					}
				}
				StructPatternField::Named(name) => match field_tys.iter().find(|(n, _)| n == &name.0) {
					Some((_, fty)) => self.define_local(name.0.clone(), *fty, mutable),
					// Not a field of this constructor. On a SINGLE-field constructor a bare
					// identifier is a positional sub-pattern against that sole field — a plain
					// binding (`Ok(command)`) or a nullary-variant pattern (`Ok(None)`). Reuse
					// the ordinary binding/variant check by synthesizing the equivalent
					// `Pattern::Binding`, and record the field it binds so lowering can emit the
					// access. (A multi-field constructor keeps the by-name error — there is no
					// single field to attach an un-named identifier to.)
					None if field_tys.len() == 1 => {
						let (fname, fty) = &field_tys[0];
						let (fname, fty) = (fname.clone(), *fty);
						// Key both this record and the synthesized pattern's own span on the
						// FIELD's span, which is what lowering looks the field name and any
						// nullary-variant resolution back up under.
						self.annotations.record_positional_field(field.1, fname);
						let synth = Spanned(
							Pattern::Binding {
								name: name.clone(),
								inner: Box::new(Spanned(Pattern::Placeholder, name.1)),
							},
							field.1,
						);
						self.pattern(&synth, fty, mutable);
					}
					None => self.emit(
						name.1,
						TypeError::UnknownField {
							field: name.0.clone(),
						},
					),
				},
				// A positional sub-pattern binds the constructor's sole field — only
				// unambiguous when there is exactly one. Reject it otherwise (there is no
				// single field to attach it to); named `field = pattern` is the way to
				// destructure a multi-field constructor.
				StructPatternField::Positional(pat) => {
					if let [(fname, fty)] = field_tys.as_slice() {
						// Record which field this positional sub-pattern resolved to, so
						// lowering (no type access) can emit the field access.
						self
							.annotations
							.record_positional_field(field.1, fname.clone());
						self.pattern(pat, *fty, mutable);
					} else {
						self.emit(
							field.1,
							TypeError::PositionalPatternArity {
								fields: field_tys.len(),
							},
						);
					}
				}
				StructPatternField::Rest => {}
			}
		}
	}

	/// If a bare pattern name is a nullary variant, return a fresh instance of its enum
	/// (to unify with the scrutinee). Returns `None` for a value-carrying variant or a
	/// non-variant name — both of which are then treated as a binding. `expected` is the
	/// scrutinee's (already-stripped) shape: when it names a concrete enum that has a
	/// variant matching `name`, that enum wins over the global by-name lookup — this is
	/// what lets a bare `Equal` resolve against a known `Order` scrutinee even when
	/// another enum also declares an `Equal` variant.
	fn nullary_variant_pattern(&mut self, name: &str, expected: Ty, span: Span) -> Option<Ty> {
		let resolved = match self.expected_enum_variant(expected, name) {
			Some(hit) => Ok(hit),
			None => self.defs.resolve_variant(name)?,
		};
		match resolved {
			Ok((enum_def, variant)) => {
				if self.sigs.enums[&enum_def].variants[variant]
					.fields
					.is_empty()
				{
					// A nullary variant pattern (`None`): record its resolution for lowering.
					let res = self.variant_resolution(enum_def, variant);
					self.annotations.record_pattern_variant(span, res);
					let (adt, _) = self.instantiate_enum(enum_def);
					Some(adt)
				} else {
					None
				}
			}
			Err(()) => {
				self.emit(span, TypeError::AmbiguousVariant { name: name.into() });
				Some(self.interner.error())
			}
		}
	}

	fn resolve_pattern_path(
		&mut self,
		path: &[Ident],
		expected: Ty,
		span: Span,
	) -> Option<PatternTarget> {
		match path {
			[single] => {
				if let Some(def) = self.defs.get(&single.0)
					&& let DefKind::Struct { .. } = self.defs.data(def).kind
				{
					return Some(PatternTarget::Struct(def));
				}
				if let Some((enum_def, variant)) = self.expected_enum_variant(expected, &single.0) {
					return Some(PatternTarget::Variant(enum_def, variant));
				}
				match self.defs.resolve_variant(&single.0) {
					Some(Ok((enum_def, variant))) => {
						return Some(PatternTarget::Variant(enum_def, variant));
					}
					Some(Err(())) => {
						self.emit(
							span,
							TypeError::AmbiguousVariant {
								name: single.0.clone(),
							},
						);
						return None;
					}
					None => {}
				}
				self.emit(
					span,
					TypeError::CannotFindConstructor {
						name: single.0.clone(),
					},
				);
				None
			}
			[type_name, variant_name] => {
				if let Some(def) = self.defs.get(&type_name.0)
					&& let DefKind::Enum { .. } = self.defs.data(def).kind
				{
					let position = self.sigs.enums[&def]
						.variants
						.iter()
						.position(|v| v.name == variant_name.0);
					return match position {
						Some(variant) => Some(PatternTarget::Variant(def, variant)),
						None => {
							self.emit(
								span,
								TypeError::EnumHasNoVariant {
									enum_name: type_name.0.clone(),
									variant: variant_name.0.clone(),
								},
							);
							None
						}
					};
				}
				self.emit(
					span,
					TypeError::CannotFindEnum {
						name: type_name.0.clone(),
					},
				);
				None
			}
			_ => {
				self.emit(span, TypeError::UnsupportedConstructorPath);
				None
			}
		}
	}
}
