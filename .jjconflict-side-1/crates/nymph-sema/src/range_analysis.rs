use std::collections::HashMap;
use std::sync::Arc;

use ecow::EcoString;
use nymph_ast::decl::{Declaration, ImplMember, InterfaceElement, InterfaceMember, Module};
use nymph_ast::expr::{Expr, ExprKind, ListItem, RangeKind, Statement, StringPart};
use nymph_ast::ops::{BinaryOperator, PrefixOperator};
use nymph_ast::{NodeId, Span};
use nymph_hir::ty::{Interner, TyKind};

use crate::{Annotations, DispatchKind, RangeDecision, RangeEvidence, RangeOperation, RangeProof};

const I64_MIN: i128 = i64::MIN as i128;
const I64_MAX: i128 = i64::MAX as i128;
const U64_MIN: i128 = 0;
const U64_MAX: i128 = u64::MAX as i128;
const HOST_INDEX_MAX: i128 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Default)]
struct Value {
	interval: Option<(i128, i128)>,
	length: Option<u64>,
	excludes_zero: bool,
	symbol: Option<EcoString>,
}

impl Value {
	fn interval(min: i128, max: i128) -> Self {
		Self {
			interval: Some((min, max)),
			length: None,
			excludes_zero: min > 0 || max < 0,
			symbol: None,
		}
	}

	fn exact(value: i128) -> Self {
		Self::interval(value, value)
	}

	fn evidence(&self, operand: u8) -> Vec<RangeEvidence> {
		let mut evidence = self
			.interval
			.map(|(min, max)| RangeEvidence::Interval { min, max })
			.into_iter()
			.collect::<Vec<_>>();
		if self.excludes_zero {
			evidence.push(RangeEvidence::Excluded { operand, value: 0 });
		}
		if let Some(length) = self.length {
			evidence.push(RangeEvidence::KnownLength(length));
		}
		evidence
	}
}

#[derive(Clone)]
struct PairBound {
	left: EcoString,
	left_sign: i8,
	right: EcoString,
	right_sign: i8,
	upper: i128,
}

#[derive(Default)]
pub(crate) struct RangeAnalysis {
	pub proofs: Vec<(NodeId, RangeProof)>,
	pub diagnostics: Vec<(Span, EcoString)>,
}

pub(crate) fn analyze_module(
	module: &Module,
	annotations: &Annotations,
	interner: &Interner,
) -> RangeAnalysis {
	let mut result = RangeAnalysis::default();
	for body in bodies(module) {
		Analyzer {
			annotations,
			interner,
			result: &mut result,
			scopes: vec![HashMap::new()],
			pair_bounds: Vec::new(),
		}
		.eval(body);
	}
	result.proofs.sort_unstable_by_key(|(node, _)| *node);
	result.proofs.dedup_by_key(|(node, _)| *node);
	result
}

fn bodies(module: &Module) -> Vec<&Expr> {
	let mut result = Vec::new();
	for declaration in &module.members {
		match declaration {
			Declaration::Let { value, .. } | Declaration::Func { body: value, .. } => result.push(value),
			Declaration::Struct {
				members,
				impls,
				fields,
				..
			} => {
				for field in fields {
					if let Some(default) = &field.0.default {
						result.push(default);
					}
				}
				member_bodies(members.iter().map(|member| &member.0), &mut result);
				for implementation in impls {
					member_bodies(
						implementation.0.members.iter().map(|member| &member.0),
						&mut result,
					);
				}
			}
			Declaration::Enum {
				members,
				impls,
				variants,
				..
			} => {
				for variant in variants {
					for field in &variant.0.fields {
						if let Some(default) = &field.0.default {
							result.push(default);
						}
					}
				}
				member_bodies(members.iter().map(|member| &member.0), &mut result);
				for implementation in impls {
					member_bodies(
						implementation.0.members.iter().map(|member| &member.0),
						&mut result,
					);
				}
			}
			Declaration::Namespace { members, .. }
			| Declaration::Impl { members, .. }
			| Declaration::ImplFor { members, .. } => {
				member_bodies(members.iter().map(|member| &member.0), &mut result);
			}
			Declaration::Interface { members, .. } => {
				for member in members {
					match &member.0 {
						InterfaceMember::Element(element) => match &element.0 {
							InterfaceElement::Let {
								value: Some(value), ..
							}
							| InterfaceElement::Func {
								body: Some(value), ..
							} => result.push(value),
							_ => {}
						},
						InterfaceMember::Impl { members, .. } => {
							member_bodies(members.iter().map(|member| &member.0), &mut result);
						}
					}
				}
			}
			Declaration::Import { .. }
			| Declaration::ExternalLet(..)
			| Declaration::ExternalFunc(..)
			| Declaration::Effect { .. }
			| Declaration::TypeAlias { .. } => {}
		}
	}
	result
}

