use std::fmt::Display;

use chumsky::{
	extra::ParserExtra,
	input::{Input, MapExtra},
	span::SimpleSpan,
};
use ecow::EcoString;

pub mod declaration;
pub mod display;
pub mod expr;
pub mod ops;
pub mod types;

pub type Ident = Spanned<EcoString>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct Span {
	pub start: usize,
	pub end: usize,
}

impl Span {
	pub fn new(start: usize, end: usize) -> Self {
		Self { start, end }
	}
}

impl From<SimpleSpan> for Span {
	fn from(span: SimpleSpan) -> Self {
		Self {
			start: span.start,
			end: span.end,
		}
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct Spanned<T>(pub T, pub Span);

impl<T> Spanned<T> {
	pub(crate) fn new<'src, I: Input<'src, Span = SimpleSpan>, E: ParserExtra<'src, I>>(
		value: T,
		e: &mut MapExtra<'src, '_, I, E>,
	) -> Self {
		Self(value, e.span().into())
	}

	#[allow(dead_code)]
	fn map<R, F: Fn(&T) -> R>(&self, f: F) -> Spanned<R> {
		Spanned(f(&self.0), self.1)
	}
}

impl<T: Display> Display for Spanned<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}[{}..{}]", self.0, self.1.start, self.1.end)
	}
}

#[cfg(test)]
pub(crate) trait SpannedExt
where
	Self: Sized,
{
	fn spanned<S: Into<Span>>(self, range: S) -> Spanned<Self>;
}

#[cfg(test)]
impl<T> SpannedExt for T {
	fn spanned<S: Into<Span>>(self, range: S) -> Spanned<Self> {
		Spanned(self, range.into())
	}
}
