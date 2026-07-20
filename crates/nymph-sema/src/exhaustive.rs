//! Match exhaustiveness and reachability, via Maranget's usefulness algorithm.
//!
//! A pattern vector is *useful* against a matrix of patterns if it matches some value
//! the matrix does not. Two queries drive checking (over **unguarded** arms only — a
//! guarded arm may fall through): a match is non-exhaustive iff `_` is useful against
//! all arms (the returned witness is the missing case), and arm `i` is unreachable iff
//! it is not useful against the arms before it. The algorithm specialises by
//! constructor, recursing into nested patterns, so it reasons about `Ok(Error(e))`
//! versus `Ok(Ok(x))` rather than only the outer variant.
//!
//! `int` scrutinees keep a dedicated interval check (their constructor space is
//! unbounded, but literal/range patterns can still tile the whole line).

use crate::errors::TypeError;
use ecow::EcoString;
use nymph_ast::{
	Span,
	expr::{ListPatternEntry, MatchArm, Pattern, RangePatternKind, StructPatternField},
};

use crate::check::Checker;
use crate::def::DefKind;
use crate::ids::ParamIdx;
use crate::ty::{GenericArgs, Ty, TyKind};

/// A pattern in a matrix column: either a wildcard or a borrowed AST pattern.
#[derive(Clone, Copy)]
enum Pat<'a> {
	Wild,
	Ref(&'a Pattern),
}

type Row<'a> = Vec<Pat<'a>>;

/// A value constructor for a column's type.
#[derive(Clone, PartialEq, Eq)]
enum Ctor {
	/// An enum variant (by index).
	Variant(usize),
	/// The sole constructor of a tuple or struct.
	Single,
	Bool(bool),
	/// An opaque scalar literal/range (`int`, `char`, `string`, `float`), keyed by a
	/// rendering so equal literals share a constructor. Never completes a signature.
	Scalar(String),
}

/// A reconstructed witness value showing a case a match fails to cover.
enum Witness {
	Wild,
	Con(Ctor, Vec<Witness>),
}

