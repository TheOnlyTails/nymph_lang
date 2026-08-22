//! Canonical, filesystem-independent formatting for Nymph source.
//!
//! Formatting is deliberately gated on the canonical parser. The concrete scanner
//! below only supplies the parser's missing lossless layer: spelling and comments.
//! It never attempts error recovery and therefore cannot rewrite malformed input.

use std::collections::{HashMap, HashSet};

use nymph_ast::{
	Span,
	decl::{Declaration, ImplMember, InterfaceElement, InterfaceMember, Module},
	expr::{Expr, ExprKind, ListItem, MapEntry, Statement, StringPart},
};
use nymph_diagnostics::Diagnostic;
use nymph_syntax::{parse_expression, parse_module};
use thiserror::Error;
use unicode_width::UnicodeWidthChar as _;

const WIDTH: usize = 100;

/// A formatting failure. Diagnostics retain their structured spans, labels and codes.
#[derive(Clone, Debug, Error)]
#[error("{diagnostic_count} syntax diagnostic(s) in {path}")]
pub struct FormatError {
	/// The logical source path supplied to [`format()`].
	pub path: String,
	/// Lexer and parser diagnostics. No formatted text is produced when this is nonempty.
	pub diagnostics: Vec<Diagnostic>,
	diagnostic_count: usize,
}

impl FormatError {
	fn syntax(path: &str, diagnostics: Vec<Diagnostic>) -> Self {
		Self {
			path: path.to_owned(),
			diagnostic_count: diagnostics.len(),
			diagnostics,
		}
	}
}

/// The source unit selected by [`format_range`] and its complete replacement text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedRange {
	pub range: Span,
	pub text: String,
}

/// Format one complete Nymph module.
pub fn format(source: &str, path: &str) -> Result<String, FormatError> {
	let parsed = parse_module(source, path);
	if !parsed.diagnostics.is_empty() {
		return Err(FormatError::syntax(path, parsed.diagnostics));
	}
	let hints = Hints::module(source, &parsed.tree);
	Ok(Formatter::new(source, hints).finish())
}

/// Format the smallest safely replaceable line/block unit containing `range`.
///
/// The selected unit is returned explicitly; callers must replace exactly that span.
/// A range wholly outside the source, or an empty selection at EOF, has no unit.
pub fn format_range(
	source: &str,
	path: &str,
	range: Span,
) -> Result<Option<FormattedRange>, FormatError> {
	let parsed = parse_module(source, path);
	if !parsed.diagnostics.is_empty() {
		return Err(FormatError::syntax(path, parsed.diagnostics));
	}
	if range.start > range.end
		|| range.start >= source.len()
		|| range.end > source.len()
		|| !source.is_char_boundary(range.start)
		|| !source.is_char_boundary(range.end)
	{
		return Ok(None);
	}
	let hints = Hints::module(source, &parsed.tree);
	if range.start == 0 && range.end >= source.len() {
		let formatted = Formatter::new(source, hints).finish();
		if formatted == source {
			return Ok(None);
		}
		return Ok(Some(FormattedRange {
			range: Span::new(0, source.len()),
			text: formatted,
		}));
	}
	let expression_unit = smallest_containing(&hints.removable_units, range)
		.or_else(|| smallest_containing(&hints.units, range));
	let Some(selected) = expression_unit.or_else(|| select_unit(source, range)) else {
		return Ok(None);
	};
	let fragment = &source[selected.start..selected.end];
	let mut candidate = format_fragment(fragment);
	if expression_unit.is_some() {
		candidate = candidate.trim_end_matches('\n').to_owned();
		let line_start = source[..selected.start]
			.rfind('\n')
			.map_or(0, |index| index + 1);
		let prefix = &source[line_start..selected.start];
		if prefix.chars().all(char::is_whitespace) && candidate.contains('\n') {
			candidate = indent_continuations(&candidate, prefix);
		}
	} else {
		let prefix_len = fragment
			.char_indices()
			.take_while(|(_, character)| matches!(character, ' ' | '\t'))
			.map(|(_, character)| character.len_utf8())
			.sum::<usize>();
		let prefix = &fragment[..prefix_len];
		if !prefix.is_empty() {
			candidate = prefix_lines(candidate.trim_start_matches([' ', '\t']), prefix);
		}
	}
	if candidate == fragment {
		return Ok(None);
	}
	Ok(Some(FormattedRange {
		range: selected,
		text: candidate,
	}))
}

fn smallest_containing(units: &[Span], requested: Span) -> Option<Span> {
	units
		.iter()
		.copied()
		.filter(|unit| unit.start <= requested.start && unit.end >= requested.end)
		.min_by_key(|unit| unit.end - unit.start)
}

fn indent_continuations(text: &str, prefix: &str) -> String {
	let mut output = String::with_capacity(text.len() + prefix.len() * text.matches('\n').count());
	for (index, line) in text.split_inclusive('\n').enumerate() {
		if index > 0 && !line.is_empty() {
			output.push_str(prefix);
		}
		output.push_str(line);
	}
	output
}

fn prefix_lines(text: &str, prefix: &str) -> String {
	let mut output = String::with_capacity(text.len() + prefix.len());
	for line in text.split_inclusive('\n') {
		if !line.is_empty() {
			output.push_str(prefix);
		}
		output.push_str(line);
	}
	output
}

fn format_fragment(source: &str) -> String {
	let parsed = parse_expression(source);
	let hints = if parsed.diagnostics.is_empty() {
		Hints::expression(source, &parsed.tree)
	} else {
		Hints::default()
	};
	Formatter::new(source, hints).finish_fragment()
}

