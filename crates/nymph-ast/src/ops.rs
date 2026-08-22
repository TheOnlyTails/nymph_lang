//! Operator kinds and the precedence ladder used by the Pratt parser.

use strum::FromRepr;

/// Binding strength of an operator, lowest to highest. The Pratt parser consults this
/// to decide how to fold `a + b * c` into `a + (b * c)`.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, FromRepr, salsa::SalsaValue)]
pub enum Precedence {
	Pipeline,
	BoolOr,
	BoolAnd,
	Equality,
	Comparison,
	In,
	Unwrap,
	BitOr,
	BitXor,
	BitAnd,
	BitShift,
	Range,
	Addition,
	Multiplication,
	Power,
	Is,
	As,
	Unary,
	IndexAccess,
	MemberAccess,
	FuncCall,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::SalsaValue)]
pub enum PrefixOperator {
	/// `!`
	BoolNot,
	/// `-`
	Negate,
	/// `~`
	BitNot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::SalsaValue)]
pub enum PostfixOperator {
	/// `?` — propagate an error / `None` to a callable or labeled target.
	ErrorReturn,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::SalsaValue)]
pub enum BinaryOperator {
	/// `+`
	Plus,
	/// `-`
	Minus,
	/// `*`
	Times,
	/// `/`
	Divide,
	/// `%`
	Remainder,
	/// `**`
	Power,
	/// `&`
	BitAnd,
	/// `|`
	BitOr,
	/// `^`
	BitXor,
	/// `<<`
	LeftShift,
	/// `>>`
	RightShift,
	/// `==`
	Equals,
	/// `!=`
	NotEquals,
	/// `<`
	LessThan,
	/// `<=`
	LessThanEquals,
	/// `>`
	GreaterThan,
	/// `>=`
	GreaterThanEquals,
	/// `in`
	In,
	/// `!in`
	NotIn,
	/// `&&`
	BoolAnd,
	/// `||`
	BoolOr,
	/// `|>`
	Pipe,
	/// `??`
	Unwrap,
}

impl BinaryOperator {
	pub fn precedence(self) -> Precedence {
		use BinaryOperator::*;
		match self {
			Pipe => Precedence::Pipeline,
			BoolOr => Precedence::BoolOr,
			BoolAnd => Precedence::BoolAnd,
			Equals | NotEquals => Precedence::Equality,
			LessThan | LessThanEquals | GreaterThan | GreaterThanEquals => Precedence::Comparison,
			In | NotIn => Precedence::In,
			Unwrap => Precedence::Unwrap,
			BitOr => Precedence::BitOr,
			BitXor => Precedence::BitXor,
			BitAnd => Precedence::BitAnd,
			LeftShift | RightShift => Precedence::BitShift,
			Plus | Minus => Precedence::Addition,
			Times | Divide | Remainder => Precedence::Multiplication,
			Power => Precedence::Power,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::SalsaValue)]
pub enum TypeOperator {
	/// `as`
	As,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::SalsaValue)]
pub enum PatternOperator {
	/// `is`
	Is,
	/// `!is`
	NotIs,
}