impl Checker<'_> {
	pub(crate) fn check_exhaustive(&mut self, scrutinee: Ty, arms: &[MatchArm], span: Span) {
		// Peel a top-level `mut` before resolving: mutability doesn't affect the
		// constructor space, but an unstripped `TyKind::Mut` would fall through to
		// the catch-all-only arm below and spuriously demand a `_` case.
		let scrutinee = self.strip_mut(scrutinee);
		let scrutinee = self.resolve_deep(scrutinee);
		match self.interner.kind(scrutinee) {
			// Can't judge an unknown or already-errored scrutinee.
			TyKind::Infer(_) | TyKind::Error => {}
			// `int`'s constructors are unbounded; reason over integer intervals instead.
			TyKind::Int => self.check_int_match(arms, span),
			// `uint` is the same interval reasoning over the unsigned domain [0, u64::MAX].
			TyKind::UInt => self.check_uint_match(arms, span),
			// Structural types have finite constructor signatures the algorithm enumerates.
			TyKind::Boolean | TyKind::Tuple(_) => self.usefulness_check(scrutinee, arms, span),
			TyKind::Adt(def, _)
				if matches!(
					self.defs.data(*def).kind,
					DefKind::Enum { .. } | DefKind::Struct { .. }
				) =>
			{
				self.usefulness_check(scrutinee, arms, span);
			}
			// Everything else is exhaustive only via a catch-all arm.
			_ => self.check_catch_all(arms, span),
		}
	}

	/// Reachability per arm and overall exhaustiveness, via the usefulness algorithm.
	fn usefulness_check(&mut self, scrutinee: Ty, arms: &[MatchArm], span: Span) {
		let mut matrix: Vec<Row> = Vec::new();
		for arm in arms {
			let query: Row = vec![Pat::Ref(&arm.pattern.0)];
			if self.useful(&matrix, &query, &[scrutinee]).is_none() {
				self.warn_unreachable(arm.body.span);
			}
			// Only unguarded arms block later ones and contribute to coverage.
			if arm.guard.is_none() {
				matrix.push(query);
			}
		}

		if let Some(witness) = self.useful(&matrix, &[Pat::Wild], &[scrutinee]) {
			let rendered = self.render_witness(witness.first().unwrap_or(&Witness::Wild), scrutinee);
			self.emit(span, TypeError::NonExhaustiveMatch { witness: rendered });
		}
	}

	/// Is `query` useful against `matrix` (columns typed by `types`)? Returns a witness
	/// value vector if so.
	fn useful(&mut self, matrix: &[Row], query: &[Pat], types: &[Ty]) -> Option<Vec<Witness>> {
		let Some((&head, rest)) = query.split_first() else {
			// No columns left: useful iff nothing above matched everything.
			return matrix.is_empty().then(Vec::new);
		};
		// A column's type can itself carry `mut` at any recursion depth (a tuple
		// element, a struct field, an enum-variant field) — strip it before it
		// drives constructor-signature lookups, or a `mut`-typed sub-position
		// falls through to "no signature" and spuriously demands a catch-all.
		let ty = self.strip_mut(types[0]);

		match self.head_ctor(head, ty) {
			Head::Or(a, b) => {
				let qa: Row = std::iter::once(a).chain(rest.iter().copied()).collect();
				if let Some(w) = self.useful(matrix, &qa, types) {
					return Some(w);
				}
				let qb: Row = std::iter::once(b).chain(rest.iter().copied()).collect();
				self.useful(matrix, &qb, types)
			}
			Head::Con(ctor) => {
				let witness = self.useful_ctor(matrix, query, &ctor, types)?;
				Some(witness)
			}
			Head::Wild => {
				let signature = self.type_signature(ty);
				let present = self.column_ctors(matrix, ty);
				let complete = signature
					.as_ref()
					.is_some_and(|all| all.iter().all(|c| present.contains(c)));

				if complete {
					// Try each constructor; a witness under any one lifts to a witness here.
					for ctor in signature.unwrap() {
						if let Some(w) = self.useful_ctor(matrix, query, &ctor, types) {
							return Some(w);
						}
					}
					None
				} else {
					// Reduce to the default matrix (rows with a wildcard head).
					let default = self.default_matrix(matrix, ty);
					let rest_witness = self.useful(&default, rest, &types[1..])?;
					// Prepend a constructor the matrix is missing (or a wildcard).
					let head_witness = match signature {
						Some(all) => all
							.into_iter()
							.find(|c| !present.contains(c))
							.map(|c| {
								let arity = self.ctor_arity(&c, ty);
								Witness::Con(c, (0..arity).map(|_| Witness::Wild).collect())
							})
							.unwrap_or(Witness::Wild),
						None => Witness::Wild,
					};
					Some(std::iter::once(head_witness).chain(rest_witness).collect())
				}
			}
		}
	}

	/// The usefulness of `query` (whose head is `ctor` or a wildcard) specialised by
	/// `ctor`, re-wrapping any witness back under `ctor`.
	fn useful_ctor(
		&mut self,
		matrix: &[Row],
		query: &[Pat],
		ctor: &Ctor,
		types: &[Ty],
	) -> Option<Vec<Witness>> {
		let ty = self.strip_mut(types[0]);
		let arity = self.ctor_arity(ctor, ty);
		let mut sub_types = self.ctor_sub_types(ctor, ty);
		sub_types.extend_from_slice(&types[1..]);

		let smatrix: Vec<Row> = matrix
			.iter()
			.flat_map(|row| self.specialize_row(row, ctor, ty))
			.collect();
		let squery = self.specialize_row(query, ctor, ty).into_iter().next()?;

		let witness = self.useful(&smatrix, &squery, &sub_types)?;
		// Fold the first `arity` witness columns back into the constructor.
		let mut it = witness.into_iter();
		let subs: Vec<Witness> = it.by_ref().take(arity).collect();
		Some(
			std::iter::once(Witness::Con(ctor.clone(), subs))
				.chain(it)
				.collect(),
		)
	}

	/// Specialise a row by `ctor`: rows headed by a matching constructor expose their
	/// sub-patterns, wildcards expand to wildcards, or-patterns split, others drop.
	fn specialize_row<'a>(&mut self, row: &[Pat<'a>], ctor: &Ctor, ty: Ty) -> Vec<Row<'a>> {
		let (&head, rest) = row.split_first().expect("non-empty row");
		match self.head_ctor(head, ty) {
			Head::Or(a, b) => {
				let ra: Row = std::iter::once(a).chain(rest.iter().copied()).collect();
				let rb: Row = std::iter::once(b).chain(rest.iter().copied()).collect();
				let mut out = self.specialize_row(&ra, ctor, ty);
				out.extend(self.specialize_row(&rb, ctor, ty));
				out
			}
			Head::Wild => {
				let arity = self.ctor_arity(ctor, ty);
				let mut new: Row = vec![Pat::Wild; arity];
				new.extend_from_slice(rest);
				vec![new]
			}
			Head::Con(c) if &c == ctor => {
				let mut new = self.ctor_subpatterns(head, ctor, ty);
				new.extend_from_slice(rest);
				vec![new]
			}
			Head::Con(_) => Vec::new(),
		}
	}

	/// The default matrix: rows whose head is a wildcard, with that head removed.
	fn default_matrix<'a>(&mut self, matrix: &[Row<'a>], ty: Ty) -> Vec<Row<'a>> {
		let mut out = Vec::new();
		for row in matrix {
			let (&head, rest) = row.split_first().expect("non-empty row");
			match self.head_ctor(head, ty) {
				Head::Wild => out.push(rest.to_vec()),
				Head::Or(a, b) => {
					let ra: Row = std::iter::once(a).chain(rest.iter().copied()).collect();
					let rb: Row = std::iter::once(b).chain(rest.iter().copied()).collect();
					out.extend(self.default_matrix(&[ra], ty));
					out.extend(self.default_matrix(&[rb], ty));
				}
				Head::Con(_) => {}
			}
		}
		out
	}

	/// The constructors appearing at the head of a matrix's first column.
	fn column_ctors(&mut self, matrix: &[Row], ty: Ty) -> Vec<Ctor> {
		let mut out: Vec<Ctor> = Vec::new();
		let mut stack: Vec<Pat> = matrix.iter().filter_map(|r| r.first().copied()).collect();
		while let Some(pat) = stack.pop() {
			match self.head_ctor(pat, ty) {
				Head::Con(c) => {
					if !out.contains(&c) {
						out.push(c);
					}
				}
				Head::Or(a, b) => {
					stack.push(a);
					stack.push(b);
				}
				Head::Wild => {}
			}
		}
		out
	}

	// ── Constructor / pattern bridge ─────────────────────────────────────────
	fn head_ctor<'a>(&self, pat: Pat<'a>, ty: Ty) -> Head<'a> {
		let Pat::Ref(p) = pat else {
			return Head::Wild;
		};
		match p {
			Pattern::Placeholder => Head::Wild,
			Pattern::Grouped(inner) => self.head_ctor(Pat::Ref(&inner.0), ty),
			Pattern::Binding { name, inner } => {
				if !matches!(inner.0, Pattern::Placeholder) {
					return self.head_ctor(Pat::Ref(&inner.0), ty);
				}
				// A bare name is a nullary-variant constructor if it names one, else a binding.
				match self.variant_index(ty, &name.0) {
					Some(index) => Head::Con(Ctor::Variant(index)),
					None => Head::Wild,
				}
			}
			Pattern::Union(a, b) => Head::Or(Pat::Ref(&a.0), Pat::Ref(&b.0)),
			Pattern::Boolean(v) => Head::Con(Ctor::Bool(v.0)),
			Pattern::Tuple(_) => Head::Con(Ctor::Single),
			Pattern::Struct { path, .. } => match self.interner.kind(ty) {
				TyKind::Adt(def, _) if matches!(self.defs.data(*def).kind, DefKind::Struct { .. }) => {
					Head::Con(Ctor::Single)
				}
				_ => match self.classify_variant(path, ty) {
					Some(index) => Head::Con(Ctor::Variant(index)),
					None => Head::Wild,
				},
			},
			Pattern::Int(n) => Head::Con(Ctor::Scalar(format!("i{}", n.0))),
			Pattern::UInt(n) => Head::Con(Ctor::Scalar(format!("u{}", n.0))),
			Pattern::Char(c) => Head::Con(Ctor::Scalar(format!("c{}", c.0))),
			Pattern::Float(f) => Head::Con(Ctor::Scalar(format!("f{}", f.0))),
			Pattern::Range(kind) => Head::Con(Ctor::Scalar(render_range(kind))),
			// Strings, lists and maps are treated as opaque single-shape constructors;
			// nested refinement of them is not modelled (and unused by real code here).
			Pattern::String(_) => Head::Con(Ctor::Scalar("string".into())),
			Pattern::List(_) => Head::Con(Ctor::Scalar("list".into())),
			Pattern::Map(_) => Head::Con(Ctor::Scalar("map".into())),
		}
	}

	/// The sub-patterns of `pat` (which matches `ctor`) in the constructor's field order.
	fn ctor_subpatterns<'a>(&self, pat: Pat<'a>, ctor: &Ctor, ty: Ty) -> Row<'a> {
		let arity = self.ctor_arity(ctor, ty);
		let Pat::Ref(mut p) = pat else {
			return vec![Pat::Wild; arity];
		};
		// Peel groupings / named bindings down to the core refutable pattern.
		loop {
			match p {
				Pattern::Grouped(inner) => p = &inner.0,
				Pattern::Binding { inner, .. } if !matches!(inner.0, Pattern::Placeholder) => {
					p = &inner.0;
				}
				_ => break,
			}
		}
		match p {
			// A tuple with no rest: one column per item, in order (the common case).
			Pattern::Tuple(entries)
				if !entries
					.iter()
					.any(|e| matches!(e.0, ListPatternEntry::Rest(_))) =>
			{
				entries
					.iter()
					.filter_map(|e| match &e.0 {
						ListPatternEntry::Item(sub) => Some(Pat::Ref(&sub.0)),
						_ => None,
					})
					.collect()
			}
			// A tuple with `...rest`: the row must still have exactly `arity` columns
			// (one per tuple element) or the matrix's column-width invariant breaks
			// (`default_matrix`'s `split_first` assumes uniform width) — expand `rest`
			// to wildcard fillers spanning the elements it covers, prefix and suffix
			// bound to the front/back columns as usual.
			Pattern::Tuple(entries) => {
				let mut prefix = Vec::new();
				let mut suffix = Vec::new();
				let mut seen_rest = false;
				for e in entries {
					match &e.0 {
						ListPatternEntry::Item(sub) => {
							if seen_rest {
								suffix.push(Pat::Ref(&sub.0));
							} else {
								prefix.push(Pat::Ref(&sub.0));
							}
						}
						ListPatternEntry::Rest(_) => seen_rest = true,
					}
				}
				// Degenerate case (already reported by the checker as a type mismatch —
				// `#(a, b, ...rest, c, d)` against a shorter tuple): prefix+suffix alone
				// can exceed `arity`. Truncate to keep the row exactly `arity` columns
				// wide rather than overrunning it and panicking downstream.
				if prefix.len() > arity {
					prefix.truncate(arity);
				}
				let remaining = arity - prefix.len();
				if suffix.len() > remaining {
					let drop = suffix.len() - remaining;
					suffix.drain(0..drop);
				}
				let filler = remaining.saturating_sub(suffix.len());
				prefix.extend(std::iter::repeat_n(Pat::Wild, filler));
				prefix.extend(suffix);
				prefix
			}
			Pattern::Struct { fields, .. } => {
				let names = self.ctor_field_names(ctor, ty);
				names
					.iter()
					.map(|fname| field_subpattern(fields, fname))
					.collect()
			}
			// Nullary variants / booleans / scalars carry no sub-patterns.
			_ => vec![Pat::Wild; arity],
		}
	}

	/// The full constructor signature of a finite type, or `None` if unbounded.
	fn type_signature(&self, ty: Ty) -> Option<Vec<Ctor>> {
		match self.interner.kind(ty) {
			TyKind::Boolean => Some(vec![Ctor::Bool(false), Ctor::Bool(true)]),
			TyKind::Tuple(_) => Some(vec![Ctor::Single]),
			TyKind::Adt(def, _) => match self.defs.data(*def).kind {
				DefKind::Enum { .. } => Some(
					(0..self.sigs.enums[def].variants.len())
						.map(Ctor::Variant)
						.collect(),
				),
				DefKind::Struct { .. } => Some(vec![Ctor::Single]),
				_ => None,
			},
			_ => None,
		}
	}

	fn ctor_arity(&self, ctor: &Ctor, ty: Ty) -> usize {
		match (ctor, self.interner.kind(ty)) {
			(Ctor::Variant(i), TyKind::Adt(def, _)) => self.sigs.enums[def].variants[*i].fields.len(),
			(Ctor::Single, TyKind::Tuple(elems)) => elems.len(),
			(Ctor::Single, TyKind::Adt(def, _)) => self.sigs.structs[def].fields.len(),
			_ => 0,
		}
	}

	fn ctor_sub_types(&mut self, ctor: &Ctor, ty: Ty) -> Vec<Ty> {
		match (ctor, self.interner.kind(ty).clone()) {
			(Ctor::Variant(i), TyKind::Adt(def, args)) => {
				let fields = self.sigs.enums[&def].variants[*i].fields.clone();
				let subst = adt_param_subst(&args);
				fields
					.iter()
					.map(|(_, t)| self.subst(*t, &subst, None))
					.collect()
			}
			(Ctor::Single, TyKind::Tuple(elems)) => elems,
			(Ctor::Single, TyKind::Adt(def, args)) => {
				let fields = self.sigs.structs[&def].fields.clone();
				let subst = adt_param_subst(&args);
				fields
					.iter()
					.map(|(_, t)| self.subst(*t, &subst, None))
					.collect()
			}
			_ => Vec::new(),
		}
	}

	fn ctor_field_names(&self, ctor: &Ctor, ty: Ty) -> Vec<EcoString> {
		match (ctor, self.interner.kind(ty)) {
			(Ctor::Variant(i), TyKind::Adt(def, _)) => self.sigs.enums[def].variants[*i]
				.fields
				.iter()
				.map(|(n, _)| n.clone())
				.collect(),
			(Ctor::Single, TyKind::Adt(def, _)) => self.sigs.structs[def]
				.fields
				.iter()
				.map(|(n, _)| n.clone())
				.collect(),
			_ => Vec::new(),
		}
	}

	/// The variant index a bare name refers to in `ty`'s enum, if it names one.
	fn variant_index(&self, ty: Ty, name: &str) -> Option<usize> {
		let TyKind::Adt(def, _) = self.interner.kind(ty) else {
			return None;
		};
		if !matches!(self.defs.data(*def).kind, DefKind::Enum { .. }) {
			return None;
		}
		self.sigs.enums[def]
			.variants
			.iter()
			.position(|v| v.name == *name)
	}

	/// The variant index a struct-pattern path refers to in `ty`'s enum, if any.
	fn classify_variant(&self, path: &[nymph_ast::Ident], ty: Ty) -> Option<usize> {
		let TyKind::Adt(def, _) = self.interner.kind(ty) else {
			return None;
		};
		let name = match path {
			[single] => &single.0,
			[type_name, variant] => {
				if self.defs.get(&type_name.0) != Some(*def) {
					return None;
				}
				&variant.0
			}
			_ => return None,
		};
		self.sigs.enums[def]
			.variants
			.iter()
			.position(|v| v.name == *name)
	}

	fn render_witness(&self, witness: &Witness, ty: Ty) -> String {
		match witness {
			Witness::Wild => "_".into(),
			Witness::Con(Ctor::Bool(b), _) => b.to_string(),
			Witness::Con(Ctor::Scalar(_), _) => "_".into(),
			Witness::Con(ctor @ (Ctor::Variant(_) | Ctor::Single), subs) => {
				let sub_types = self.witness_sub_types(ctor, ty);
				let inner: Vec<String> = subs
					.iter()
					.zip(sub_types.iter().chain(std::iter::repeat(&ty)))
					.map(|(w, &t)| self.render_witness(w, t))
					.collect();
				match (ctor, self.interner.kind(ty)) {
					(Ctor::Single, TyKind::Tuple(_)) => format!("#({})", inner.join(", ")),
					(Ctor::Variant(i), TyKind::Adt(def, _)) => {
						let name = &self.sigs.enums[def].variants[*i].name;
						if inner.is_empty() {
							name.to_string()
						} else {
							format!("{name}({})", inner.join(", "))
						}
					}
					(Ctor::Single, TyKind::Adt(def, _)) => {
						let name = self.defs.data(*def).name.clone();
						if inner.is_empty() {
							name.to_string()
						} else {
							format!("{name}({})", inner.join(", "))
						}
					}
					_ => "_".into(),
				}
			}
		}
	}

	/// Non-substituting sub-column types for rendering a witness (generic params render
	/// as `_`, which is fine for a diagnostic).
	fn witness_sub_types(&self, ctor: &Ctor, ty: Ty) -> Vec<Ty> {
		match (ctor, self.interner.kind(ty)) {
			(Ctor::Variant(i), TyKind::Adt(def, _)) => self.sigs.enums[def].variants[*i]
				.fields
				.iter()
				.map(|(_, t)| *t)
				.collect(),
			(Ctor::Single, TyKind::Tuple(elems)) => elems.clone(),
			(Ctor::Single, TyKind::Adt(def, _)) => self.sigs.structs[def]
				.fields
				.iter()
				.map(|(_, t)| *t)
				.collect(),
			_ => Vec::new(),
		}
	}

	/// Check an `int` match by covering the full `i64` line with literal/range patterns.
	fn check_int_match(&mut self, arms: &[MatchArm], span: Span) {
		let mut ranges: Vec<(i64, i64)> = Vec::new();
		for arm in arms {
			if arm.guard.is_some() {
				continue;
			}
			match int_cover(&arm.pattern.0) {
				IntCover::All => return,
				IntCover::Ranges(rs) => ranges.extend(rs),
			}
		}
		if !covers_full_int_range(ranges) {
			self.emit(span, TypeError::NonExhaustiveInt);
		}
	}

	fn check_uint_match(&mut self, arms: &[MatchArm], span: Span) {
		let mut ranges: Vec<(u64, u64)> = Vec::new();
		for arm in arms {
			if arm.guard.is_some() {
				continue;
			}
			match uint_cover(&arm.pattern.0) {
				UIntCover::All => return,
				UIntCover::Ranges(rs) => ranges.extend(rs),
			}
		}
		if !covers_full_uint_range(ranges) {
			self.emit(span, TypeError::NonExhaustiveUInt);
		}
	}

	fn check_catch_all(&mut self, arms: &[MatchArm], span: Span) {
		let mut has_catch_all = false;
		for arm in arms {
			if arm.guard.is_none() && is_irrefutable(&arm.pattern.0) {
				if has_catch_all {
					self.warn_unreachable(arm.body.span);
				}
				has_catch_all = true;
			}
		}
		if !has_catch_all {
			self.emit(span, TypeError::NonExhaustiveNeedsWildcard);
		}
	}

	fn warn_unreachable(&mut self, span: Span) {
		self.emit(span, TypeError::UnreachableArm);
	}
}

