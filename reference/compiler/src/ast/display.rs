use std::fmt::{Display, Formatter, Result};

use crate::ast::Spanned;

use super::{declaration::*, expr::*, ops::*, types::*};

// ============================================================================
// Expr Display
// ============================================================================

impl Display for Expr {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			Expr::Int(Spanned(n, _)) => write!(f, "int({n})"),
			Expr::Float(Spanned(n, _)) => write!(f, "float({n})"),
			Expr::Char(Spanned(c, _)) => write!(f, "char({c:?})"),
			Expr::String(parts) => {
				write!(f, "str(")?;
				for (i, part) in parts.iter().enumerate() {
					if i > 0 {
						write!(f, " + ")?;
					}
					match &part.inner() {
						StringPart::Text(s) => write!(f, "{s:?}")?,
						StringPart::EscapeSequence(esc) => write!(f, "{esc}")?,
						StringPart::InterpolatedExpr(Spanned(expr, _)) => write!(f, "${{{expr}}}")?,
					}
				}
				write!(f, ")")
			}
			Expr::Boolean(Spanned(b, _)) => write!(f, "{b}"),
			Expr::Identifier(Spanned(id, _)) => write!(f, "id({id})"),
			Expr::List(items) => {
				write!(f, "#[")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &item.inner() {
						ListItem::Expr(Spanned(e, _)) => write!(f, "{e}")?,
						ListItem::Spread(Spanned(e, _)) => write!(f, "...{e}")?,
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
						ListItem::Expr(Spanned(e, _)) => write!(f, "{e}")?,
						ListItem::Spread(Spanned(e, _)) => write!(f, "...{e}")?,
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
						MapEntry::Expr(Spanned(k, _), Spanned(v, _)) => write!(f, "{k}: {v}")?,
						MapEntry::Spread(Spanned(e, _)) => write!(f, "...{e}")?,
					}
				}
				write!(f, " }}")
			}
			Expr::Range(kind) => match kind {
				RangeKind::From(e) => write!(f, "{}..", e.inner()),
				RangeKind::To(e) => write!(f, "..<{}", e.inner()),
				RangeKind::Exclusive { min, max } => {
					write!(f, "{}..<{}", min.inner(), max.inner())
				}
				RangeKind::ToInclusive(e) => write!(f, "..={}", e.inner()),
				RangeKind::Inclusive { min, max } => {
					write!(f, "{}..={}", min.inner(), max.inner())
				}
			},
			Expr::Call {
				func,
				generics,
				args,
			} => {
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
			Expr::MemberAccess {
				parent,
				member,
				optional,
			} => {
				write!(
					f,
					"access({}{}{})",
					parent.inner(),
					if *optional { "?." } else { "." },
					member.inner()
				)
			}
			Expr::IndexAccess {
				parent,
				index,
				optional,
			} => {
				write!(
					f,
					"index({}{}[{}])",
					parent.inner(),
					if *optional { "?" } else { "" },
					index.inner()
				)
			}
			Expr::Closure {
				params,
				generics,
				return_type,
				body,
			} => {
				write!(f, "(")?;
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
					write!(f, " : {}", ret.inner())?;
				}
				write!(f, " -> {})", body.inner())
			}
			Expr::PrefixOp { op, value } => {
				write!(f, "{op}({})", value.inner())
			}
			Expr::PostfixOp { op, value } => {
				write!(f, "{op}({})", value.inner())
			}
			Expr::BinaryOp { lhs, op, rhs } => {
				write!(f, "{op}({}, {})", lhs.inner(), rhs.inner())
			}
			Expr::TypeOp { lhs, op, rhs } => {
				write!(f, "{op}({}, {})", lhs.inner(), rhs.inner())
			}
			Expr::PatternOp { lhs, op, rhs } => {
				write!(f, "{op}({}, {})", lhs.inner(), rhs.inner())
			}
			Expr::AssignOp { lhs, op, rhs } => {
				write!(f, "{op}({}, {})", lhs.inner(), rhs.inner())
			}
			Expr::Return { value, label } => {
				write!(f, "return")?;
				if let Some(Spanned(label, _)) = label {
					write!(f, " @{label}")?;
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
				if let Some(Spanned(label, _)) = label {
					write!(f, " @{label}")?;
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
				if let Some(Spanned(label, _)) = label {
					write!(f, " @{label}")?;
				}
				Ok(())
			}
			Expr::While {
				condition,
				body,
				label,
			} => {
				write!(f, "while")?;
				if let Some(Spanned(label, _)) = label {
					write!(f, " @{label}")?;
				}
				write!(f, "({}) {}", condition.inner(), body.inner())
			}
			Expr::For {
				variable,
				iterable,
				body,
				label,
			} => {
				write!(f, "for")?;
				if let Some(Spanned(label, _)) = label {
					write!(f, " @{label}")?;
				}
				write!(
					f,
					"({} in {}) {}",
					variable.inner(),
					iterable.inner(),
					body.inner()
				)
			}
			Expr::If {
				condition,
				then,
				otherwise,
			} => {
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
				if let Some(Spanned(label, _)) = label {
					write!(f, " @{label}")?;
				}
				write!(f, " {{ ")?;
				for (i, Spanned(stmt, _)) in body.iter().enumerate() {
					if i > 0 {
						write!(f, "; ")?;
					}
					write!(f, "{stmt}")?;
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
			Statement::Expr(Spanned(expr, _)) => write!(f, "{expr}"),
			Statement::Let {
				meta,
				value: Spanned(value, _),
			} => {
				write!(f, "let")?;
				if meta.mutable {
					write!(f, " mut")?;
				}
				write!(f, " {} = {value}", meta.name.inner())
			}
		}
	}
}

impl Display for Pattern {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		match self {
			Pattern::Int(Spanned(n, _)) => write!(f, "{n}"),
			Pattern::Float(Spanned(fl, _)) => write!(f, "{fl}"),
			Pattern::Char(Spanned(c, _)) => write!(f, "{c:?}"),
			Pattern::String(parts) => {
				write!(f, "\"")?;
				for part in parts.iter() {
					match &part.inner() {
						StringPatternPart::Text(s) => write!(f, "{s}")?,
						StringPatternPart::EscapeSequence(esc) => write!(f, "{esc}")?,
					}
				}
				write!(f, "\"")
			}
			Pattern::Boolean(Spanned(b, _)) => write!(f, "{b}"),
			Pattern::Binding {
				name: Spanned(name, _),
				inner,
			} => {
				write!(f, "{name} @ {}", inner.inner())
			}
			Pattern::List(items) => {
				write!(f, "#[")?;
				for (i, item) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &item.inner() {
						ListPatternEntry::Item(Spanned(pat, _)) => write!(f, "{pat}")?,
						ListPatternEntry::Rest(name) => {
							write!(f, "...")?;
							if let Some(Spanned(name, _)) = name {
								write!(f, "{name}")?;
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
						ListPatternEntry::Item(Spanned(pat, _)) => write!(f, "{pat}")?,
						ListPatternEntry::Rest(name) => {
							write!(f, "...")?;
							if let Some(Spanned(name, _)) = name {
								write!(f, "{name}")?;
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
						MapPatternEntry::Entry(Spanned(k, _), Spanned(v, _)) => write!(f, "{k}: {v}")?,
						MapPatternEntry::Rest(name) => {
							write!(f, "...")?;
							if let Some(Spanned(name, _)) = name {
								write!(f, "{name}")?;
							}
						}
					}
				}
				write!(f, " }}")
			}
			Pattern::Range(kind) => match kind {
				RangePatternKind::ExclusiveMin(e) => write!(f, "{}..", e.inner()),
				RangePatternKind::ExclusiveBoth { min, max } => {
					write!(f, "{}..<{}", min.inner(), max.inner())
				}
				RangePatternKind::InclusiveMax(e) => write!(f, "..={}", e.inner()),
				RangePatternKind::InclusiveBoth { min, max } => {
					write!(f, "{}..={}", min.inner(), max.inner())
				}
			},
			Pattern::Struct { path, fields } => {
				write!(f, "struct(")?;
				for (i, Spanned(seg, _)) in path.iter().enumerate() {
					if i > 0 {
						write!(f, "::")?;
					}
					write!(f, "{seg}")?;
				}
				write!(f, " {{ ")?;
				for (i, field) in fields.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					match &field.inner() {
						StructPatternField::Value {
							name: Spanned(name, _),
							value: Spanned(value, _),
						} => write!(f, "{name}: {value}")?,
						StructPatternField::Named(Spanned(name, _)) => write!(f, "{name}")?,
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
				for (i, Spanned(ty, _)) in items.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{ty}")?;
				}
				write!(f, ")")
			}
			Type::Map(k, v) => write!(f, "#{{{}:{}}}", k.inner(), v.inner()),
			Type::Function {
				params,
				return_type,
			} => {
				write!(f, "(")?;
				for (i, (name, Spanned(ty, _))) in params.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					if let Some(Spanned(name, _)) = name {
						write!(f, "{name}: ")?;
					}
					write!(f, "{ty}")?;
				}
				write!(f, ") -> {}", return_type.inner())
			}
			Type::Reference {
				name: Spanned(name, _),
				generics,
			} => {
				write!(f, "{name}")?;
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
		write!(
			f,
			"{}",
			match self {
				PrefixOperator::BoolNot => "!",
				PrefixOperator::Negate => "-",
				PrefixOperator::BitNot => "~",
			}
		)
	}
}

impl Display for PostfixOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		write!(
			f,
			"{}",
			match self {
				PostfixOperator::ErrorReturn => "?",
			}
		)
	}
}

impl Display for BinaryOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		write!(
			f,
			"{}",
			match self {
				BinaryOperator::Plus => "+",
				BinaryOperator::Minus => "-",
				BinaryOperator::Times => "*",
				BinaryOperator::Divide => "/",
				BinaryOperator::Remainder => "%",
				BinaryOperator::Power => "**",
				BinaryOperator::BitAnd => "&",
				BinaryOperator::BitOr => "|",
				BinaryOperator::BitXor => "^",
				BinaryOperator::LeftShift => "<<",
				BinaryOperator::RightShift => ">>",
				BinaryOperator::Equals => "==",
				BinaryOperator::NotEquals => "!=",
				BinaryOperator::LessThan => "<",
				BinaryOperator::LessThanEquals => "<=",
				BinaryOperator::GreaterThan => ">",
				BinaryOperator::GreaterThanEquals => ">=",
				BinaryOperator::In => "in",
				BinaryOperator::NotIn => "!in",
				BinaryOperator::BoolAnd => "&&",
				BinaryOperator::BoolOr => "||",
				BinaryOperator::Pipe => "|>",
				BinaryOperator::Unwrap => "??",
			}
		)
	}
}