#[derive(Default)]
struct Hints {
	line_before: HashSet<usize>,
	comma_after: HashSet<usize>,
	blocks: HashSet<usize>,
	matches: HashSet<usize>,
	remove_delimiters: HashMap<usize, usize>,
	grouped: HashSet<usize>,
	multiline_lists: HashMap<usize, usize>,
	units: Vec<Span>,
	removable_units: Vec<Span>,
	continuation_before: HashSet<usize>,
}

impl Hints {
	fn module(source: &str, module: &Module) -> Self {
		let mut hints = Self::default();
		for declaration in &module.members {
			hints.visit_declaration(source, declaration);
		}
		hints.analyze_lists(source);
		hints
	}

	fn expression(source: &str, expression: &Expr) -> Self {
		let mut hints = Self::default();
		hints.visit_expr(source, expression, true);
		hints.analyze_lists(source);
		hints
	}

	fn analyze_lists(&mut self, source: &str) {
		struct Candidate {
			open: usize,
			opener: &'static str,
			width: usize,
			has_comma: bool,
			has_line_comment: bool,
		}

		let mut scanner = Scanner::new(source);
		let mut stack: Vec<Candidate> = Vec::new();
		let mut line_width = 0;
		let mut depth: usize = 0;
		let mut declaration_prefix = false;
		while let Some(item) = scanner.next() {
			if item.kind != Kind::Space {
				if item.kind == Kind::Token && depth == 0 {
					if is_declaration_prefix(item.text) {
						if !declaration_prefix {
							line_width = 0;
						}
						declaration_prefix = true;
					} else if is_declaration_start(item.text) {
						if !declaration_prefix {
							line_width = 0;
						}
						declaration_prefix = false;
					}
				}
				if self.line_before.contains(&item.start) {
					line_width = 0;
				}
				let item_width = display_width(item.text);
				if line_width > 0 {
					line_width += 1;
				}
				line_width += item_width;
				for candidate in &mut stack {
					candidate.width += item_width + 1;
					candidate.has_line_comment |=
						item.kind == Kind::LineComment || item.kind == Kind::BlockComment && item.had_newline;
				}
			}
			if item.kind != Kind::Token {
				if item.kind == Kind::LineComment || item.kind == Kind::BlockComment && item.had_newline {
					line_width = item
						.text
						.rsplit(['\r', '\n'])
						.next()
						.map_or(0, display_width);
				}
				continue;
			}
			match item.text {
				"(" | "[" | "{" | "#(" | "#[" | "#{" => {
					stack.push(Candidate {
						open: item.start,
						opener: match item.text {
							"(" => "(",
							"[" => "[",
							"{" => "{",
							"#(" => "#(",
							"#[" => "#[",
							"#{" => "#{",
							_ => unreachable!(),
						},
						width: line_width,
						has_comma: false,
						has_line_comment: false,
					});
					depth += 1;
				}
				"," => {
					if let Some(candidate) = stack.last_mut() {
						candidate.has_comma = true;
					}
				}
				")" | "]" | "}" => {
					let Some(candidate) = stack.last() else {
						continue;
					};
					let matches = matches!(
						(candidate.opener, item.text),
						("(", ")") | ("#(", ")") | ("[", "]") | ("#[", "]") | ("{", "}") | ("#{", "}")
					);
					if !matches {
						continue;
					}
					let candidate = stack.pop().expect("candidate exists");
					depth = depth.saturating_sub(1);
					if candidate.opener != "{"
						&& candidate.has_comma
						&& (candidate.width > WIDTH || candidate.has_line_comment)
					{
						self.multiline_lists.insert(candidate.open, item.start);
					}
				}
				_ => {}
			}
		}
	}

	fn visit_declaration(&mut self, source: &str, declaration: &Declaration) {
		match declaration {
			Declaration::Let { value, .. } | Declaration::Func { body: value, .. } => {
				self.visit_expr(source, value, true);
			}
			Declaration::Struct {
				fields,
				members,
				impls,
				..
			} => {
				for field in fields {
					if let Some(default) = &field.0.default {
						self.visit_expr(source, default, true);
					}
				}
				for member in members {
					self.line_before.insert(member.1.start);
					self.visit_impl_member(source, &member.0);
				}
				for implementation in impls {
					self.line_before.insert(implementation.1.start);
					for member in &implementation.0.members {
						self.line_before.insert(member.1.start);
						self.visit_impl_member(source, &member.0);
					}
				}
			}
			Declaration::Enum {
				embeddings,
				variants,
				members,
				impls,
				..
			} => {
				for embedding in embeddings {
					self.line_before.insert(embedding.1.start);
					if !source[embedding.1.end..].trim_start().starts_with(',') {
						self.comma_after.insert(embedding.1.end);
					}
				}
				for variant in variants {
					self.line_before.insert(variant.1.start);
					if !source[variant.1.end..].trim_start().starts_with(',') {
						self.comma_after.insert(variant.1.end);
					}
					for field in &variant.0.fields {
						if let Some(default) = &field.0.default {
							self.visit_expr(source, default, true);
						}
					}
				}
				for member in members {
					self.line_before.insert(member.1.start);
					self.visit_impl_member(source, &member.0);
				}
				for implementation in impls {
					self.line_before.insert(implementation.1.start);
					for member in &implementation.0.members {
						self.line_before.insert(member.1.start);
						self.visit_impl_member(source, &member.0);
					}
				}
			}
			Declaration::Namespace { members, .. }
			| Declaration::Impl { members, .. }
			| Declaration::ImplFor { members, .. } => {
				for member in members {
					self.line_before.insert(member.1.start);
					self.visit_impl_member(source, &member.0);
				}
			}
			Declaration::Interface { members, .. } => {
				for member in members {
					self.line_before.insert(member.1.start);
					self.visit_interface_member(source, &member.0);
				}
			}
			Declaration::Import { .. }
			| Declaration::Effect { .. }
			| Declaration::ExternalLet(..)
			| Declaration::ExternalFunc(..)
			| Declaration::TypeAlias { .. } => {}
		}
	}