/// The head of a pattern within a column: a wildcard, an or-split, or a constructor.
enum Head<'a> {
	Wild,
	Or(Pat<'a>, Pat<'a>),
	Con(Ctor),
}

/// The sub-pattern a struct/variant pattern gives for field `fname` (a wildcard if the
/// field is absent, a rest pattern, or only a binding).
fn field_subpattern<'a>(
	fields: &'a [nymph_ast::Spanned<StructPatternField>],
	fname: &str,
) -> Pat<'a> {
	for field in fields {
		match &field.0 {
			StructPatternField::Value { name, value } if name.0 == *fname => {
				return Pat::Ref(&value.0);
			}
			StructPatternField::Named(name) if name.0 == *fname => return Pat::Wild,
			_ => {}
		}
	}
	Pat::Wild
}

/// A map from an ADT's `ParamIdx` to its concrete generic arguments.
fn adt_param_subst(args: &GenericArgs) -> rustc_hash::FxHashMap<ParamIdx, Ty> {
	args
		.positional
		.iter()
		.enumerate()
		.map(|(i, &t)| (ParamIdx(i as u32), t))
		.collect()
}

fn render_range(kind: &RangePatternKind) -> String {
	// A stable key so identical ranges share a constructor.
	format!("range:{kind:?}")
}

