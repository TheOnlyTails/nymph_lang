//! The abstract syntax tree, token vocabulary, and source spans shared across the
//! Nymph toolchain.
//!
//! This crate is intentionally dependency-light and pure data: it defines *what* a
//! Nymph program looks like as a tree, but contains no lexing, parsing, checking, or
//! code-generation logic. Every downstream crate ([`nymph-syntax`], [`nymph-sema`],
//! [`nymph-codegen`], the driver, and the tooling) speaks in terms of these types.
//!
//! All nodes derive [`salsa::Update`] so that a whole tree can be stored inside the
//! incremental compilation database without extra glue.

use std::fmt::Display;

use ecow::EcoString;

pub mod decl;
pub mod expr;
pub mod ops;
pub mod token;
pub mod ty;

/// A source-relative identifier: its text plus the span it occupied.
pub type Ident = Spanned<EcoString>;

/// A half-open byte range `[start, end)` into a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::Update)]
pub struct Span {
	pub start: usize,
	pub end: usize,
}

impl Span {
	pub fn new(start: usize, end: usize) -> Self {
		Self { start, end }
	}

	/// The smallest span covering both `self` and `other`.
	pub fn to(self, other: Span) -> Span {
		Span {
			start: self.start.min(other.start),
			end: self.end.max(other.end),
		}
	}

	pub fn len(self) -> usize {
		self.end.saturating_sub(self.start)
	}

	pub fn is_empty(self) -> bool {
		self.end <= self.start
	}
}

impl From<std::ops::Range<usize>> for Span {
	fn from(range: std::ops::Range<usize>) -> Self {
		Self {
			start: range.start,
			end: range.end,
		}
	}
}

impl From<Span> for std::ops::Range<usize> {
	fn from(span: Span) -> Self {
		span.start..span.end
	}
}

/// A value paired with the source [`Span`] it was produced from.
///
/// This is the workhorse wrapper of the AST: nearly every node is stored as a
/// `Spanned<T>` so diagnostics, hover, and go-to-definition can point back at exact
/// source ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::Update)]
pub struct Spanned<T>(pub T, pub Span);

impl<T> Spanned<T> {
	pub fn new(value: T, span: impl Into<Span>) -> Self {
		Self(value, span.into())
	}

	pub fn value(&self) -> &T {
		&self.0
	}

	pub fn span(&self) -> Span {
		self.1
	}

	pub fn map<R>(self, f: impl FnOnce(T) -> R) -> Spanned<R> {
		Spanned(f(self.0), self.1)
	}

	pub fn as_ref(&self) -> Spanned<&T> {
		Spanned(&self.0, self.1)
	}
}

impl<T: Display> Display for Spanned<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}[{}..{}]", self.0, self.1.start, self.1.end)
	}
}

/// Convenience for constructing a `Spanned` value in tests and builders.
pub trait IntoSpanned: Sized {
	fn spanned(self, span: impl Into<Span>) -> Spanned<Self> {
		Spanned(self, span.into())
	}
}

impl<T> IntoSpanned for T {}
