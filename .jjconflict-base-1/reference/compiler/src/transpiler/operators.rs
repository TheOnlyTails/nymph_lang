use crate::ast::ops::{AssignOperator, BinaryOperator, PostfixOperator, PrefixOperator};

/// Returns the interface method name for a binary operator.
///
/// All binary operators in Nymph are dispatched as method calls
/// on the LHS value, following the stdlib operator interfaces.
pub fn binary_op_method(op: BinaryOperator) -> &'static str {
	match op {
		BinaryOperator::Plus => "plus",
		BinaryOperator::Minus => "minus",
		BinaryOperator::Times => "times",
		BinaryOperator::Divide => "divide",
		BinaryOperator::Remainder => "remainder",
		BinaryOperator::Power => "power",
		BinaryOperator::BitAnd => "bit_and",
		BinaryOperator::BitOr => "bit_or",
		BinaryOperator::BitXor => "bit_xor",
		BinaryOperator::LeftShift => "shl",
		BinaryOperator::RightShift => "shr",
		BinaryOperator::Equals => "equals",
		BinaryOperator::NotEquals => "not_equals",
		BinaryOperator::LessThan => "less_than",
		BinaryOperator::LessThanEquals => "less_than_eq",
		BinaryOperator::GreaterThan => "greater_than",
		BinaryOperator::GreaterThanEquals => "greater_than_eq",
		BinaryOperator::BoolAnd => "and",
		BinaryOperator::BoolOr => "or",
		// These are handled specially in emit.rs, not as simple method calls
		BinaryOperator::In | BinaryOperator::NotIn => "contains",
		BinaryOperator::Pipe => unreachable!("pipe is rewritten, not a method call"),
		BinaryOperator::Unwrap => "unwrap_or",
	}
}

/// Returns the interface method name for a prefix operator.
pub fn prefix_op_method(op: PrefixOperator) -> &'static str {
	match op {
		PrefixOperator::BoolNot => "not",
		PrefixOperator::Negate => "negate",
		PrefixOperator::BitNot => "bit_not",
	}
}

/// Returns the interface method name for a postfix operator, if any.
pub fn postfix_op_method(op: PostfixOperator) -> &'static str {
	match op {
		// `x?` is the error-return operator; for now we treat it as identity
		PostfixOperator::ErrorReturn => "unwrap",
	}
}

/// Returns the underlying binary operator for a compound assignment.
/// e.g. `+=` → `Plus`, `<<=` → `LeftShift`.
/// `=` (plain assign) returns `None`.
pub fn assign_op_to_binary(op: AssignOperator) -> Option<BinaryOperator> {
	match op {
		AssignOperator::Assign => None,
		AssignOperator::PlusAssign => Some(BinaryOperator::Plus),
		AssignOperator::MinusAssign => Some(BinaryOperator::Minus),
		AssignOperator::TimesAssign => Some(BinaryOperator::Times),
		AssignOperator::DivideAssign => Some(BinaryOperator::Divide),
		AssignOperator::RemainderAssign => Some(BinaryOperator::Remainder),
		AssignOperator::PowerAssign => Some(BinaryOperator::Power),
		AssignOperator::LeftShiftAssign => Some(BinaryOperator::LeftShift),
		AssignOperator::RightShiftAssign => Some(BinaryOperator::RightShift),
		AssignOperator::BitAndAssign => Some(BinaryOperator::BitAnd),
		AssignOperator::BitXorAssign => Some(BinaryOperator::BitXor),
		AssignOperator::BitOrAssign => Some(BinaryOperator::BitOr),
		AssignOperator::BitNotAssign => None, // ~= has no binary form; handled specially
		AssignOperator::BoolAndAssign => Some(BinaryOperator::BoolAnd),
		AssignOperator::BoolOrAssign => Some(BinaryOperator::BoolOr),
	}
}