	fn visit_impl_member(&mut self, source: &str, member: &ImplMember) {
		match member {
			ImplMember::Let { value, .. } | ImplMember::Func { body: value, .. } => {
				self.visit_expr(source, value, true);
			}
			ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => {}
		}
	}

	fn visit_interface_member(&mut self, source: &str, member: &InterfaceMember) {
		match member {
			InterfaceMember::Element(element) => match &element.0 {
				InterfaceElement::Let { value, .. } => {
					if let Some(value) = value {
						self.visit_expr(source, value, true);
					}
				}
				InterfaceElement::Func { body, .. } => {
					if let Some(body) = body {
						self.visit_expr(source, body, true);
					}
				}
			},
			InterfaceMember::Impl { members, .. } => {
				for member in members {
					self.line_before.insert(member.1.start);
					self.visit_impl_member(source, &member.0);
				}
			}
		}
	}

	fn visit_expr(&mut self, source: &str, expr: &Expr, root: bool) {
		if matches!(
			&expr.kind,
			ExprKind::List(_)
				| ExprKind::Tuple(_)
				| ExprKind::Map(_)
				| ExprKind::Call { .. }
				| ExprKind::BinaryOp { .. }
				| ExprKind::Match { .. }
				| ExprKind::Block { .. }
				| ExprKind::Grouped(_)
		) {
			self.units.push(expr.span);
		}
		match &expr.kind {
			ExprKind::String(parts) => {
				for part in parts {
					if let StringPart::InterpolatedExpr(inner) = &part.0 {
						self.visit_expr(source, inner, true);
					}
				}
			}
			ExprKind::List(items) | ExprKind::Tuple(items) => {
				for item in items {
					match &item.0 {
						ListItem::Expr(value) | ListItem::Spread(value) => self.visit_expr(source, value, true),
					}
				}
			}
			ExprKind::Map(entries) => {
				for entry in entries {
					match &entry.0 {
						MapEntry::Entry(key, value) => {
							self.visit_expr(source, key, true);
							self.visit_expr(source, value, true);
						}
						MapEntry::Spread(value) => self.visit_expr(source, value, true),
					}
				}
			}
			ExprKind::Call { func, args, .. } => {
				self.visit_expr(source, func, false);
				for arg in args {
					let removable =
						arg.0.name().is_some() || arg.0.is_spread() || !exposes_named_argument(arg.0.value());
					self.visit_expr(source, arg.0.value(), removable);
				}
			}
			ExprKind::AsyncBlock { body, .. } => {
				self.units.push(expr.span);
				if matches!(&body.kind, ExprKind::Block { .. })
					&& let Some(open) = source[body.span.start..body.span.end].find('{')
				{
					self.blocks.insert(body.span.start + open);
				}
				self.visit_expr(source, body, false);
			}
			ExprKind::Await { value, .. } => self.visit_expr(source, value, false),
			ExprKind::MemberAccess { parent, .. }
			| ExprKind::IndexAccess { parent, .. }
			| ExprKind::PostfixOp { value: parent, .. }
			| ExprKind::PrefixOp { value: parent, .. }
			| ExprKind::Echo {
				operand: parent, ..
			} => self.visit_expr(source, parent, false),
			ExprKind::Closure { body, label, .. } => {
				if label.is_some()
					&& matches!(&body.kind, ExprKind::Block { .. })
					&& let Some(open) = source[body.span.start..body.span.end].find('{')
				{
					self.blocks.insert(body.span.start + open);
				}
				self.visit_expr(source, body, true);
			}
			ExprKind::BinaryOp { lhs, rhs, .. } => {
				if flat_width(&source[expr.span.start..expr.span.end]) > WIDTH.saturating_sub(30)
					&& let Some(operator) = first_token_start(source, lhs.span.end, rhs.span.start)
				{
					self.line_before.insert(operator);
					self.continuation_before.insert(operator);
				}
				self.visit_expr(source, lhs, false);
				self.visit_expr(source, rhs, false);
			}
			ExprKind::TypeOp { lhs, .. } | ExprKind::PatternOp { lhs, .. } => {
				self.visit_expr(source, lhs, false)
			}
			ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
				if let Some(value) = value {
					self.visit_expr(source, value, true);
				}
			}
			ExprKind::Continue { replacements, .. } => {
				for replacement in replacements {
					self.visit_expr(source, &replacement.value, true);
				}
			}
			ExprKind::For { iterable, body, .. } => {
				self.visit_expr(source, iterable, true);
				self.visit_expr(source, body, true);
			}
			ExprKind::StateLoop { bindings, body, .. } => {
				for binding in bindings {
					self.visit_expr(source, &binding.value, true);
				}
				self.visit_expr(source, body, true);
			}
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => {
				self.visit_expr(source, condition, true);
				// Removing braces around a nested `if` in this position can make the
				// outer `else` bind to the inner expression instead.
				self.visit_expr(
					source,
					then,
					otherwise.is_none() || !ends_in_unmatched_if(then),
				);
				if let Some(otherwise) = otherwise {
					self.visit_expr(source, otherwise, true);
				}
			}
			ExprKind::Match { value, arms } => {
				self.visit_expr(source, value, true);
				if let Some(open) = source[value.span.end..expr.span.end].find('{') {
					self.matches.insert(value.span.end + open);
				}
				for arm in arms {
					self.line_before.insert(arm.pattern.1.start);
					if let Some(guard) = &arm.guard {
						self.visit_expr(source, guard, true);
					}
					self.visit_expr(source, &arm.body, true);
				}
			}
			ExprKind::Block { body, label } => {
				let Some(open_offset) = source[expr.span.start..expr.span.end].find('{') else {
					return;
				};
				let open = expr.span.start + open_offset;
				let close = expr.span.end.saturating_sub(1);
				let removable = label.is_none()
					&& root
					&& !self.blocks.contains(&open)
					&& body.len() == 1
					&& matches!(body[0].0, Statement::Expr(_))
					&& !contains_comment(&source[open + 1..body[0].1.start])
					&& !contains_comment(&source[body[0].1.end..close]);
				if removable {
					self.remove_delimiters.insert(open, close);
					self.removable_units.push(expr.span);
				} else {
					self.blocks.insert(open);
					for statement in body {
						self.line_before.insert(statement.1.start);
					}
				}
				for statement in body {
					match &statement.0 {
						Statement::Expr(value) => self.visit_expr(source, value, true),
						Statement::Let { value, .. } => self.visit_expr(source, value, true),
					}
				}
			}
			ExprKind::Grouped(inner) => {
				let open = expr.span.start;
				let close = expr.span.end.saturating_sub(1);
				self.grouped.insert(open);
				if root
					&& !contains_comment(&source[open + 1..inner.span.start])
					&& !contains_comment(&source[inner.span.end..close])
				{
					self.remove_delimiters.insert(open, close);
				}
				self.visit_expr(source, inner, root);
			}
			ExprKind::Range(range) => match range {
				nymph_ast::expr::RangeKind::From(value)
				| nymph_ast::expr::RangeKind::To(value)
				| nymph_ast::expr::RangeKind::ToInclusive(value) => self.visit_expr(source, value, false),
				nymph_ast::expr::RangeKind::Exclusive { min, max }
				| nymph_ast::expr::RangeKind::Inclusive { min, max } => {
					self.visit_expr(source, min, false);
					self.visit_expr(source, max, false);
				}
			},
			ExprKind::Int(_)
			| ExprKind::UInt(_)
			| ExprKind::Float(_)
			| ExprKind::Char(_)
			| ExprKind::Boolean(_)
			| ExprKind::Identifier(_)
			| ExprKind::AnonymousParam(_)
			| ExprKind::This => {}
		}
	}
}

