//! Checker-phase validation of the program's entry point (`main`).
//!
//! Entry mode (`check_module_entry`, see `check.rs`) requires the module to
//! declare a top-level `func main` taking no parameters, declaring no generic
//! parameters, and resolving to one of the supported root result shapes;
//! library mode (`check_module`) never runs this pass. This stays separate
//! from `check.rs` to keep that file from growing further.

use nymph_ast::Span;
use nymph_ast::decl::Declaration;

use crate::check::Checker;
use crate::errors::TypeError;
use crate::{DefKind, GenericArgs, Ty, TyKind};

/// Exact semantic result shape accepted at the executable root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryRootShape {
	Void,
	Option,
	Result,
	TaskVoid,
	TaskOption,
	TaskResult,
}

impl Checker<'_> {
	/// Validate the module's entry point. Called once, in entry mode only,
	/// after every body has been checked (see `check::check_module_impl`), so
	/// its diagnostics append after any body-checking diagnostics.
	///
	/// Resolution deliberately walks `self.module.members` directly rather
	/// than `self.defs.by_name`: `build_def_map`'s "later definition wins"
	/// rule means a later `struct main`/`let main` sharing the name would
	/// otherwise shadow the func entry in the def map. A struct/enum method
	/// named `main` can never false-positive here either — it lives inside
	/// that `Declaration::Struct`/`Enum`'s own `members`, never as a
	/// top-level `Declaration::Func` — so "a method named `main` doesn't
	/// count" holds by construction, with no special-casing needed. An
	/// `external` func named `main` also doesn't satisfy this: it has no body
	/// to run as the program's entry point, and isn't a `Declaration::Func`.
	pub(crate) fn check_entry_main(&mut self) -> Option<EntryRootShape> {
		let main = self
			.module
			.members
			.iter()
			.enumerate()
			.find_map(|(member, decl)| match decl {
				Declaration::Func { meta, .. } if meta.name.0 == "main" => Some((member, meta)),
				_ => None,
			});

		let Some((member, meta)) = main else {
			self.emit(Span::new(0, 0), TypeError::MainMissing);
			return None;
		};

		// Independent checks: a single malformed `main` can fail more than
		// one rule at once (e.g. both generic and parameterized), and each is
		// worth reporting on its own rather than only ever surfacing the
		// first.
		if let (Some(first), Some(last)) = (meta.generics.first(), meta.generics.last()) {
			self.emit(first.span().to(last.span()), TypeError::MainGeneric);
		}

		if let (Some(first), Some(last)) = (meta.params.first(), meta.params.last()) {
			self.emit(first.span().to(last.span()), TypeError::MainHasParams);
		}

		let main_def = self
			.defs
			.defs
			.iter()
			.enumerate()
			.find_map(|(index, definition)| {
				let id = crate::DefId(index as u32);
				(definition.kind == DefKind::Func && self.defs.local_member(id) == Some(member))
					.then_some(id)
			});
		let shape = main_def
			.and_then(|main| self.sigs.funcs.get(&main).map(|signature| signature.ret))
			.and_then(|root| self.classify_entry_root(root));
		if shape.is_none() {
			let span = meta
				.return_type
				.as_ref()
				.map_or_else(|| meta.name.span(), |ret| ret.span());
			self.emit(span, TypeError::MainNonVoidReturn);
		}
		shape
	}

	fn classify_entry_root(&mut self, root: Ty) -> Option<EntryRootShape> {
		let root = self.resolve_deep(root);
		match self.interner.kind(root).clone() {
			TyKind::Void => Some(EntryRootShape::Void),
			TyKind::Adt(definition, arguments) => self.classify_sync_root(definition, &arguments),
			TyKind::Task { output, .. } => match self.classify_entry_root(output)? {
				EntryRootShape::Void => Some(EntryRootShape::TaskVoid),
				EntryRootShape::Option => Some(EntryRootShape::TaskOption),
				EntryRootShape::Result => Some(EntryRootShape::TaskResult),
				EntryRootShape::TaskVoid | EntryRootShape::TaskOption | EntryRootShape::TaskResult => None,
			},
			_ => None,
		}
	}

	fn classify_sync_root(
		&mut self,
		definition: crate::DefId,
		arguments: &GenericArgs,
	) -> Option<EntryRootShape> {
		if !arguments.named.is_empty() {
			return None;
		}
		if Some(definition) == self.runtime_roles.option
			&& let [value] = arguments.positional.as_slice()
		{
			let value = self.resolve_deep(*value);
			if matches!(self.interner.kind(value), TyKind::Void) {
				return Some(EntryRootShape::Option);
			}
		}
		if Some(definition) == self.runtime_roles.result
			&& let [value, _error] = arguments.positional.as_slice()
		{
			let value = self.resolve_deep(*value);
			if matches!(self.interner.kind(value), TyKind::Void) {
				return Some(EntryRootShape::Result);
			}
		}
		None
	}
}
