use std::fmt::{Display, Formatter, Result};

use super::{
	declaration::*,
	expr::*,
	ops::*,
	types::*,
};

// ============================================================================
// Expr Display
// ============================================================================

impl Display for Expr {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			Expr::Int(n) => write!(f, "int({})", n.inner()),
			Expr::Float(n) => write!(f, "float({})", n.inner()),
			Expr::Char(c) => write!(f, "char({:?})", c.inner()),
			Expr::String(parts) => {
				write!(f, "str(")?;
				for (i, part) in parts.iter().enumerate() {
					if i > 0 {
						write!(f, " + ")?;
					}
					match &part.inner() {
						StringPart::Text(s) => write!(f, "{:?}", s)?,
						StringPart::EscapeSequence(esc) => write!(f, "{}", esc)?,
						StringPart::InterpolatedExpr(expr) => write!(f, "${{{}}}", expr.inner())?,
					}
				}
				write!(f, ")")
			}
			Expr::Boolean(b) => write!(f, "bool({})", b.inner()),
			Expr::Identifier(id) => write!(f, "id({})", id.inner()),
			Expr::List(items) => {
				write!(f, "#[")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &item.inner() {
						ListItem::Expr(e) => write!(f, "{}", e.inner())?,
						ListItem::Spread(e) => write!(f, "...{}", e.inner())?,
					}
				}
				write!(f, "]")
			}
			Expr::Tuple(items) => {
				write!(f, "#(")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &item.inner() {
						ListItem::Expr(e) => write!(f, "{}", e.inner())?,
						ListItem::Spread(e) => write!(f, "...{}", e.inner())?,
					}
				}
				write!(f, ")")
			}
			Expr::Map(entries) => {
				write!(f, "#{{ ")?;
				for (i, entry) in entries.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &entry.inner() {
						MapEntry::Expr(k, v) => write!(f, "{}: {}", k.inner(), v.inner())?,
						MapEntry::Spread(e) => write!(f, "...{}", e.inner())?,
					}
				}
				write!(f, " }}")
			}
			Expr::Range(kind) => {
				match kind {
					RangeKind::From(e) => write!(f, "range({}..)", e.inner()),
					RangeKind::To(e) => write!(f, "range(..{})", e.inner()),
					RangeKind::Exclusive { min, max } => {
						write!(f, "range({}..{})", min.inner(), max.inner())
					}
					RangeKind::ToInclusive(e) => write!(f, "range(..={})", e.inner()),
					RangeKind::Inclusive { min, max } => {
						write!(f, "range({}..={})", min.inner(), max.inner())
					}
				}
			}
			Expr::Call { func, generics, args } => {
				write!(f, "call({}", func.inner())?;
				if !generics.is_empty() {
					write!(f, "<")?;
					for (i, g) in generics.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", g.inner().value.inner())?;
					}
					write!(f, ">")?;
				}
				write!(f, "(")?;
				for (i, arg) in args.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					if let Some(name) = &arg.inner().name {
						write!(f, "{}=", name.inner())?;
					}
					if arg.inner().spread {
						write!(f, "...")?;
					}
					write!(f, "{}", arg.inner().value.inner())?;
				}
				write!(f, "))")
			}
			Expr::MemberAccess { parent, member, optional } => {
				write!(f, "access({}{}{})", parent.inner(), if *optional { "?." } else { "." }, member.inner())
			}
			Expr::IndexAccess { parent, index, optional } => {
				write!(f, "index({}{}[{}])", parent.inner(), if *optional { "?" } else { "" }, index.inner())
			}
			Expr::Closure { params, generics, return_type, body } => {
				write!(f, "λ(")?;
				for (i, param) in params.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					let p = &param.inner();
					if p.mutable {
						write!(f, "mut ")?;
					}
					if p.spread {
						write!(f, "...")?;
					}
					write!(f, "{}", p.name.inner())?;
					if let Some(ty) = &p.type_ {
						write!(f, ": {}", ty.inner())?;
					}
				}
				write!(f, ")")?;
				if !generics.is_empty() {
					write!(f, "<")?;
					for (i, g) in generics.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", g.inner().name.inner())?;
					}
					write!(f, ">")?;
				}
				if let Some(ret) = return_type {
					write!(f, " -> {}", ret.inner())?;
				}
				write!(f, " => {})", body.inner())
			}
			Expr::PrefixOp { op, value } => {
				write!(f, "{}({})", op, value.inner())
			}
			Expr::PostfixOp { op, value } => {
				write!(f, "{}({})", op, value.inner())
			}
			Expr::BinaryOp { lhs, op, rhs } => {
				write!(f, "{}({}, {})", op, lhs.inner(), rhs.inner())
			}
			Expr::TypeOp { lhs, op, rhs } => {
				write!(f, "{}({}, {})", op, lhs.inner(), rhs.inner())
			}
			Expr::PatternOp { lhs, op, rhs } => {
				write!(f, "{}({}, {})", op, lhs.inner(), rhs.inner())
			}
			Expr::AssignOp { lhs, op, rhs } => {
				write!(f, "{}({}, {})", op, lhs.inner(), rhs.inner())
			}
			Expr::Return { value, label } => {
				write!(f, "return")?;
				if let Some(lbl) = label {
					write!(f, " @{}", lbl.inner())?;
				}
				if let Some(val) = value {
					write!(f, "({})", val.inner())?;
				} else {
					write!(f, "()")?;
				}
				Ok(())
			}
			Expr::Break { value, label } => {
				write!(f, "break")?;
				if let Some(lbl) = label {
					write!(f, " @{}", lbl.inner())?;
				}
				if let Some(val) = value {
					write!(f, "({})", val.inner())?;
				} else {
					write!(f, "()")?;
				}
				Ok(())
			}
			Expr::Continue { label } => {
				write!(f, "continue")?;
				if let Some(lbl) = label {
					write!(f, " @{}", lbl.inner())?;
				}
				Ok(())
			}
			Expr::For { variable, iterable, body, label } => {
				write!(f, "for")?;
				if let Some(lbl) = label {
					write!(f, " @{}", lbl.inner())?;
				}
				write!(f, "({} in {}) {}", variable.inner(), iterable.inner(), body.inner())
			}
			Expr::While { condition, body, label } => {
				write!(f, "while")?;
				if let Some(lbl) = label {
					write!(f, " @{}", lbl.inner())?;
				}
				write!(f, "({}) {}", condition.inner(), body.inner())
			}
			Expr::If { condition, then, otherwise } => {
				write!(f, "if({}) {} ", condition.inner(), then.inner())?;
				if let Some(els) = otherwise {
					write!(f, "else {}", els.inner())?;
				}
				Ok(())
			}
			Expr::Match { value, arms } => {
				write!(f, "match({}) {{ ", value.inner())?;
				for (i, arm) in arms.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", arm.pattern.inner())?;
					if let Some(guard) = &arm.guard {
						write!(f, " if {}", guard.inner())?;
					}
					write!(f, " => {}", arm.body.inner())?;
				}
				write!(f, " }}")
			}
			Expr::This => write!(f, "this"),
			Expr::Placeholder => write!(f, "_"),
			Expr::Block { body, label } => {
				write!(f, "block")?;
				if let Some(lbl) = label {
					write!(f, " @{}", lbl.inner())?;
				}
				write!(f, " {{ ")?;
				for (i, stmt) in body.iter().enumerate() {
					if i > 0 {
						write!(f, "; ")?;
					}
					write!(f, "{}", stmt.inner())?;
				}
				write!(f, " }}")
			}
			Expr::Grouped(inner) => write!(f, "({})", inner.inner()),
		}
	}
}