fn ends_in_unmatched_if(expr: &Expr) -> bool {
	match &expr.kind {
		ExprKind::If { otherwise, .. } => otherwise.as_deref().is_none_or(ends_in_unmatched_if),
		ExprKind::Block { body, label: None } if body.len() == 1 => match &body[0].0 {
			Statement::Expr(expr) => ends_in_unmatched_if(expr),
			Statement::Let { .. } => false,
		},
		ExprKind::Grouped(inner) => ends_in_unmatched_if(inner),
		ExprKind::For { body, .. } | ExprKind::Closure { body, .. } => ends_in_unmatched_if(body),
		ExprKind::PrefixOp { value, .. } | ExprKind::Echo { operand: value, .. } => {
			ends_in_unmatched_if(value)
		}
		ExprKind::Return {
			value: Some(value), ..
		}
		| ExprKind::Break {
			value: Some(value), ..
		}
		| ExprKind::BinaryOp { rhs: value, .. } => ends_in_unmatched_if(value),
		ExprKind::Range(range) => match range {
			nymph_ast::expr::RangeKind::To(value)
			| nymph_ast::expr::RangeKind::ToInclusive(value)
			| nymph_ast::expr::RangeKind::Exclusive { max: value, .. }
			| nymph_ast::expr::RangeKind::Inclusive { max: value, .. } => ends_in_unmatched_if(value),
			nymph_ast::expr::RangeKind::From(_) => false,
		},
		_ => false,
	}
}

fn exposes_named_argument(expr: &Expr) -> bool {
	match &expr.kind {
		ExprKind::Grouped(inner) => exposes_named_argument(inner),
		ExprKind::Block { body, label: None } if body.len() == 1 => match &body[0].0 {
			Statement::Expr(expr) => exposes_named_argument(expr),
			Statement::Let { .. } => false,
		},
		_ => false,
	}
}

fn contains_comment(source: &str) -> bool {
	source.contains("//") || source.contains("/*")
}

fn flat_width(source: &str) -> usize {
	let mut scanner = Scanner::new(source);
	let mut width = 0;
	let mut first = true;
	while let Some(item) = scanner.next() {
		if item.kind == Kind::Space {
			continue;
		}
		if !first {
			width += 1;
		}
		width += display_width(item.text);
		first = false;
	}
	width
}

fn display_width(source: &str) -> usize {
	source
		.chars()
		.map(|character| match character {
			'\t' => 2,
			'\r' | '\n' => 0,
			_ => character.width().unwrap_or(0),
		})
		.sum()
}

fn first_token_start(source: &str, start: usize, end: usize) -> Option<usize> {
	let mut scanner = Scanner::new(&source[start..end]);
	while let Some(item) = scanner.next() {
		if item.kind == Kind::Token {
			return Some(start + item.start);
		}
	}
	None
}