fn member_bodies<'a>(members: impl Iterator<Item = &'a ImplMember>, result: &mut Vec<&'a Expr>) {
	for member in members {
		match member {
			ImplMember::Let { value, .. } | ImplMember::Func { body: value, .. } => result.push(value),
			ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => {}
		}
	}
}

struct Analyzer<'a> {
	annotations: &'a Annotations,
	interner: &'a Interner,
	result: &'a mut RangeAnalysis,
	scopes: Vec<HashMap<EcoString, Value>>,
	pair_bounds: Vec<PairBound>,
}

impl Analyzer<'_> {
	fn eval(&mut self, expr: &Expr) -> Value {
		match &expr.kind {
			ExprKind::Int(value) => Value::exact(*value.value() as i128),
			ExprKind::UInt(value) => Value::exact(*value.value() as i128),
			ExprKind::Identifier(name) => {
				if let Some(value) = self
					.scopes
					.iter()
					.rev()
					.find_map(|scope| scope.get(&name.0).cloned())
				{
					value
				} else {
					let mut value = self.declared_value(expr.id);
					value.symbol = Some(name.0.clone());
					self
						.scopes
						.first_mut()
						.unwrap()
						.insert(name.0.clone(), value.clone());
					value
				}
			}
			ExprKind::Grouped(inner) => self.eval(inner),
			ExprKind::PrefixOp { op, value } => {
				let value = self.eval(value);
				match (op, value.interval) {
					(PrefixOperator::Negate, Some((min, max))) => Value::interval(-max, -min),
					(PrefixOperator::BitNot, Some((min, max))) => Value::interval(!max, !min),
					_ => Value::default(),
				}
			}
			ExprKind::BinaryOp { lhs, op, rhs } => self.binary(expr, lhs, *op, rhs),
			ExprKind::TypeOp { lhs, .. } => self.cast(expr, lhs),
			ExprKind::List(items) | ExprKind::Tuple(items) => {
				for item in items {
					match &item.0 {
						ListItem::Expr(value) | ListItem::Spread(value) => {
							self.eval(value);
						}
					}
				}
				let length = items
					.iter()
					.all(|item| matches!(item.0, ListItem::Expr(_)))
					.then_some(items.len() as u64);
				Value {
					length,
					..Value::default()
				}
			}
			ExprKind::String(parts) => {
				let mut length = 0_u64;
				let mut known = true;
				for part in parts {
					match &part.0 {
						StringPart::Text(value) => length += value.chars().count() as u64,
						StringPart::EscapeSequence(value) => length += u64::from(value.to_char().is_some()),
						StringPart::InterpolatedExpr(value) => {
							self.eval(value);
							known = false;
						}
					}
				}
				Value {
					length: known.then_some(length),
					..Value::default()
				}
			}
			ExprKind::IndexAccess { parent, index, .. } => self.index(expr, parent, index),
			ExprKind::Block { body, .. } => self.block(body),
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => {
				self.eval(condition);
				let original = self.scopes.clone();
				let original_pairs = self.pair_bounds.clone();
				self.refine(condition, true);
				let then = self.eval(then);
				self.scopes.clone_from(&original);
				self.pair_bounds.clone_from(&original_pairs);
				let otherwise = otherwise.as_ref().map(|value| {
					self.refine(condition, false);
					self.eval(value)
				});
				self.scopes = original;
				self.pair_bounds = original_pairs;
				join(then, otherwise.unwrap_or_default())
			}
			ExprKind::Call { func, args, .. } => {
				let receiver_length = if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
					&& member.0 == "length"
				{
					self.eval(parent).length
				} else {
					self.eval(func);
					None
				};
				for argument in args {
					self.eval(argument.0.value());
				}
				// Calls can mutate aliased collections or captured bindings. Keep exact
				// lengths only until the next call rather than publishing stale proofs.
				self.invalidate_lengths();
				if let Some(length) = receiver_length {
					Value::exact(length as i128)
				} else if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
					&& member.0 == "length"
					&& let ExprKind::Identifier(name) = &parent.kind
				{
					let mut value = Value::interval(0, HOST_INDEX_MAX);
					value.symbol = Some(format!("length:{}", name.0).into());
					value
				} else {
					Value::default()
				}
			}
			ExprKind::For {
				variable,
				iterable,
				body,
				..
			} => {
				let range = self.range_interval(iterable);
				self.eval(iterable);
				self.scopes.push(HashMap::new());
				if let (Some(name), Some((min, max))) = (variable.0.as_binding(), range) {
					self
						.scopes
						.last_mut()
						.unwrap()
						.insert(name.0.clone(), Value::interval(min, max));
				}
				let result = self.eval(body);
				self.scopes.pop();
				result
			}
			_ => {
				expr.for_each_child(|child| {
					self.eval(child);
				});
				self.declared_value(expr.id)
			}
		}
	}

	fn block(&mut self, statements: &[nymph_ast::Spanned<Statement>]) -> Value {
		self.scopes.push(HashMap::new());
		let mut result = Value::default();
		for statement in statements {
			match &statement.0 {
				Statement::Expr(expr) => result = self.eval(expr),
				Statement::Let { meta, value } => {
					let inferred = self.eval(value);
					if let Some(name) = meta.name.0.as_binding() {
						self
							.scopes
							.last_mut()
							.unwrap()
							.insert(name.0.clone(), inferred);
					}
				}
			}
		}
		self.scopes.pop();
		result
	}

	fn binary(&mut self, expr: &Expr, lhs: &Expr, op: BinaryOperator, rhs: &Expr) -> Value {
		let left = self.eval(lhs);
		let right = self.eval(rhs);
		let builtin = self
			.annotations
			.resolution_of(expr.id)
			.is_some_and(|resolution| resolution.dispatch == DispatchKind::BuiltinEager);
		if !builtin {
			return self.declared_value(expr.id);
		}
		let operation = match op {
			BinaryOperator::Plus | BinaryOperator::Minus | BinaryOperator::Times => {
				Some(RangeOperation::Arithmetic)
			}
			BinaryOperator::Divide => Some(RangeOperation::Division),
			BinaryOperator::Remainder => Some(RangeOperation::Remainder),
			BinaryOperator::Power => Some(RangeOperation::Power),
			BinaryOperator::LeftShift | BinaryOperator::RightShift => Some(RangeOperation::Shift),
			_ => None,
		};
		let Some(operation) = operation else {
			return self.declared_value(expr.id);
		};
		let result = arithmetic_interval(op, &left, &right);
		let (target_min, target_max) = self.integer_bounds(expr.id).unwrap_or((I64_MIN, I64_MAX));
		let decision = classify_operation(op, &left, &right, &result, target_min, target_max);
		let mut evidence = left.evidence(0);
		evidence.extend(right.evidence(1));
		if let Some((min, max)) = result.interval {
			evidence.push(RangeEvidence::Interval { min, max });
		}
		evidence.push(RangeEvidence::Target {
			min: target_min,
			max: target_max,
		});
		self.record(
			expr,
			operation,
			decision,
			evidence,
			invalid_reason(operation, decision),
		);
		result
	}

	fn cast(&mut self, expr: &Expr, lhs: &Expr) -> Value {
		let value = self.eval(lhs);
		let Some((min, max)) = self.integer_bounds(expr.id) else {
			return self.declared_value(expr.id);
		};
		let decision = match value.interval {
			Some((lo, hi)) if lo >= min && hi <= max => RangeDecision::Safe,
			Some((lo, hi)) if hi < min || lo > max => RangeDecision::Invalid,
			_ => RangeDecision::Unknown,
		};
		self.record(
			expr,
			RangeOperation::Conversion,
			decision,
			{
				let mut evidence = value.evidence(0);
				evidence.push(RangeEvidence::Target { min, max });
				evidence
			},
			Some("integer conversion is outside the destination range"),
		);
		value
	}

	fn index(&mut self, expr: &Expr, parent: &Expr, index: &Expr) -> Value {
		let collection = self.eval(parent);
		if let ExprKind::Range(range) = &index.kind {
			self.eval(index);
			let mut decision = RangeDecision::Safe;
			let mut evidence = collection
				.length
				.map(RangeEvidence::KnownLength)
				.into_iter()
				.collect::<Vec<_>>();
			for (bound, inclusive) in range_bounds(range) {
				let value = self.eval(bound);
				if let Some(length) = collection.length {
					if let Some((min, max)) = value.interval {
						evidence.push(RangeEvidence::SliceBound {
							min,
							max,
							inclusive,
						});
					}
					decision = combine(decision, classify_bound(&value, length, inclusive));
				} else {
					let (bound_decision, bound_evidence) =
						self.symbolic_slice_bound(&collection, &value, inclusive);
					decision = combine(decision, bound_decision);
					if let Some(bound_evidence) = bound_evidence {
						evidence.push(bound_evidence);
					}
				}
			}
			let operation = if matches!(
				range,
				RangeKind::Inclusive { .. } | RangeKind::ToInclusive(_)
			) {
				RangeOperation::SliceInclusive
			} else {
				RangeOperation::SliceExclusive
			};
			self.record(
				expr,
				operation,
				decision,
				evidence,
				Some("slice bound is outside the collection"),
			);
			return self.declared_value(expr.id);
		}
		let index = self.eval(index);
		let decision = collection.length.map_or_else(
			|| self.symbolic_index_decision(&collection, &index),
			|length| classify_index(&index, length),
		);
		let mut evidence = index.evidence(0);
		evidence.extend(self.symbolic_index_evidence(&collection, &index));
		if let Some(length) = collection.length {
			evidence.push(RangeEvidence::KnownLength(length));
		}
		self.record(
			expr,
			RangeOperation::Index,
			decision,
			evidence,
			Some("index is outside the collection"),
		);
		self.declared_value(expr.id)
	}

	fn record(
		&mut self,
		expr: &Expr,
		operation: RangeOperation,
		decision: RangeDecision,
		evidence: Vec<RangeEvidence>,
		reason: Option<&'static str>,
	) {
		self.result.proofs.push((
			expr.id,
			RangeProof {
				operation,
				decision,
				evidence: Arc::from(evidence),
			},
		));
		if decision == RangeDecision::Invalid
			&& let Some(reason) = reason
		{
			self.result.diagnostics.push((expr.span, reason.into()));
		}
	}

	fn declared_value(&self, node: NodeId) -> Value {
		self
			.integer_bounds(node)
			.map_or_else(Value::default, |(min, max)| Value::interval(min, max))
	}

	fn integer_bounds(&self, node: NodeId) -> Option<(i128, i128)> {
		let ty = self.annotations.get(node)?.ty;
		match self.interner.kind(ty) {
			TyKind::Int => Some((I64_MIN, I64_MAX)),
			TyKind::UInt => Some((U64_MIN, U64_MAX)),
			_ => None,
		}
	}

	fn invalidate_lengths(&mut self) {
		for scope in &mut self.scopes {
			for value in scope.values_mut() {
				value.length = None;
			}
		}
	}

	fn refine(&mut self, condition: &Expr, truth: bool) {
		let ExprKind::BinaryOp { lhs, op, rhs } = &condition.kind else {
			return;
		};
		if matches!(op, BinaryOperator::BoolAnd) && truth {
			self.refine(lhs, true);
			self.refine(rhs, true);
			return;
		}
		let right = self.eval(rhs);
		if let (Some((left, left_expr_sign)), Some((right, right_expr_sign))) =
			(signed_expression_symbol(lhs), signed_expression_symbol(rhs))
		{
			if let Some((left_sign, right_sign, upper)) = comparison_bound(*op, truth) {
				self.pair_bounds.push(PairBound {
					left,
					left_sign: left_sign * left_expr_sign,
					right,
					right_sign: right_sign * right_expr_sign,
					upper,
				});
			}
			return;
		}
		let (ExprKind::Identifier(name), Some((value, value_max))) = (&lhs.kind, right.interval) else {
			return;
		};
		if value != value_max {
			return;
		}
		let Some(bound) = self
			.scopes
			.iter_mut()
			.rev()
			.find_map(|scope| scope.get_mut(&name.0))
		else {
			return;
		};
		let Some((mut min, mut max)) = bound.interval else {
			return;
		};
		match (op, truth) {
			(BinaryOperator::LessThan, true) | (BinaryOperator::GreaterThanEquals, false) => {
				max = max.min(value - 1);
			}
			(BinaryOperator::LessThanEquals, true) | (BinaryOperator::GreaterThan, false) => {
				max = max.min(value);
			}
			(BinaryOperator::GreaterThan, true) | (BinaryOperator::LessThanEquals, false) => {
				min = min.max(value + 1);
			}
			(BinaryOperator::GreaterThanEquals, true) | (BinaryOperator::LessThan, false) => {
				min = min.max(value);
			}
			(BinaryOperator::NotEquals, true) | (BinaryOperator::Equals, false) if value == 0 => {
				bound.excludes_zero = true;
			}
			_ => {}
		}
		bound.interval = Some((min, max));
	}

	fn symbolic_index_evidence(&self, collection: &Value, index: &Value) -> Vec<RangeEvidence> {
		let (Some(collection), Some(index)) = (collection.symbol.as_ref(), index.symbol.as_ref())
		else {
			return vec![];
		};
		let length = EcoString::from(format!("length:{collection}"));
		self
			.pair_bounds
			.iter()
			.filter(|bound| {
				&bound.left == index
					&& bound.right == length
					&& ((bound.left_sign == 1 && bound.right_sign == -1 && bound.upper <= -1)
						|| (bound.left_sign == -1 && bound.right_sign == -1 && bound.upper <= 0))
			})
			.map(|bound| RangeEvidence::SignedPairBound {
				left_sign: bound.left_sign,
				right_sign: bound.right_sign,
				upper: bound.upper,
			})
			.collect()
	}

	fn symbolic_index_decision(&self, collection: &Value, index: &Value) -> RangeDecision {
		let (Some(collection), Some(index_symbol), Some((minimum, maximum))) = (
			collection.symbol.as_ref(),
			index.symbol.as_ref(),
			index.interval,
		) else {
			return RangeDecision::Unknown;
		};
		let length = EcoString::from(format!("length:{collection}"));
		let positive = minimum >= 0 && self.has_pair(index_symbol, 1, &length, -1, -1);
		let negative = maximum <= -1 && self.has_pair(index_symbol, -1, &length, -1, 0);
		if positive || negative {
			RangeDecision::Safe
		} else {
			RangeDecision::Unknown
		}
	}

	fn symbolic_slice_bound(
		&self,
		collection: &Value,
		bound: &Value,
		inclusive: bool,
	) -> (RangeDecision, Option<RangeEvidence>) {
		let (Some(collection), Some(bound_symbol), Some((min, max))) = (
			collection.symbol.as_ref(),
			bound.symbol.as_ref(),
			bound.interval,
		) else {
			return (RangeDecision::Unknown, None);
		};
		let length = EcoString::from(format!("length:{collection}"));
		let lower = self.has_pair(bound_symbol, -1, &length, -1, 0);
		let upper = self.has_pair(bound_symbol, 1, &length, -1, if inclusive { -1 } else { 0 });
		let decision = if (min >= 0 || lower) && (max < 0 || upper) {
			RangeDecision::Safe
		} else {
			RangeDecision::Unknown
		};
		(
			decision,
			Some(RangeEvidence::SymbolicSliceBound {
				min,
				max,
				inclusive,
				lower,
				upper,
			}),
		)
	}

	fn has_pair(
		&self,
		left: &EcoString,
		left_sign: i8,
		right: &EcoString,
		right_sign: i8,
		upper: i128,
	) -> bool {
		self.pair_bounds.iter().any(|bound| {
			&bound.left == left
				&& bound.left_sign == left_sign
				&& &bound.right == right
				&& bound.right_sign == right_sign
				&& bound.upper <= upper
		})
	}

	fn range_interval(&mut self, expr: &Expr) -> Option<(i128, i128)> {
		let ExprKind::Range(range) = &expr.kind else {
			return None;
		};
		match range {
			RangeKind::Exclusive { min, max } => {
				let (min, min_max) = self.eval(min).interval?;
				let (max_min, max) = self.eval(max).interval?;
				(min == min_max && max_min == max && min < max).then_some((min, max - 1))
			}
			RangeKind::Inclusive { min, max } => {
				let (min, min_max) = self.eval(min).interval?;
				let (max_min, max) = self.eval(max).interval?;
				(min == min_max && max_min == max && min <= max).then_some((min, max))
			}
			_ => None,
		}
	}
}