impl Display for TypeOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		write!(
			f,
			"{}",
			match self {
				TypeOperator::As => "as",
			}
		)
	}
}

impl Display for PatternOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		write!(
			f,
			"{}",
			match self {
				PatternOperator::Is => "is",
				PatternOperator::NotIs => "!is",
			}
		)
	}
}

impl Display for AssignOperator {
	fn fmt(&self, f: &mut Formatter<'_>) -> Result {
		write!(
			f,
			"{}",
			match self {
				AssignOperator::Assign => "=",
				AssignOperator::PlusAssign => "+=",
				AssignOperator::MinusAssign => "-=",
				AssignOperator::TimesAssign => "*=",
				AssignOperator::DivideAssign => "/=",
				AssignOperator::RemainderAssign => "%=",
				AssignOperator::PowerAssign => "**=",
				AssignOperator::LeftShiftAssign => "<<=",
				AssignOperator::RightShiftAssign => ">>=",
				AssignOperator::BitAndAssign => "&=",
				AssignOperator::BitXorAssign => "^=",
				AssignOperator::BitOrAssign => "|=",
				AssignOperator::BitNotAssign => "~=",
				AssignOperator::BoolAndAssign => "&&=",
				AssignOperator::BoolOrAssign => "||=",
			}
		)
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
			write!(f, "\n\t{member}")?;
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
					ImportRoot::Package(Spanned(p, _)) => write!(f, "{p}")?,
					ImportRoot::Project => write!(f, "^")?,
					ImportRoot::Current => write!(f, ".")?,
					ImportRoot::Parent => write!(f, "..")?,
				}
				for Spanned(seg, _) in path {
					write!(f, "/{seg}")?;
				}
				if let Some(list) = idents {
					write!(f, " with (")?;
					for (i, (Spanned(name, _), alias)) in list.iter().enumerate() {
						if i > 0 {
							write!(f, ", ")?;
						}
						write!(f, "{name}")?;
						if let Some(Spanned(alias, _)) = alias {
							write!(f, " as {alias}")?;
						}
					}
					write!(f, ")")?;
				}
				Ok(())
			}
			Declaration::Let {
				visibility,
				meta,
				value,
			} => {
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
			Declaration::Func {
				visibility,
				meta,
				body: Spanned(body, _),
			} => {
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
				if let Some(Spanned(ret, _)) = &meta.return_type {
					write!(f, " -> {ret}")?;
				}
				write!(f, " = {body}")
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
				if let Some(Spanned(ret, _)) = &meta.return_type {
					write!(f, " -> {ret}")?;
				}
				Ok(())
			}
			Declaration::TypeAlias {
				visibility,
				meta,
				value: Spanned(value, _),
			} => {
				write_visibility(f, visibility)?;
				write!(f, "type {}(", meta.name.inner())?;
				for (i, g) in meta.generics.iter().enumerate() {
					if i > 0 {
						write!(f, ", ")?;
					}
					write!(f, "{}", g.inner().name.inner())?;
				}
				write!(f, ") = {value}")
			}
			Declaration::Struct {
				visibility,
				name: Spanned(name, _),
				generics,
				fields,
				members,
			} => {
				write_visibility(f, visibility)?;
				write!(f, "struct {name}(")?;
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
			Declaration::Enum {
				visibility,
				name,
				generics,
				variants,
				members,
			} => {
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
			Declaration::Namespace {
				visibility,
				name,
				members,
			} => {
				write_visibility(f, visibility)?;
				write!(f, "namespace {} [{}]", name.inner(), members.len())
			}
			Declaration::Interface {
				visibility,
				name,
				generics,
				super_interfaces,
				members,
			} => {
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
			Declaration::Impl {
				visibility,
				generics,
				mutable,
				type_,
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
	use crate::ast::Span;
	use crate::ast::Spanned;

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
		let stmt = Statement::Let {
			meta: let_decl,
			value,
		};
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