fn format_string_literal(literal: &str) -> String {
	if !literal.starts_with('"') || literal.len() < 2 {
		return literal.to_owned();
	}
	let mut output = String::with_capacity(literal.len());
	output.push('"');
	let bytes = literal.as_bytes();
	let mut at = 1;
	let content_end = literal.len().saturating_sub(1);
	while at < content_end {
		if bytes[at] == b'\\' {
			let next = (at + 2).min(content_end);
			output.push_str(&literal[at..next]);
			at = next;
			continue;
		}
		if bytes[at] == b'$' && bytes.get(at + 1) == Some(&b'{') {
			let Some(close) = interpolation_close(literal, at + 2, content_end) else {
				output.push_str(&literal[at..content_end]);
				break;
			};
			let inner = &literal[at + 2..close];
			let formatted = format_fragment(inner);
			let formatted = formatted.trim_start_matches(char::is_whitespace);
			let trailing_line_comment = ends_with_line_comment(formatted);
			let formatted = formatted.strip_suffix('\n').unwrap_or(formatted);
			output.push_str("${");
			output.push_str(formatted);
			if trailing_line_comment {
				output.push('\n');
			}
			output.push('}');
			at = close + 1;
			continue;
		}
		if bytes[at] == b'\r' {
			// Preserve the cooked string value while keeping canonical source free
			// of carriage-return bytes. In CRLF, the following LF remains literal.
			output.push_str("\\r");
			at += 1;
			continue;
		}
		let ch = literal[at..].chars().next().expect("valid string slice");
		output.push(ch);
		at += ch.len_utf8();
	}
	output.push('"');
	output
}

fn ends_with_line_comment(source: &str) -> bool {
	let mut scanner = Scanner::new(source);
	let mut last = None;
	while let Some(item) = scanner.next() {
		if item.kind != Kind::Space {
			last = Some(item.kind);
		}
	}
	last == Some(Kind::LineComment)
}

fn interpolation_close(source: &str, mut at: usize, end: usize) -> Option<usize> {
	let bytes = source.as_bytes();
	let mut depth = 1_u32;
	while at < end {
		match bytes[at] {
			b'"' | b'\'' => {
				let quote = bytes[at];
				at += 1;
				while at < end {
					if bytes[at] == b'\\' {
						at = (at + 2).min(end);
					} else if bytes[at] == quote {
						at += 1;
						break;
					} else {
						at += 1;
					}
				}
			}
			b'/' if bytes.get(at + 1) == Some(&b'/') => {
				at += 2;
				while at < end && bytes[at] != b'\n' {
					at += 1;
				}
			}
			b'/' if bytes.get(at + 1) == Some(&b'*') => {
				at += 2;
				while at + 1 < end && &bytes[at..at + 2] != b"*/" {
					at += 1;
				}
				at = (at + 2).min(end);
			}
			b'{' => {
				depth += 1;
				at += 1;
			}
			b'}' => {
				depth -= 1;
				if depth == 0 {
					return Some(at);
				}
				at += 1;
			}
			_ => at += 1,
		}
	}
	None
}

fn select_unit(source: &str, requested: Span) -> Option<Span> {
	let start = source[..requested.start.min(source.len())]
		.rfind('\n')
		.map_or(0, |index| index + 1);
	let mut end = source[requested.end.min(source.len())..]
		.find('\n')
		.map_or(source.len(), |index| {
			requested.end.min(source.len()) + index + 1
		});
	// Include continuation lines while delimiters opened in the selected text remain open.
	let mut depth = delimiter_depth(&source[start..end]);
	while depth > 0 && end < source.len() {
		end += source[end..]
			.find('\n')
			.map_or(source.len() - end, |i| i + 1);
		depth = delimiter_depth(&source[start..end]);
	}
	// A closer without its opener cannot be formatted safely without a construct
	// span: widening to byte zero would rewrite unrelated preceding declarations.
	if depth < 0 {
		return None;
	}
	Some(Span::new(start, end))
}

fn delimiter_depth(text: &str) -> i32 {
	let mut scanner = Scanner::new(text);
	let mut depth = 0;
	while let Some(item) = scanner.next() {
		if item.kind == Kind::Token {
			match item.text {
				"(" | "[" | "{" | "#(" | "#[" | "#{" => depth += 1,
				")" | "]" | "}" => depth -= 1,
				_ => {}
			}
		}
	}
	depth
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
	Token,
	LineComment,
	BlockComment,
	Space,
}

#[derive(Clone, Copy, Debug)]
struct Item<'a> {
	kind: Kind,
	text: &'a str,
	had_newline: bool,
	start: usize,
	end: usize,
}

/// Lossless concrete scanner. Literal and comment bodies are opaque source slices.
struct Scanner<'a> {
	source: &'a str,
	at: usize,
}

impl<'a> Scanner<'a> {
	fn new(source: &'a str) -> Self {
		Self { source, at: 0 }
	}

	fn next(&mut self) -> Option<Item<'a>> {
		if self.at >= self.source.len() {
			return None;
		}
		let start = self.at;
		let rest = &self.source[start..];
		if rest.starts_with("//") {
			self.at += rest.find('\n').unwrap_or(rest.len());
			return Some(Item {
				kind: Kind::LineComment,
				text: &self.source[start..self.at],
				had_newline: false,
				start,
				end: self.at,
			});
		}
		if rest.starts_with("/*") {
			self.at += rest.find("*/").map_or(rest.len(), |i| i + 2);
			let text = &self.source[start..self.at];
			return Some(Item {
				kind: Kind::BlockComment,
				text,
				had_newline: text.contains('\n'),
				start,
				end: self.at,
			});
		}
		let first = rest.chars().next().unwrap();
		if first.is_whitespace() {
			self.at += rest
				.char_indices()
				.take_while(|(_, c)| c.is_whitespace())
				.map(|(_, c)| c.len_utf8())
				.sum::<usize>();
			let text = &self.source[start..self.at];
			return Some(Item {
				kind: Kind::Space,
				text,
				had_newline: text.contains(['\n', '\r']),
				start,
				end: self.at,
			});
		}
		if first == '"' || first == '\'' {
			self.at = quoted_literal_end(self.source, start, first);
			return Some(Item {
				kind: Kind::Token,
				text: &self.source[start..self.at],
				had_newline: false,
				start,
				end: self.at,
			});
		}
		const OPS: &[&str] = &[
			"...", "<<=", ">>=", "**=", "&&=", "||=", "!in", "!is", "#[", "#(", "#{", "->", "?.", "??",
			"::", "|>", "**", "&&", "||", "<<", ">>", "==", "!=", "<=", ">=", "+=", "-=", "*=", "/=",
			"%=", "&=", "|=", "^=", "~=", "..=", "..",
		];
		if let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) {
			self.at += op.len();
		} else if first.is_ascii_alphanumeric() || first == '_' || unicode_ident::is_xid_start(first) {
			self.at += first.len_utf8();
			self.at += self.source[self.at..]
				.char_indices()
				.take_while(|(_, c)| {
					c.is_ascii_alphanumeric() || *c == '_' || unicode_ident::is_xid_continue(*c)
				})
				.map(|(_, c)| c.len_utf8())
				.sum::<usize>();
		} else {
			self.at += first.len_utf8();
		}
		Some(Item {
			kind: Kind::Token,
			text: &self.source[start..self.at],
			had_newline: false,
			start,
			end: self.at,
		})
	}
}