impl Display for Statement {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			Statement::Expr(expr) => write!(f, "{}", expr.inner()),
			Statement::Let { meta, value } => {
				write!(f, "let")?;
				if meta.mutable {
					write!(f, " mut")?;
				}
				write!(f, " {} = {}", meta.name.inner(), value.inner())
			}
		}
	}
}

impl Display for Pattern {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			Pattern::Int(n) => write!(f, "{}", n.inner()),
			Pattern::Float(fl) => write!(f, "{}", fl.inner()),
			Pattern::Char(c) => write!(f, "{:?}", c.inner()),
			Pattern::String(parts) => {
				write!(f, "\"")?;
				for part in parts.iter() {
					match &part.inner() {
						StringPatternPart::Text(s) => write!(f, "{}", s)?,
						StringPatternPart::EscapeSequence(esc) => write!(f, "{}", esc)?,
					}
				}
				write!(f, "\"")
			}
			Pattern::Boolean(b) => write!(f, "{}", b.inner()),
			Pattern::Binding { name, inner } => {
				write!(f, "{}@{}", name.inner(), inner.inner())
			}
			Pattern::List(items) => {
				write!(f, "#[")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &item.inner() {
						ListPatternEntry::Item(pat) => write!(f, "{}", pat.inner())?,
						ListPatternEntry::Rest(name) => {
							write!(f, "...")?;
							if let Some(n) = name {
								write!(f, "{}", n.inner())?;
							}
						}
					}
				}
				write!(f, "]")
			}
			Pattern::Tuple(items) => {
				write!(f, "#(")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &item.inner() {
						ListPatternEntry::Item(pat) => write!(f, "{}", pat.inner())?,
						ListPatternEntry::Rest(name) => {
							write!(f, "...")?;
							if let Some(n) = name {
								write!(f, "{}", n.inner())?;
							}
						}
					}
				}
				write!(f, ")")
			}
			Pattern::Map(entries) => {
				write!(f, "#{{ ")?;
				for (i, entry) in entries.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &entry.inner() {
						MapPatternEntry::Entry(k, v) => {
							write!(f, "{}: {}", k.inner(), v.inner())?
						}
						MapPatternEntry::Rest(name) => {
							write!(f, "...")?;
							if let Some(n) = name {
								write!(f, "{}", n.inner())?;
							}
						}
					}
				}
				write!(f, " }}")
			}
			Pattern::Range(kind) => match kind {
				RangePatternKind::ExclusiveMin(e) => write!(f, "{}..)", e.inner()),
				RangePatternKind::ExclusiveBoth { min, max } => {
					write!(f, "{}..{}", min.inner(), max.inner())
				}
				RangePatternKind::InclusiveMax(e) => write!(f, "..={}", e.inner()),
				RangePatternKind::InclusiveBoth { min, max } => {
					write!(f, "{}..={}", min.inner(), max.inner())
				}
			},
			Pattern::Struct { path, fields } => {
				write!(f, "struct(")?;
				for (i, seg) in path.iter().enumerate() {
					if i > 0 {
						write!(f, "::")?;
					}
					write!(f, "{}", seg.inner())?;
				}
				write!(f, " {{ ")?;
				for (i, field) in fields.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &field.inner() {
						StructPatternField::Value { name, value } => {
							write!(f, "{}: {}", name.inner(), value.inner())?
						}
						StructPatternField::Named(name) => write!(f, "{}", name.inner())?,
						StructPatternField::Rest => write!(f, "..")?,
					}
				}
				write!(f, " }})")
			}
			Pattern::Placeholder => write!(f, "_"),
			Pattern::Union(left, right) => {
				write!(f, "{}|{}", left.inner(), right.inner())
			}
			Pattern::Grouped(inner) => write!(f, "({})", inner.inner()),
		}
	}
}