fn arithmetic_interval(op: BinaryOperator, left: &Value, right: &Value) -> Value {
	let (a, b) = match (left.interval, right.interval) {
		(Some(left), Some(right)) => (left, right),
		_ => return Value::default(),
	};
	let interval = match op {
		BinaryOperator::Plus => (a.0.saturating_add(b.0), a.1.saturating_add(b.1)),
		BinaryOperator::Minus => (a.0.saturating_sub(b.1), a.1.saturating_sub(b.0)),
		BinaryOperator::Times => {
			let products = [
				a.0.saturating_mul(b.0),
				a.0.saturating_mul(b.1),
				a.1.saturating_mul(b.0),
				a.1.saturating_mul(b.1),
			];
			(
				*products.iter().min().unwrap(),
				*products.iter().max().unwrap(),
			)
		}
		BinaryOperator::Divide | BinaryOperator::Remainder => (a.0, a.1),
		BinaryOperator::Power if b.0 == b.1 && b.0 >= 0 && b.0 <= u32::MAX as i128 => {
			let exponent = b.0 as u32;
			let values = [
				saturating_power(a.0, exponent),
				saturating_power(a.1, exponent),
				saturating_power(0, exponent),
			];
			(*values.iter().min().unwrap(), *values.iter().max().unwrap())
		}
		BinaryOperator::LeftShift if b.0 >= 0 && b.1 < 64 => (
			saturating_shift_left(a.0, b.1 as u32),
			saturating_shift_left(a.1, b.1 as u32),
		),
		BinaryOperator::RightShift if b.0 >= 0 && b.1 < 64 => (a.0 >> b.1, a.1 >> b.0),
		_ => return Value::default(),
	};
	Value::interval(interval.0, interval.1)
}