fn quoted_literal_end(source: &str, start: usize, quote: char) -> usize {
	let bytes = source.as_bytes();
	let mut at = start + quote.len_utf8();
	let mut interpolation_depth = 0_u32;
	while at < source.len() {
		if interpolation_depth == 0 {
			if bytes[at] == b'\\' {
				at = (at + 2).min(source.len());
				continue;
			}
			if bytes[at] == quote as u8 {
				return at + 1;
			}
			if quote == '"' && bytes[at] == b'$' && bytes.get(at + 1) == Some(&b'{') {
				interpolation_depth = 1;
				at += 2;
				continue;
			}
			let character = source[at..].chars().next().expect("character boundary");
			at += character.len_utf8();
			continue;
		}
		match bytes[at] {
			b'"' => at = quoted_literal_end(source, at, '"'),
			b'\'' => at = quoted_literal_end(source, at, '\''),
			b'/' if bytes.get(at + 1) == Some(&b'/') => {
				at += 2;
				while at < source.len() && bytes[at] != b'\n' {
					at += 1;
				}
			}
			b'/' if bytes.get(at + 1) == Some(&b'*') => {
				at += 2;
				while at + 1 < source.len() && &bytes[at..at + 2] != b"*/" {
					at += 1;
				}
				at = (at + 2).min(source.len());
			}
			b'{' => {
				interpolation_depth += 1;
				at += 1;
			}
			b'}' => {
				interpolation_depth -= 1;
				at += 1;
			}
			_ => {
				let character = source[at..].chars().next().expect("character boundary");
				at += character.len_utf8();
			}
		}
	}
	source.len()
}

struct Formatter<'a> {
	source: &'a str,
	scanner: Scanner<'a>,
	out: String,
	indent: usize,
	line_len: usize,
	at_line_start: bool,
	pending_space: bool,
	pending_blank: bool,
	previous: Option<&'a str>,
	depth: usize,
	in_import: bool,
	match_pending: bool,
	match_depth: Option<usize>,
	control_pending: bool,
	hints: Hints,
	skipped_closes: HashSet<usize>,
	declaration_prefix: bool,
	generic_depth: usize,
	map_depths: HashSet<usize>,
	multiline_depths: HashSet<usize>,
	previous_was_prefix: bool,
	previous_was_block_close: bool,
	continuation_line: bool,
}