// ============================================================================
// Type Display
// ============================================================================

impl Display for Type {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			Type::Int => write!(f, "int"),
			Type::Float => write!(f, "float"),
			Type::Char => write!(f, "char"),
			Type::String => write!(f, "string"),
			Type::Boolean => write!(f, "bool"),
			Type::Void => write!(f, "void"),
			Type::Never => write!(f, "never"),
			Type::Self_ => write!(f, "self"),
			Type::Infer => write!(f, "_"),
			Type::Intersection(left, right) => {
				write!(f, "({} + {})", left.inner(), right.inner())
			}
			Type::List(inner) => write!(f, "#[{}]", inner.inner()),
			Type::Tuple(items) => {
				write!(f, "#(")?;
				for (i, ty) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", ty.inner())?;
				}
				write!(f, ")")
			}
			Type::Map(k, v) => write!(f, "#{{{}:{}}}", k.inner(), v.inner()),
			Type::Function { params, return_type } => {
				write!(f, "(")?;
				for (i, (name, ty)) in params.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					if let Some(n) = name {
						write!(f, "{}:", n.inner())?;
					}
					write!(f, "{}", ty.inner())?;
				}
				write!(f, ") -> {}", return_type.inner())
			}
			Type::Reference { name, generics } => {
				write!(f, "{}", name.inner())?;
				if !generics.is_empty() {
					write!(f, "<")?;
					for (i, g) in generics.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", &g.inner().value.inner())?;
					}
					write!(f, ">")?;
				}
				Ok(())
			}
			Type::Grouped(inner) => write!(f, "({})", inner.inner()),
		}
	}
}