fn saturating_shift_left(value: i128, shift: u32) -> i128 {
	value
		.checked_shl(shift)
		.unwrap_or(if value < 0 { i128::MIN } else { i128::MAX })
}

fn saturating_power(value: i128, exponent: u32) -> i128 {
	value.checked_pow(exponent).unwrap_or({
		if value < 0 && exponent % 2 == 1 {
			i128::MIN
		} else {
			i128::MAX
		}
	})
}

fn classify_operation(
	op: BinaryOperator,
	left: &Value,
	right: &Value,
	result: &Value,
	min: i128,
	max: i128,
) -> RangeDecision {
	if matches!(op, BinaryOperator::Divide | BinaryOperator::Remainder) {
		if right.interval == Some((0, 0)) {
			return RangeDecision::Invalid;
		}
		if !right.excludes_zero {
			return RangeDecision::Unknown;
		}
	}
	if matches!(op, BinaryOperator::LeftShift | BinaryOperator::RightShift) {
		return match right.interval {
			Some((lo, hi)) if lo >= 0 && hi < 64 => fit(result, min, max),
			Some((lo, hi)) if hi < 0 || lo >= 64 => RangeDecision::Invalid,
			_ => RangeDecision::Unknown,
		};
	}
	if op == BinaryOperator::Power {
		return match right.interval {
			Some((_, hi)) if hi < 0 => RangeDecision::Invalid,
			Some((lo, _)) if lo < 0 => RangeDecision::Unknown,
			Some(_) => fit(result, min, max),
			_ => RangeDecision::Unknown,
		};
	}
	let _ = left;
	fit(result, min, max)
}