impl<'a> Formatter<'a> {
	fn new(source: &'a str, hints: Hints) -> Self {
		Self {
			source,
			scanner: Scanner::new(source),
			out: String::new(),
			indent: 0,
			line_len: 0,
			at_line_start: true,
			pending_space: false,
			pending_blank: false,
			previous: None,
			depth: 0,
			in_import: false,
			match_pending: false,
			match_depth: None,
			control_pending: false,
			hints,
			skipped_closes: HashSet::new(),
			declaration_prefix: false,
			generic_depth: 0,
			map_depths: HashSet::new(),
			multiline_depths: HashSet::new(),
			previous_was_prefix: false,
			previous_was_block_close: false,
			continuation_line: false,
		}
	}
	fn finish(mut self) -> String {
		self.run();
		self.final_newline();
		self.out
	}
	fn finish_fragment(mut self) -> String {
		self.run();
		if self.out.ends_with('\n') {
			self.out
		} else {
			self.out.push('\n');
			self.out
		}
	}
	fn run(&mut self) {
		while let Some(item) = self.scanner.next() {
			match item.kind {
				Kind::Space => {
					self.pending_space = true;
					self.pending_blank |= item.text.matches('\n').count() > 1;
				}
				Kind::LineComment => {
					if item.text.starts_with("///")
						&& self.depth == 0
						&& !self.out.ends_with("\n\n")
						&& !self.out.is_empty()
					{
						self.newline();
						self.out.push('\n');
					}
					if !self.at_line_start {
						self.space();
					}
					self.write_raw(item.text.trim_end());
					self.newline();
					self.pending_blank = false;
				}
				Kind::BlockComment => {
					if !self.at_line_start {
						self.space();
					}
					let normalized = item.text.replace("\r\n", "\n").replace('\r', "\n");
					self.write_literal(&normalized);
					if item.had_newline {
						self.newline();
					} else {
						self.pending_space = true;
					}
				}
				Kind::Token => self.token(item),
			}
		}
	}
	fn token(&mut self, item: Item<'a>) {
		let token = item.text;
		if self.skipped_closes.remove(&item.start) {
			self.pending_space = true;
			return;
		}
		if let Some(close) = self.hints.remove_delimiters.get(&item.start).copied() {
			self.skipped_closes.insert(close);
			self.pending_space = true;
			return;
		}
		if self.hints.line_before.contains(&item.start) {
			if !self.at_line_start {
				self.newline();
			}
			self.continuation_line = self.hints.continuation_before.contains(&item.start);
		}
		if token.starts_with('"') {
			if self.needs_space(token) {
				self.space();
			}
			let formatted = format_string_literal(token);
			self.write_literal(&formatted);
			self.previous_was_prefix = false;
			self.previous = Some(token);
			return;
		}
		if self.previous_was_block_close
			&& !matches!(
				token,
				"else" | ")" | "]" | "," | "." | "?." | "::" | "?" | "(" | "["
			) && !is_infix_operator(token)
		{
			self.newline();
		}
		self.previous_was_block_close = false;
		if self.depth == 0 && is_declaration_prefix(token) {
			if !self.declaration_prefix && self.previous.is_some() {
				self.newline();
			}
			self.declaration_prefix = true;
			self.in_import = false;
		} else if self.depth == 0 && is_declaration_start(token) {
			if !self.declaration_prefix && self.previous.is_some() {
				self.newline();
			}
			self.declaration_prefix = false;
			self.in_import = false;
		}
		if self.pending_blank
			&& self.at_line_start
			&& !self.out.ends_with("\n\n")
			&& !self.out.is_empty()
		{
			self.out.push('\n');
		}
		self.pending_blank = false;
		if self.depth == 0 && token == "import" {
			self.in_import = true;
		}
		if token == "match" {
			self.match_pending = true;
		}
		if matches!(token, "if" | "while" | "for" | "loop" | "match") {
			self.control_pending = true;
		}
		match token {
			"}" => {
				if self.map_depths.remove(&self.depth) {
					if self.multiline_depths.remove(&self.depth) {
						if self.previous != Some(",") {
							self.trailing_comma();
						}
						self.indent = self.indent.saturating_sub(1);
						self.newline();
					}
					self.depth = self.depth.saturating_sub(1);
					self.write_raw(token);
					self.previous = Some(token);
					self.previous_was_block_close = false;
					return;
				}
				if self.match_depth == Some(self.depth) && self.previous != Some(",") {
					self.trailing_comma();
				}
				self.depth = self.depth.saturating_sub(1);
				self.indent = self.indent.saturating_sub(1);
				if !self.at_line_start {
					self.newline();
				}
				self.write_raw(token);
				if self.match_depth == Some(self.depth + 1) {
					self.match_depth = None;
				}
				self.previous_was_block_close = true;
			}
			"{" => {
				self.control_pending = false;
				if let Some(close) = self.empty_brace_close(item.end) {
					if self.needs_space(token) {
						self.space();
					}
					self.write_raw("{}");
					self.skipped_closes.insert(close);
					self.previous = Some("}");
					self.previous_was_block_close = true;
					return;
				}
				if self.needs_space(token) {
					self.space();
				}
				self.write_raw(token);
				self.depth += 1;
				if self.match_pending {
					self.match_depth = Some(self.depth);
					self.match_pending = false;
				}
				self.indent += 1;
				self.newline();
			}
			"," => {
				self.write_raw(token);
				if self.match_depth == Some(self.depth)
					|| self.multiline_depths.contains(&self.depth)
					|| self.line_len > WIDTH
				{
					self.newline();
				} else {
					self.pending_space = true;
				}
			}
			";" => {}
			")" | "]" => {
				if self.multiline_depths.remove(&self.depth) {
					if self.previous != Some(",") {
						self.trailing_comma();
					}
					self.indent = self.indent.saturating_sub(1);
					self.newline();
				}
				self.depth = self.depth.saturating_sub(1);
				self.write_raw(token);
			}
			"(" | "[" | "#(" | "#[" | "#{" => {
				if token == "("
					&& (self.control_pending || self.previous == Some("with") || self.previous == Some(","))
				{
					self.space();
					self.control_pending = false;
				} else if token == "(" && self.hints.grouped.contains(&item.start) {
					if self.grouped_needs_space() {
						self.space();
					}
				} else if self.needs_space(token) {
					self.space();
				}
				self.write_raw(token);
				self.depth += 1;
				if self.hints.multiline_lists.contains_key(&item.start) {
					self.multiline_depths.insert(self.depth);
					self.indent += 1;
					self.newline();
				}
				if token == "#{" {
					self.map_depths.insert(self.depth);
				}
			}
			"." | "?." | "::" => {
				if self.previous == Some("import") {
					self.space();
				}
				self.write_raw(token);
			}
			"/" if self.in_import => self.write_raw(token),
			"@" => {
				if self.previous == Some("import") {
					self.space();
				}
				self.write_raw(token);
			}
			":" => {
				self.write_raw(token);
				self.pending_space = true;
			}
			"<" if self.looks_like_generic(item.start) => {
				self.write_raw(token);
				self.generic_depth += 1;
			}
			">>" if self.generic_depth >= 2 => {
				self.write_raw(token);
				self.generic_depth -= 2;
			}
			">" if self.generic_depth > 0 => {
				self.write_raw(token);
				self.generic_depth -= 1;
			}
			".." | "..=" => {
				if self.previous == Some(",") {
					self.space();
				}
				self.write_raw(token);
			}
			_ => {
				if self.needs_space(token) {
					self.space();
				}
				self.write_raw(token);
			}
		}
		if token == "}" {
			self.pending_space = true;
		}
		if self.hints.comma_after.contains(&item.end) {
			self.write_raw(",");
			self.newline();
		}
		self.previous_was_prefix = is_prefix_operator(token, self.previous);
		self.previous = Some(token);
	}
	fn needs_space(&self, token: &str) -> bool {
		if self.at_line_start {
			return false;
		}
		let Some(prev) = self.previous else {
			return false;
		};
		let tight_before = matches!(token, ")" | "]" | "," | "." | "?." | "::" | "?" | "(" | "[");
		let tight_after = matches!(
			prev,
			"(" | "[" | "." | "?." | "::" | "#(" | "#[" | "#{" | "@" | "!" | "..." | ".." | "..="
		) || self.previous_was_prefix
			|| self.generic_depth > 0 && prev == "<";
		if self.in_import && prev == "/" {
			return false;
		}
		if tight_before || tight_after {
			return false;
		}
		true
	}

