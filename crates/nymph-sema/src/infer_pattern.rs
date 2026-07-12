//! Pattern typing and binding collection.
//!
//! A pattern is checked against an expected scrutinee type: literal patterns unify
//! with it, structural patterns decompose it, and binding patterns introduce
//! locals. `bind_pattern` (for `let`/parameters) and `check_pattern` (for `match`
//! and `is`) differ only in the mutability the introduced bindings get.
//!
//! Milestone A does not yet check exhaustiveness or that union arms bind identical
//! names — those are Milestone B (`exhaustive.rs`).

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
		match &pattern.0 {
			Pattern::Placeholder => {}
			Pattern::Binding { name, inner } => {
				// A bare name (`None`, `Red`) that names a nullary variant is a constructor
				// pattern, not a new binding — resolution, not capitalisation, decides.
				if matches!(inner.0, Pattern::Placeholder)
					&& let Some(adt) = self.nullary_variant_pattern(&name.0, span)
				{
					self.unify(ty, adt, span);
				} else {
					self.define_local(name.0.clone(), ty, mutable);
					if !matches!(inner.0, Pattern::Placeholder) {
						self.pattern(inner, ty, mutable);
					}
				}
			}
			Pattern::Int(_) => {
				let t = self.interner.int();
				self.unify(ty, t, span);
			}
			Pattern::UInt(_) => {
				let t = self.interner.uint();
				self.unify(ty, t, span);
			}
			Pattern::Float(_) => {
				let t = self.interner.float();
				self.unify(ty, t, span);
			}
			Pattern::Char(_) => {
				let t = self.interner.char();
				self.unify(ty, t, span);
			}
			Pattern::Boolean(_) => {
				let t = self.interner.boolean();
				self.unify(ty, t, span);
			}
			Pattern::String(_) => {
				let t = self.interner.string();
				self.unify(ty, t, span);
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
			Pattern::Tuple(entries) => {
				let mut items = Vec::new();
				for entry in entries {
					if let ListPatternEntry::Item(p) = &entry.0 {
						items.push(p);
					}
				}
				let elem_tys: Vec<Ty> = items.iter().map(|_| self.fresh()).collect();
				let tuple = self.interner.mk_tuple(elem_tys.clone());
				self.unify(ty, tuple, span);
				for (p, elem) in items.iter().zip(&elem_tys) {
					self.pattern(p, *elem, mutable);
				}
			}
			Pattern::List(entries) => {
				let elem = self.fresh();
				let list = self.interner.mk_list(elem);
				self.unify(ty, list, span);
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
				self.unify(ty, map, span);
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
			Pattern::Struct { path, fields } => self.pattern_struct(path, fields, ty, span, mutable),
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
		let target = self.resolve_pattern_path(path, span);
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
					None => self.emit(
						name.1,
						TypeError::UnknownField {
							field: name.0.clone(),
						},
					),
				},
				StructPatternField::Rest => {}
			}
		}
	}

	/// If a bare pattern name is a nullary variant, return a fresh instance of its enum
	/// (to unify with the scrutinee). Returns `None` for a value-carrying variant or a
	/// non-variant name — both of which are then treated as a binding.
	fn nullary_variant_pattern(&mut self, name: &str, span: Span) -> Option<Ty> {
		match self.defs.resolve_variant(name)? {
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

	fn resolve_pattern_path(&mut self, path: &[Ident], span: Span) -> Option<PatternTarget> {
		match path {
			[single] => {
				if let Some(def) = self.defs.get(&single.0)
					&& let DefKind::Struct { .. } = self.defs.data(def).kind
				{
					return Some(PatternTarget::Struct(def));
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