/// What an integer pattern covers on the `i64` line.
enum IntCover {
	All,
	Ranges(Vec<(i64, i64)>),
}

fn int_cover(pattern: &Pattern) -> IntCover {
	match pattern {
		Pattern::Placeholder => IntCover::All,
		Pattern::Binding { inner, .. } => int_cover(&inner.0),
		Pattern::Grouped(inner) => int_cover(&inner.0),
		Pattern::Union(a, b) => match (int_cover(&a.0), int_cover(&b.0)) {
			(IntCover::All, _) | (_, IntCover::All) => IntCover::All,
			(IntCover::Ranges(mut x), IntCover::Ranges(y)) => {
				x.extend(y);
				IntCover::Ranges(x)
			}
		},
		Pattern::Int(n) => IntCover::Ranges(vec![(n.0, n.0)]),
		Pattern::Range(kind) => match int_range_bounds(kind) {
			Some((lo, hi)) if lo <= hi => IntCover::Ranges(vec![(lo, hi)]),
			_ => IntCover::Ranges(Vec::new()),
		},
		_ => IntCover::Ranges(Vec::new()),
	}
}

fn int_range_bounds(kind: &RangePatternKind) -> Option<(i64, i64)> {
	match kind {
		RangePatternKind::From(min) => Some((int_lit(&min.0)?, i64::MAX)),
		RangePatternKind::To(max) => Some((i64::MIN, int_lit(&max.0)?.saturating_sub(1))),
		RangePatternKind::ToInclusive(max) => Some((i64::MIN, int_lit(&max.0)?)),
		RangePatternKind::Exclusive { min, max } => {
			Some((int_lit(&min.0)?, int_lit(&max.0)?.saturating_sub(1)))
		}
		RangePatternKind::Inclusive { min, max } => Some((int_lit(&min.0)?, int_lit(&max.0)?)),
	}
}

