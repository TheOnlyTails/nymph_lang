//! The abstract syntax tree, token vocabulary, and source spans shared across the
//! Nymph toolchain.
//!
//! This crate is intentionally dependency-light and pure data: it defines *what* a
//! Nymph program looks like as a tree, but contains no lexing, parsing, checking, or
//! code-generation logic. Every downstream crate ([`nymph-syntax`], [`nymph-sema`],
//! [`nymph-codegen`], the driver, and the tooling) speaks in terms of these types.
//!
//! All nodes derive [`salsa::SalsaValue`] so that a whole tree can be stored inside the
//! incremental compilation database without extra glue.

#![warn(clippy::all)]

use std::fmt::Display;

use ecow::EcoString;

pub mod decl;
pub mod expr;
pub mod ops;
pub mod token;
pub mod ty;

/// A source-relative identifier: its text plus the span it occupied.
pub type Ident = Spanned<EcoString>;

/// Stable identity for the expansion that produced syntax. Zero denotes source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct OriginId(pub u64);

impl OriginId {
	pub const SOURCE: Self = Self(0);
}

/// The name-resolution context carried by every token and identifier span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum SyntaxContext {
	#[default]
	Source,
	Definition(u64),
	CallSite(OriginId),
	Fresh {
		origin: OriginId,
		index: u32,
	},
	Exposed(OriginId),
}

/// A half-open byte range `[start, end)` into a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct Span {
	pub start: usize,
	pub end: usize,
	pub context: SyntaxContext,
	pub origin: OriginId,
}

impl Span {
	pub const fn new(start: usize, end: usize) -> Self {
		Self {
			start,
			end,
			context: SyntaxContext::Source,
			origin: OriginId::SOURCE,
		}
	}

	pub fn with_context(mut self, context: SyntaxContext) -> Self {
		self.context = context;
		self
	}

	pub fn with_origin(mut self, origin: OriginId) -> Self {
		self.origin = origin;
		self
	}

	/// The smallest span covering both `self` and `other`.
	pub fn to(self, other: Span) -> Span {
		Span {
			start: self.start.min(other.start),
			end: self.end.max(other.end),
			context: if self.context == SyntaxContext::Source {
				other.context
			} else {
				self.context
			},
			origin: if self.origin == OriginId::SOURCE {
				other.origin
			} else {
				self.origin
			},
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
			context: SyntaxContext::Source,
			origin: OriginId::SOURCE,
		}
	}
}

impl From<Span> for std::ops::Range<usize> {
	fn from(span: Span) -> Self {
		span.start..span.end
	}
}

/// Stable identity for an AST expression node, assigned once by the parser in
/// construction order. Distinct from [`Span`]: two nodes can share text but never
/// an id. Used to key semantic annotations (resolved types, operator impl
/// selections) that later passes read back.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, salsa::SalsaValue)]
pub struct NodeId(pub u32, pub OriginId);

impl NodeId {
	/// A placeholder id for nodes built outside the parser (tests, synthetic
	/// nodes). Never assigned by the parser, so it never collides with a real id.
	pub const DUMMY: NodeId = NodeId(u32::MAX, OriginId::SOURCE);

	pub const fn source(local: u32) -> Self {
		Self(local, OriginId::SOURCE)
	}

	pub const fn generated(local: u32, origin: OriginId) -> Self {
		Self(local, origin)
	}
}

/// A value paired with the source [`Span`] it was produced from.
///
/// This is the workhorse wrapper of the AST: nearly every node is stored as a
/// `Spanned<T>` so diagnostics, hover, and go-to-definition can point back at exact
/// source ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
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