// ============================================================================
// Operator Display
// ============================================================================

impl Display for PrefixOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			PrefixOperator::BoolNot => write!(f, "!"),
			PrefixOperator::Negate => write!(f, "-"),
			PrefixOperator::BitNot => write!(f, "~"),
		}
	}
}

impl Display for PostfixOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			PostfixOperator::ErrorReturn => write!(f, "?"),
		}
	}
}

impl Display for BinaryOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			BinaryOperator::Plus => write!(f, "+"),
			BinaryOperator::Minus => write!(f, "-"),
			BinaryOperator::Times => write!(f, "*"),
			BinaryOperator::Divide => write!(f, "/"),
			BinaryOperator::Remainder => write!(f, "%"),
			BinaryOperator::Power => write!(f, "**"),
			BinaryOperator::BitAnd => write!(f, "&"),
			BinaryOperator::BitOr => write!(f, "|"),
			BinaryOperator::BitXor => write!(f, "^"),
			BinaryOperator::LeftShift => write!(f, "<<"),
			BinaryOperator::RightShift => write!(f, ">>"),
			BinaryOperator::Equals => write!(f, "=="),
			BinaryOperator::NotEquals => write!(f, "!="),
			BinaryOperator::LessThan => write!(f, "<"),
			BinaryOperator::LessThanEquals => write!(f, "<="),
			BinaryOperator::GreaterThan => write!(f, ">"),
			BinaryOperator::GreaterThanEquals => write!(f, ">="),
			BinaryOperator::In => write!(f, "in"),
			BinaryOperator::NotIn => write!(f, "!in"),
			BinaryOperator::BoolAnd => write!(f, "&&"),
			BinaryOperator::BoolOr => write!(f, "||"),
			BinaryOperator::Pipe => write!(f, "|>"),
			BinaryOperator::Unwrap => write!(f, "??"),
		}
	}
}

impl Display for TypeOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			TypeOperator::As => write!(f, "as"),
		}
	}
}

impl Display for PatternOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			PatternOperator::Is => write!(f, "is"),
			PatternOperator::NotIs => write!(f, "!is"),
		}
	}
}

impl Display for AssignOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			AssignOperator::Assign => write!(f, "="),
			AssignOperator::PlusAssign => write!(f, "+="),
			AssignOperator::MinusAssign => write!(f, "-="),
			AssignOperator::TimesAssign => write!(f, "*="),
			AssignOperator::DivideAssign => write!(f, "/="),
			AssignOperator::RemainderAssign => write!(f, "%="),
			AssignOperator::PowerAssign => write!(f, "**="),
			AssignOperator::LeftShiftAssign => write!(f, "<<="),
			AssignOperator::RightShiftAssign => write!(f, ">>="),
			AssignOperator::BitAndAssign => write!(f, "&="),
			AssignOperator::BitXorAssign => write!(f, "^="),
			AssignOperator::BitOrAssign => write!(f, "|="),
			AssignOperator::BitNotAssign => write!(f, "~="),
			AssignOperator::BoolAndAssign => write!(f, "&&="),
			AssignOperator::BoolOrAssign => write!(f, "||="),
		}
	}
}

// ============================================================================
// Module Display
// ============================================================================

impl Display for Module {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		write!(f, "module({}) [", self.path)?;
		for (i, member) in self.members.iter().enumerate() {
			if i > 0 {
				write!(f, ";")?;
			}
			write!(f, "\n\t{}", member)?;
		}
		write!(f, "\n]")
	}
}

// ============================================================================
// Declaration Display
// ============================================================================