fn int_lit(pattern: &Pattern) -> Option<i64> {
	match pattern {
		Pattern::Int(n) => Some(n.0),
		Pattern::Grouped(inner) => int_lit(&inner.0),
		_ => None,
	}
}

fn covers_full_int_range(mut ranges: Vec<(i64, i64)>) -> bool {
	ranges.retain(|&(lo, hi)| lo <= hi);
	ranges.sort_by_key(|&(lo, _)| lo);
	let Some(&(first_lo, first_hi)) = ranges.first() else {
		return false;
	};
	if first_lo > i64::MIN {
		return false;
	}
	let mut reach = first_hi;
	for &(lo, hi) in &ranges[1..] {
		if reach == i64::MAX {
			return true;
		}
		if lo > reach + 1 {
			return false;
		}
		reach = reach.max(hi);
	}
	reach == i64::MAX
}

/// What a `uint` pattern covers on the unsigned `u64` line — the `int` machinery's
/// counterpart over the `[0, u64::MAX]` domain. A `uint` match arm must use
/// `u`-suffixed literals (`0u`, `1u..`); a plain `int` literal against a `uint`
/// scrutinee is a separate "mismatched types" error from pattern inference, so
/// `uint_cover`/`uint_lit` key on `Pattern::UInt` only.
enum UIntCover {
	All,
	Ranges(Vec<(u64, u64)>),
}