fn fit(value: &Value, min: i128, max: i128) -> RangeDecision {
	match value.interval {
		Some((lo, hi)) if lo >= min && hi <= max => RangeDecision::Safe,
		Some((lo, hi)) if hi < min || lo > max => RangeDecision::Invalid,
		_ => RangeDecision::Unknown,
	}
}

fn classify_index(index: &Value, length: u64) -> RangeDecision {
	let length = length as i128;
	match index.interval {
		Some((min, max)) if min >= -length && max < length => RangeDecision::Safe,
		Some((min, max)) if max < -length || min >= length => RangeDecision::Invalid,
		_ => RangeDecision::Unknown,
	}
}

fn classify_bound(bound: &Value, length: u64, inclusive: bool) -> RangeDecision {
	let length = length as i128;
	let maximum = if inclusive { length - 1 } else { length };
	match bound.interval {
		Some((min, max)) if min >= -length && max <= maximum => RangeDecision::Safe,
		Some((min, max)) if max < -length || min > maximum => RangeDecision::Invalid,
		_ => RangeDecision::Unknown,
	}
}

fn range_bounds(range: &RangeKind) -> Vec<(&Expr, bool)> {
	match range {
		RangeKind::From(start) => vec![(start, false)],
		RangeKind::To(end) => vec![(end, false)],
		RangeKind::Exclusive { min, max } => vec![(min, false), (max, false)],
		RangeKind::ToInclusive(end) => vec![(end, true)],
		RangeKind::Inclusive { min, max } => vec![(min, false), (max, true)],
	}
}