impl Display for Declaration {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			Declaration::Import { root, path, idents } => {
				write!(f, "import ")?;
				match root {
					ImportRoot::Package(p) => write!(f, "{}", p.inner())?,
					ImportRoot::Project => write!(f, "^")?,
					ImportRoot::Current => write!(f, ".")?,
					ImportRoot::Parent => write!(f, "..")?,
				}
				for seg in path {
					write!(f, "/{}", seg.inner())?;
				}
				if let Some(list) = idents {
					write!(f, " with (")?;
					for (i, (name, alias)) in list.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", name.inner())?;
						if let Some(a) = alias {
							write!(f, " as {}", a.inner())?;
						}
					}
					write!(f, ")")?;
				}
				Ok(())
			}
			Declaration::Let { visibility, meta, value } => {
				write_visibility(f, visibility)?;
				write!(f, "let")?;
				if meta.mutable {
					write!(f, " mut")?;
				}
				write!(f, " {} = {}", meta.name.inner(), value.inner())
			}
			Declaration::ExternalLet(visibility, meta) => {
				write_visibility(f, visibility)?;
				write!(f, "external let")?;
				if meta.mutable {
					write!(f, " mut")?;
				}
				write!(f, " {}", meta.name.inner())
			}
			Declaration::Func { visibility, meta, body } => {
				write_visibility(f, visibility)?;
				write!(f, "func {}(", meta.name.inner())?;
				for (i, param) in meta.params.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					let p = &param.inner();
					if p.mutable {
						write!(f, "mut ")?;
					}
					if p.spread {
						write!(f, "...")?;
					}
					write!(f, "{}: {}", p.name.inner(), p.type_.inner())?;
				}
				write!(f, ")")?;
				if !meta.generics.is_empty() {
					write!(f, "<")?;
					for (i, g) in meta.generics.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", g.inner().name.inner())?;
					}
					write!(f, ">")?;
				}
				if let Some(ret) = &meta.return_type {
					write!(f, " -> {}", ret.inner())?;
				}
				write!(f, " = {}", body.inner())
			}
			Declaration::ExternalFunc(visibility, meta) => {
				write_visibility(f, visibility)?;
				write!(f, "external func {}(", meta.name.inner())?;
				for (i, param) in meta.params.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					let p = &param.inner();
					write!(f, "{}: {}", p.name.inner(), p.type_.inner())?;
				}
				write!(f, ")")?;
				if let Some(ret) = &meta.return_type {
					write!(f, " -> {}", ret.inner())?;
				}
				Ok(())
			}
			Declaration::TypeAlias { visibility, meta, value } => {
				write_visibility(f, visibility)?;
				write!(f, "type {}(", meta.name.inner())?;
				for (i, g) in meta.generics.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", g.inner().name.inner())?;
				}
				write!(f, ") = {}", value.inner())
			}
			Declaration::Struct { visibility, name, generics, fields, members } => {
				write_visibility(f, visibility)?;
				write!(f, "struct {}(", name.inner())?;
				for (i, g) in generics.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", g.inner().name.inner())?;
				}
				write!(f, ") {{ fields: ")?;
				for (i, field) in fields.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					let f_ = &field.inner();
					write!(f, "{}: {}", f_.name.inner(), f_.type_.inner())?;
				}
				write!(f, ", members: [{}] }}", members.len())
			}
			Declaration::Enum { visibility, name, generics, variants, members } => {
				write_visibility(f, visibility)?;
				write!(f, "enum {}(", name.inner())?;
				for (i, g) in generics.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", g.inner().name.inner())?;
				}
				write!(f, ") {{ variants: ")?;
				for (i, var) in variants.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", var.inner().name.inner())?;
				}
				write!(f, ", members: [{}] }}", members.len())
			}
			Declaration::Namespace { visibility, name, members } => {
				write_visibility(f, visibility)?;
				write!(f, "namespace {} [{}]", name.inner(), members.len())
			}
			Declaration::Interface { visibility, name, generics, super_interfaces, members } => {
				write_visibility(f, visibility)?;
				write!(f, "interface {}(", name.inner())?;
				for (i, g) in generics.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", g.inner().name.inner())?;
				}
				write!(f, ")")?;
				if !super_interfaces.is_empty() {
					write!(f, " extends ")?;
					for (i, super_if) in super_interfaces.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						let (name, args) = &super_if.inner();
						write!(f, "{}", name.inner())?;
						if !args.is_empty() {
							write!(f, "<")?;
							for (j, a) in args.iter().enumerate() {
								if j > 0 {
									write!(f, ", ")?;
								}
								write!(f, "{}", &a.inner().value.inner())?;
							}
							write!(f, ">")?;
						}
					}
				}
				write!(f, " [{}]", members.len())
			}
			Declaration::Impl { visibility, generics, mutable, type_, members } => {
				write_visibility(f, visibility)?;
				if *mutable {
					write!(f, "impl mut")?;
				} else {
					write!(f, "impl")?;
				}
				if !generics.is_empty() {
					write!(f, "<")?;
					for (i, g) in generics.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", g.inner().name.inner())?;
					}
					write!(f, ">")?;
				}
				write!(f, " {} [{}]", type_.inner(), members.len())
			}
			Declaration::ImplFor {
				visibility,
				generics,
				mutable,
				type_,
				for_interface,
				members,
			} => {
				write_visibility(f, visibility)?;
				if *mutable {
					write!(f, "impl mut")?;
				} else {
					write!(f, "impl")?;
				}
				if !generics.is_empty() {
					write!(f, "<")?;
					for (i, g) in generics.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", g.inner().name.inner())?;
					}
					write!(f, ">")?;
				}
				write!(f, " {} for {}", type_.inner(), for_interface.0.inner())?;
				if !for_interface.1.is_empty() {
					write!(f, "<")?;
					for (i, a) in for_interface.1.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{}", &a.inner().value.inner())?;
					}
					write!(f, ">")?;
				}
				write!(f, " [{}]", members.len())
			}
		}
	}
}