fn uint_cover(pattern: &Pattern) -> UIntCover {
	match pattern {
		Pattern::Placeholder => UIntCover::All,
		Pattern::Binding { inner, .. } => uint_cover(&inner.0),
		Pattern::Grouped(inner) => uint_cover(&inner.0),
		Pattern::Union(a, b) => match (uint_cover(&a.0), uint_cover(&b.0)) {
			(UIntCover::All, _) | (_, UIntCover::All) => UIntCover::All,
			(UIntCover::Ranges(mut x), UIntCover::Ranges(y)) => {
				x.extend(y);
				UIntCover::Ranges(x)
			}
		},
		Pattern::UInt(n) => UIntCover::Ranges(vec![(n.0, n.0)]),
		Pattern::Range(kind) => match uint_range_bounds(kind) {
			Some((lo, hi)) if lo <= hi => UIntCover::Ranges(vec![(lo, hi)]),
			_ => UIntCover::Ranges(Vec::new()),
		},
		_ => UIntCover::Ranges(Vec::new()),
	}
}

fn uint_range_bounds(kind: &RangePatternKind) -> Option<(u64, u64)> {
	match kind {
		RangePatternKind::From(min) => Some((uint_lit(&min.0)?, u64::MAX)),
		// `..max` / `min..max` are half-open: a `checked_sub` underflows to `None` when
		// `max` is `0` (`..0u` covers nothing), rather than wrapping around the domain.
		RangePatternKind::To(max) => Some((0, uint_lit(&max.0)?.checked_sub(1)?)),
		RangePatternKind::ToInclusive(max) => Some((0, uint_lit(&max.0)?)),
		RangePatternKind::Exclusive { min, max } => {
			Some((uint_lit(&min.0)?, uint_lit(&max.0)?.checked_sub(1)?))
		}
		RangePatternKind::Inclusive { min, max } => Some((uint_lit(&min.0)?, uint_lit(&max.0)?)),
	}
}

fn uint_lit(pattern: &Pattern) -> Option<u64> {
	match pattern {
		Pattern::UInt(n) => Some(n.0),
		Pattern::Grouped(inner) => uint_lit(&inner.0),
		_ => None,
	}
}

fn covers_full_uint_range(mut ranges: Vec<(u64, u64)>) -> bool {
	ranges.retain(|&(lo, hi)| lo <= hi);
	ranges.sort_by_key(|&(lo, _)| lo);
	let Some(&(first_lo, first_hi)) = ranges.first() else {
		return false;
	};
	if first_lo > 0 {
		return false;
	}
	let mut reach = first_hi;
	for &(lo, hi) in &ranges[1..] {
		// Short-circuit BEFORE `reach + 1`, which would overflow at `u64::MAX`.
		if reach == u64::MAX {
			return true;
		}
		if lo > reach + 1 {
			return false;
		}
		reach = reach.max(hi);
	}
	reach == u64::MAX
}

fn is_irrefutable(pattern: &Pattern) -> bool {
	match pattern {
		Pattern::Placeholder => true,
		Pattern::Binding { inner, .. } => is_irrefutable(&inner.0),
		Pattern::Grouped(inner) => is_irrefutable(&inner.0),
		_ => false,
	}
}