	fn grouped_needs_space(&self) -> bool {
		let Some(previous) = self.previous else {
			return false;
		};
		!self.previous_was_prefix
			&& !matches!(
				previous,
				"(" | "[" | "." | "?." | "::" | "#(" | "#[" | "#{" | "@" | "!" | "..." | ".." | "..="
			)
	}

	fn empty_brace_close(&self, after_open: usize) -> Option<usize> {
		let mut scanner = Scanner {
			source: self.source,
			at: after_open,
		};
		while let Some(item) = scanner.next() {
			match item.kind {
				Kind::Space => continue,
				Kind::Token if item.text == "}" => return Some(item.start),
				_ => return None,
			}
		}
		None
	}

	fn looks_like_generic(&self, start: usize) -> bool {
		let mut scanner = Scanner {
			source: self.source,
			at: start + 1,
		};
		let mut depth = 1;
		while let Some(item) = scanner.next() {
			if item.kind != Kind::Token {
				continue;
			}
			match item.text {
				"<" => depth += 1,
				">>" if depth <= 2 => return true,
				">>" => depth -= 2,
				">" => {
					depth -= 1;
					if depth == 0 {
						return true;
					}
				}
				"+" if depth == 1 => {
					let next = std::iter::from_fn(|| scanner.next()).find(|next| next.kind == Kind::Token);
					if !next.is_some_and(|next| next.text == "!") {
						return false;
					}
				}
				"-" | "*" | "/" | "%" | "==" | "!=" | "&&" | "||" if depth == 1 => {
					return false;
				}
				"(" | ")" | "[" | "]" | "{" | "}" | ";" if depth == 1 => return false,
				_ => {}
			}
		}
		false
	}
	fn write_raw(&mut self, text: &str) {
		if self.at_line_start {
			let indent = self.indent + usize::from(self.continuation_line);
			for _ in 0..indent {
				self.out.push('\t');
			}
			self.line_len = indent * 2;
			self.at_line_start = false;
			self.continuation_line = false;
		}
		self.out.push_str(text);
		self.line_len += display_width(text);
	}
	fn trailing_comma(&mut self) {
		if self.at_line_start {
			let end = self.out.trim_end_matches('\n').len();
			self.out.insert(end, ',');
		} else {
			self.write_raw(",");
		}
	}

	fn write_literal(&mut self, text: &str) {
		if self.at_line_start {
			let indent = self.indent + usize::from(self.continuation_line);
			for _ in 0..indent {
				self.out.push('\t');
			}
			self.line_len = indent * 2;
			self.at_line_start = false;
			self.continuation_line = false;
		}
		self.out.push_str(text);
		if text.contains('\n') {
			self.line_len = text.rsplit('\n').next().map_or(0, display_width);
		} else {
			self.line_len += display_width(text);
		}
	}
	fn space(&mut self) {
		if !self.at_line_start && !self.out.ends_with([' ', '\n', '\t']) {
			self.out.push(' ');
			self.line_len += 1;
		}
	}
	fn newline(&mut self) {
		while self.out.ends_with([' ', '\t']) {
			self.out.pop();
		}
		if !self.out.ends_with('\n') {
			self.out.push('\n');
		}
		self.at_line_start = true;
		self.pending_space = false;
	}
	fn final_newline(&mut self) {
		while self.out.ends_with(char::is_whitespace) {
			self.out.pop();
		}
		self.out.push('\n');
	}
}

fn is_declaration_start(token: &str) -> bool {
	matches!(
		token,
		"import"
			| "let"
			| "func"
			| "effect"
			| "type"
			| "struct"
			| "enum"
			| "namespace"
			| "interface"
			| "impl"
	)
}

fn is_declaration_prefix(token: &str) -> bool {
	matches!(
		token,
		"public" | "internal" | "private" | "external" | "async"
	)
}

fn is_infix_operator(token: &str) -> bool {
	matches!(
		token,
		"+"
			| "-"
			| "*"
			| "/"
			| "%"
			| "**"
			| "&"
			| "|"
			| "^"
			| "=="
			| "!="
			| "<"
			| ">"
			| "<="
			| ">="
			| "in"
			| "!in"
			| "&&"
			| "||"
			| "|>"
			| "??"
			| "<<"
			| ">>"
			| "="
			| "+="
			| "-="
			| "*="
			| "/="
			| "%="
			| "**="
			| "<<="
			| ">>="
			| "&="
			| "^="
			| "|="
			| "~="
			| "&&="
			| "||="
			| "as"
			| "is"
			| "!is"
	)
}

fn is_prefix_operator(token: &str, previous: Option<&str>) -> bool {
	if !matches!(token, "!" | "-" | "~") {
		return false;
	}
	previous.is_none_or(|previous| {
		matches!(
			previous,
			"("
				| "["
				| "#("
				| "#["
				| "#{"
				| "{"
				| ","
				| "="
				| "->"
				| ":"
				| "return"
				| "break"
				| "+"
				| "-"
				| "*"
				| "/"
				| "%"
				| "&&"
				| "||"
				| "|>"
		)
	})
}