fn combine(left: RangeDecision, right: RangeDecision) -> RangeDecision {
	match (left, right) {
		(RangeDecision::Invalid, _) | (_, RangeDecision::Invalid) => RangeDecision::Invalid,
		(RangeDecision::Unknown, _) | (_, RangeDecision::Unknown) => RangeDecision::Unknown,
		_ => RangeDecision::Safe,
	}
}

fn invalid_reason(operation: RangeOperation, decision: RangeDecision) -> Option<&'static str> {
	if decision != RangeDecision::Invalid {
		return None;
	}
	Some(match operation {
		RangeOperation::Division | RangeOperation::Remainder => "integer division by zero",
		RangeOperation::Shift => "integer shift count must be in 0..63",
		RangeOperation::Power => "integer exponent must be nonnegative",
		_ => "integer operation overflows its result type",
	})
}

fn join(left: Value, right: Value) -> Value {
	let interval = match (left.interval, right.interval) {
		(Some((left_min, left_max)), Some((right_min, right_max))) => {
			Some((left_min.min(right_min), left_max.max(right_max)))
		}
		_ => None,
	};
	Value {
		interval,
		length: (left.length == right.length)
			.then_some(left.length)
			.flatten(),
		excludes_zero: left.excludes_zero && right.excludes_zero,
		symbol: None,
	}
}

