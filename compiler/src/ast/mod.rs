use std::{fmt::Display, ops::Range};

use chumsky::{
	extra::ParserExtra,
	input::{Input, MapExtra},
	span::SimpleSpan,
};
use ecow::EcoString;

pub(crate) mod declaration;
pub(crate) mod error;
pub(crate) mod expr;
pub(crate) mod ops;
pub(crate) mod types;

pub(crate) type Ident = Spanned<EcoString>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Spanned<T>(pub(crate) T, pub(crate) SimpleSpan);

impl<T> Spanned<T> {
	pub(crate) fn new<'src, I: Input<'src, Span = SimpleSpan>, E: ParserExtra<'src, I>>(
		value: T,
		e: &mut MapExtra<'src, '_, I, E>,
	) -> Self {
		Self(value, e.span())
	}

	pub(crate) fn start(&self) -> usize {
		self.1.start
	}

	pub(crate) fn end(&self) -> usize {
		self.1.end
	}

	pub(crate) fn span(&self) -> SimpleSpan {
		self.1
	}

	pub(crate) fn value(&self) -> &T {
		&self.0
	}

	pub(crate) fn from_range(value: T, range: Range<usize>) -> Self {
		Self(value, range.into())
	}

	fn map<R, F: Fn(&T) -> R>(&self, f: F) -> Spanned<R> {
		Spanned(f(&self.0), self.1)
	}
}

impl<T: Display> Display for Spanned<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}[{}..{}]", self.0, self.1.start, self.1.end)
	}
}

pub(crate) trait SpannedExt where Self: Sized {
	fn spanned(self, range: Range<usize>) -> Spanned<Self>;
}

impl<T> SpannedExt for T {
	fn spanned(self, range: Range<usize>) -> Spanned<Self> {
		Spanned(self, range.into())
	}
}
