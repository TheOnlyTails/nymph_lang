use strum::FromRepr;

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, FromRepr, salsa::SalsaValue)]
pub enum Precedence {
	Assignment,
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
	/// `?`
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

#[derive(Clone, Copy, PartialEq, Eq, Debug, salsa::SalsaValue)]
pub enum AssignOperator {
	/// `=`
	Assign,
	/// `+=`
	PlusAssign,
	/// `-=`
	MinusAssign,
	/// `*=`
	TimesAssign,
	/// `/=`
	DivideAssign,
	/// `%=`
	RemainderAssign,
	/// `**=`
	PowerAssign,
	/// `<<=`
	LeftShiftAssign,
	/// `>>=`
	RightShiftAssign,
	/// `&=`
	BitAndAssign,
	/// `^=`
	BitXorAssign,
	/// `|=`
	BitOrAssign,
	/// `~=`
	BitNotAssign,
	/// `&&=`
	BoolAndAssign,
	/// `||=`
	BoolOrAssign,
}