fn signed_expression_symbol(expr: &Expr) -> Option<(EcoString, i8)> {
	match &expr.kind {
		ExprKind::Identifier(name) => Some((name.0.clone(), 1)),
		ExprKind::Grouped(inner) | ExprKind::TypeOp { lhs: inner, .. } => {
			signed_expression_symbol(inner)
		}
		ExprKind::PrefixOp {
			op: PrefixOperator::Negate,
			value,
		} => signed_expression_symbol(value).map(|(name, sign)| (name, -sign)),
		ExprKind::Call { func, .. }
			if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
				&& member.0 == "length"
				&& let ExprKind::Identifier(name) = &parent.kind =>
		{
			Some((EcoString::from(format!("length:{}", name.0)), 1))
		}
		_ => None,
	}
}

fn comparison_bound(op: BinaryOperator, truth: bool) -> Option<(i8, i8, i128)> {
	match (op, truth) {
		(BinaryOperator::LessThan, true) | (BinaryOperator::GreaterThanEquals, false) => {
			Some((1, -1, -1))
		}
		(BinaryOperator::LessThanEquals, true) | (BinaryOperator::GreaterThan, false) => {
			Some((1, -1, 0))
		}
		(BinaryOperator::GreaterThan, true) | (BinaryOperator::LessThanEquals, false) => {
			Some((-1, 1, -1))
		}
		(BinaryOperator::GreaterThanEquals, true) | (BinaryOperator::LessThan, false) => {
			Some((-1, 1, 0))
		}
		_ => None,
	}
}