fn write_visibility(f: &mut Formatter<'_>, vis: &Option<Visibility>) -> Result {
	if let Some(v) = vis {
		match v {
			Visibility::Public => write!(f, "pub "),
			Visibility::Internal => write!(f, "internal "),
			Visibility::Private => write!(f, "private "),
		}
	} else {
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ast::Spanned;
	use crate::ast::Span;

	fn span() -> Span {
		Span::new(0, 0)
	}

	#[test]
	fn test_expr_display() {
		let expr = Expr::Int(Spanned(42, span()));
		assert_eq!(expr.to_string(), "int(42)");

		let expr = Expr::Boolean(Spanned(true, span()));
		assert_eq!(expr.to_string(), "bool(true)");

		let expr = Expr::This;
		assert_eq!(expr.to_string(), "this");

		let expr = Expr::Placeholder;
		assert_eq!(expr.to_string(), "_");
	}

	#[test]
	fn test_type_display() {
		let ty = Type::Int;
		assert_eq!(ty.to_string(), "int");

		let ty = Type::Float;
		assert_eq!(ty.to_string(), "float");

		let ty = Type::String;
		assert_eq!(ty.to_string(), "string");

		let ty = Type::Boolean;
		assert_eq!(ty.to_string(), "bool");

		let ty = Type::Void;
		assert_eq!(ty.to_string(), "void");

		let ty = Type::Never;
		assert_eq!(ty.to_string(), "never");
	}

	#[test]
	fn test_operator_display() {
		assert_eq!(BinaryOperator::Plus.to_string(), "+");
		assert_eq!(BinaryOperator::Minus.to_string(), "-");
		assert_eq!(BinaryOperator::Times.to_string(), "*");
		assert_eq!(BinaryOperator::Divide.to_string(), "/");
		assert_eq!(BinaryOperator::BoolOr.to_string(), "||");
		assert_eq!(BinaryOperator::BoolAnd.to_string(), "&&");

		assert_eq!(PrefixOperator::BoolNot.to_string(), "!");
		assert_eq!(PrefixOperator::Negate.to_string(), "-");
		assert_eq!(PrefixOperator::BitNot.to_string(), "~");

		assert_eq!(AssignOperator::Assign.to_string(), "=");
		assert_eq!(AssignOperator::PlusAssign.to_string(), "+=");
		assert_eq!(AssignOperator::PowerAssign.to_string(), "**=");
	}

	#[test]
	fn test_statement_display() {
		use crate::ast::declaration::LetDeclaration;

		let stmt = Statement::Expr(Spanned(Expr::This, span()));
		assert_eq!(stmt.to_string(), "this");

		let let_decl = LetDeclaration {
			mutable: true,
			name: Spanned(Pattern::Placeholder, span()),
			type_: None,
		};
		let value = Spanned(Expr::Int(Spanned(0, span())), span());
		let stmt = Statement::Let { meta: let_decl, value };
		assert_eq!(stmt.to_string(), "let mut _ = int(0)");
	}

	#[test]
	fn test_pattern_display() {
		let pat = Pattern::Int(Spanned(123, span()));
		assert_eq!(pat.to_string(), "123");

		let pat = Pattern::Boolean(Spanned(false, span()));
		assert_eq!(pat.to_string(), "false");

		let pat = Pattern::Placeholder;
		assert_eq!(pat.to_string(), "_");
	}
}
