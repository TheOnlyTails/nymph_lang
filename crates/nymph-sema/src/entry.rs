//! Checker-phase validation of the program's entry point (`main`).
//!
//! Entry mode (`check_module_entry`, see `check.rs`) requires the module to
//! declare a top-level `func main` taking no parameters, declaring no generic
//! parameters, and declaring no return type other than `void`; library mode
//! (`check_module`) never runs this pass. Deliberately its own small file
//! rather than folded into `check.rs`, per the crate's anti-monolith split
//! (see `check.rs`'s module doc).

use nymph_ast::Span;
use nymph_ast::decl::Declaration;
use nymph_ast::ty::Type;

use crate::check::Checker;
use crate::errors::TypeError;

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
	pub(crate) fn check_entry_main(&mut self) {
		let main = self.module.members.iter().find_map(|decl| match decl {
			Declaration::Func { meta, .. } if meta.name.0 == "main" => Some(meta),
			_ => None,
		});

		let Some(meta) = main else {
			self.emit(Span::new(0, 0), TypeError::MainMissing);
			return;
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

		if let Some(ret) = &meta.return_type {
			// This is an AST-declared-annotation-only rule: an unannotated
			// `main` is accepted even when its body infers a non-`void`
			// type (`func main() = 42`), and `type V = void; func main():
			// V = {}` is rejected despite `V` being an alias for `void` —
			// only the surface annotation is inspected, not the lowered
			// semantic type. Unwrap a `Grouped` annotation first so `func
			// main(): (void)` is accepted like the unparenthesized form.
			let mut inner = ret;
			while let Type::Grouped(boxed) = inner.value() {
				inner = boxed.as_ref();
			}
			if !matches!(inner.value(), Type::Void) {
				self.emit(ret.span(), TypeError::MainNonVoidReturn);
			}
		}
	}
}
