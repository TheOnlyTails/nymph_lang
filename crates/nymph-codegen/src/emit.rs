use oxc::{
	allocator::{Allocator, Box as ArenaBox, CloneIn, Vec as ArenaVec},
	ast::{AstBuilder, ast::*},
	codegen::Codegen,
	span::SPAN,
	syntax::number::BigintBase,
};

use ecow::EcoString;

use nymph_hir::hir::{
	BinOp, BuiltinResult, HirArm, HirArrayElem, HirArrayKind, HirBoundDispatchCase,
	HirBoundDispatchTarget, HirCallMode, HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirLit,
	HirMapElem, HirMethod, HirModule, HirOptionAbi, HirPat, HirRange, HirReturnTarget, HirStmt,
	NumKind, OperationMode, ScalarCastKind, UnOp,
};

use crate::EchoEmission;
use crate::box_rt;

fn external_alias(module: &str, symbol: &str, kind: &str) -> String {
	fn encode(value: &str) -> String {
		value
			.as_bytes()
			.iter()
			.map(|byte| format!("{byte:02x}"))
			.collect()
	}
	let module = encode(module);
	let symbol = encode(symbol);
	format!("$nymph_external${kind}${module}${symbol}")
}

/// A re-emittable reference to a sub-value of the scrutinee, used while compiling a
/// pattern. oxc expression nodes are arena values that can't be cheaply cloned, so
/// pattern bindings and tests carry a `Subject` (which re-emits a fresh expression
/// each time) instead of a built `Expression`.
#[derive(Clone)]
enum Subject {
	/// The scrutinee temporary, by name.
	Temp(String),
	/// `<base>.<field>`.
	Field(Box<Subject>, String),
	/// `<base>[<index>]`.
	Index(Box<Subject>, usize),
	/// `<base>[<base>.length - <offset>]` — a list element counted from the end
	/// (`offset` ≥ 1; the last element is offset 1).
	IndexFromEnd(Box<Subject>, usize),
	/// `<base>.get(<key>)` — a map value.
	MapGet(Box<Subject>, HirLit),
	/// `<base>.slice(<start>, <base>.length - <end_from_end>)` — a list rest slice
	/// (`end_from_end == 0` ⇒ `<base>.slice(<start>)`).
	Slice(Box<Subject>, usize, usize, HirArrayKind),
	/// The rest-of-map for a map pattern's `...rest`: a persistent map built by
	/// removing each named key from `<base>`.
	MapRest(Box<Subject>, Vec<HirLit>),
	/// Select the subject bound by the matching side of a union pattern. The
	/// decision is assigned while testing the union and shared by every binding,
	/// so neither the test nor either extraction plan is repeated.
	PatternSelect {
		decision: String,
		left: Box<Subject>,
		right: Box<Subject>,
	},
}

/// Intermediate representation for expression-valued code.
///
/// In Nymph, blocks (and eventually `if`/`while` in value position) are
/// expressions. When emitting to JS we may need to wrap them in an IIFE.
/// `JsValue` keeps the leading statements separate from the final expression
/// so the common case (no statements) can emit the expression directly.
struct JsValue<'a> {
	stmts: ArenaVec<'a, Statement<'a>>,
	expr: Expression<'a>,
}

struct MatchArmControl<'a> {
	guard: Option<Expression<'a>>,
	test: Option<Expression<'a>>,
	selection: Option<Expression<'a>>,
	label: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActivationLocation {
	frame: String,
	slot: usize,
}

#[derive(Clone, Debug)]
enum ActivationAction {
	Store {
		target: ActivationLocation,
		value: HirExpr,
	},
	RegisterCleanup(HirExpr),
	RegisterStateCleanup {
		cleanup: HirExpr,
		binding: String,
		value: ActivationLocation,
		handle: ActivationLocation,
	},
	CommitStateTransition {
		header_depth: usize,
		replacements: Vec<(ActivationLocation, ActivationLocation)>,
	},
	EnterCleanupScope,
	UnwindCleanupScopes(usize),
}

#[derive(Clone, Debug)]
enum ActivationTerminal {
	Goto(u32),
	Branch {
		condition: HirExpr,
		then_state: u32,
		else_state: u32,
	},
	Transfer {
		call: HirExpr,
		resume_state: u32,
		result_slot: usize,
	},
	Return(HirExpr),
}

#[derive(Clone, Debug)]
struct ActivationState {
	environment: std::collections::BTreeMap<String, ActivationLocation>,
	pattern_bindings: std::collections::BTreeSet<String>,
	actions: Vec<ActivationAction>,
	terminal: ActivationTerminal,
}

#[derive(Clone, Debug)]
struct ActivationPlan {
	frame: String,
	states: Vec<ActivationState>,
}

#[derive(Clone)]
struct ActivationLoop {
	target: nymph_hir::hir::LoopTarget,
	result: ActivationLocation,
	exit_state: u32,
	continue_state: u32,
	option: Option<HirOptionAbi>,
	cleanup_depth: usize,
	state: Option<ActivationStateLoop>,
}

#[derive(Clone, Debug)]
struct ActivationStateBinding {
	name: String,
	location: ActivationLocation,
	cleanup: Option<HirExpr>,
	cleanup_handle: Option<ActivationLocation>,
}

#[derive(Clone, Debug)]
struct ActivationStateLoop {
	bindings: Vec<ActivationStateBinding>,
	header_depth: usize,
	loop_head: u32,
}

#[derive(Clone)]
struct ActivationBlock {
	target: nymph_hir::hir::BlockTarget,
	result: ActivationLocation,
	exit_state: u32,
	cleanup_depth: usize,
}

fn pattern_binding_names(pattern: &HirPat, names: &mut std::collections::BTreeSet<EcoString>) {
	match pattern {
		HirPat::Wildcard | HirPat::Lit(_) | HirPat::Range(_) => {}
		HirPat::Binding { name, sub } => {
			names.insert(name.clone());
			if let Some(sub) = sub {
				pattern_binding_names(sub, names);
			}
		}
		HirPat::Variant { fields, .. } | HirPat::Struct { fields } => {
			for (_, pattern) in fields {
				pattern_binding_names(pattern, names);
			}
		}
		HirPat::Tuple(items) => {
			for pattern in items {
				pattern_binding_names(pattern, names);
			}
		}
		HirPat::List {
			prefix,
			rest,
			suffix,
			..
		} => {
			for pattern in prefix.iter().chain(suffix) {
				pattern_binding_names(pattern, names);
			}
			if let Some(Some(name)) = rest {
				names.insert(name.clone());
			}
		}
		HirPat::Map { entries, rest } => {
			for (_, pattern) in entries {
				pattern_binding_names(pattern, names);
			}
			if let Some(Some(name)) = rest {
				names.insert(name.clone());
			}
		}
		HirPat::Or(left, right) => {
			pattern_binding_names(left, names);
			pattern_binding_names(right, names);
		}
	}
}

fn activation_protocol_step(module: &str, symbol: &str) -> Option<&'static str> {
	match (module, symbol) {
		("std/io", "print") => Some("nymphPrintStep"),
		("std/io", "println") => Some("nymphPrintlnStep"),
		_ => None,
	}
}

fn requires_activation_split(expr: &HirExpr) -> bool {
	fn any(items: &[HirExpr]) -> bool {
		items.iter().any(requires_activation_split)
	}
	fn array(items: &[HirArrayElem]) -> bool {
		items.iter().any(|item| match item {
			HirArrayElem::Item(value) | HirArrayElem::Spread(value) => requires_activation_split(value),
		})
	}
	fn map(items: &[HirMapElem]) -> bool {
		items.iter().any(|item| match item {
			HirMapElem::Entry(key, value) => {
				requires_activation_split(key) || requires_activation_split(value)
			}
			HirMapElem::Spread(value) => requires_activation_split(value),
		})
	}
	match expr {
		HirExpr::Int(_)
		| HirExpr::UInt(_)
		| HirExpr::Num(..)
		| HirExpr::Str(_)
		| HirExpr::Bool(_)
		| HirExpr::Char(_)
		| HirExpr::Undefined
		| HirExpr::Local(_)
		| HirExpr::ExternValue { .. }
		| HirExpr::This
		| HirExpr::Closure { .. } => false,
		HirExpr::TaskRecipe { .. } => false,
		HirExpr::TaskOperation {
			operation,
			operands,
		} => operation.suspends() || any(operands),
		HirExpr::Continue { .. } | HirExpr::ContinueTransition { .. } => true,
		HirExpr::ActivationCall { .. }
		| HirExpr::StaticEnumDispatch { .. }
		| HirExpr::BoundDispatch { .. }
		| HirExpr::UnaryBoundDispatch { .. } => true,
		HirExpr::InterpolatedString(items) | HirExpr::Array { items, .. } => any(items),
		HirExpr::ProtocolDisplay(_) => true,
		HirExpr::ListConstruct(items) => array(items),
		HirExpr::RuntimeTypeObject { arguments, .. } => any(arguments),
		HirExpr::RuntimeTypeProjection { receiver, .. }
		| HirExpr::Field { recv: receiver, .. }
		| HirExpr::ScalarCast {
			operand: receiver, ..
		} => requires_activation_split(receiver),
		HirExpr::Echo { operand, .. } => requires_activation_split(operand),
		HirExpr::WithPrototype { value, prototype } => {
			requires_activation_split(value) || requires_activation_split(prototype)
		}
		HirExpr::RuntimeTypeAttachment { object, .. } => requires_activation_split(object),
		HirExpr::Binary { lhs, rhs, .. } => {
			requires_activation_split(lhs) || requires_activation_split(rhs)
		}
		HirExpr::Unary { operand, .. } => requires_activation_split(operand),
		HirExpr::Call { callee, args } => {
			requires_activation_split(callee) || args.iter().any(requires_activation_split)
		}
		HirExpr::ExternCall {
			module,
			symbol,
			args,
			..
		} => activation_protocol_step(module, symbol).is_some() || any(args),
		HirExpr::ListRead { recv, index, .. } | HirExpr::Index { recv, index, .. } => {
			requires_activation_split(recv) || requires_activation_split(index)
		}
		HirExpr::ListAppend { recv, value } => {
			requires_activation_split(recv) || requires_activation_split(value)
		}
		HirExpr::ListReplace { recv, index, value } => {
			requires_activation_split(recv)
				|| requires_activation_split(index)
				|| requires_activation_split(value)
		}
		HirExpr::ListSlice { recv, start, end } => {
			requires_activation_split(recv)
				|| requires_activation_split(start)
				|| requires_activation_split(end)
		}
		HirExpr::ArraySpread { elems, .. } => array(elems),
		HirExpr::MapLit(entries) => entries
			.iter()
			.any(|(key, value)| requires_activation_split(key) || requires_activation_split(value)),
		HirExpr::MapSpread(items) => map(items),
		HirExpr::Slice {
			recv, start, end, ..
		} => {
			requires_activation_split(recv)
				|| start.as_deref().is_some_and(requires_activation_split)
				|| end.as_deref().is_some_and(requires_activation_split)
		}
		HirExpr::MapGet { recv, key } => {
			requires_activation_split(recv) || requires_activation_split(key)
		}
		HirExpr::New {
			fields, prototype, ..
		}
		| HirExpr::StructFresh {
			fields, prototype, ..
		}
		| HirExpr::VariantNew {
			fields, prototype, ..
		} => {
			fields
				.iter()
				.any(|(_, value)| requires_activation_split(value))
				|| prototype.as_deref().is_some_and(requires_activation_split)
		}
		HirExpr::StructCloneUpdate {
			source,
			replacements,
			prototype,
			..
		} => {
			requires_activation_split(source)
				|| replacements
					.iter()
					.any(|(_, value)| requires_activation_split(value))
				|| prototype.as_deref().is_some_and(requires_activation_split)
		}
		HirExpr::VariantRef { prototype, .. } => {
			prototype.as_deref().is_some_and(requires_activation_split)
		}
		HirExpr::Block { stmts, tail } => {
			stmts.iter().any(|stmt| match stmt {
				HirStmt::Let { value, cleanup, .. } => {
					cleanup.is_some() || requires_activation_split(value)
				}
				HirStmt::Expr(value) => requires_activation_split(value),
				HirStmt::Return { .. } => true,
			}) || tail.as_deref().is_some_and(requires_activation_split)
		}
		HirExpr::LabeledBlock { body, .. } => requires_activation_split(body),
		HirExpr::If {
			cond,
			then,
			otherwise,
		} => {
			requires_activation_split(cond)
				|| requires_activation_split(then)
				|| otherwise.as_deref().is_some_and(requires_activation_split)
		}
		HirExpr::StateLoop { .. } => true,
		HirExpr::For {
			iterator,
			next,
			body,
			..
		} => {
			requires_activation_split(iterator)
				|| requires_activation_split(next)
				|| requires_activation_split(body)
		}
		HirExpr::Break { .. } => true,
		HirExpr::Match { scrutinee, arms } => {
			requires_activation_split(scrutinee)
				|| arms.iter().any(|arm| {
					arm.guard.as_ref().is_some_and(requires_activation_split)
						|| requires_activation_split(&arm.body)
				})
		}
	}
}

struct ActivationPlanner<'e, 'a> {
	emitter: &'e Emitter<'a>,
	frame: String,
	next_slot: usize,
	source_scopes: Vec<std::collections::BTreeMap<String, ActivationLocation>>,
	temporaries: std::collections::BTreeMap<String, ActivationLocation>,
	states: Vec<Option<ActivationState>>,
	loops: Vec<ActivationLoop>,
	blocks: Vec<ActivationBlock>,
	cleanup_depth: usize,
}

impl<'e, 'a> ActivationPlanner<'e, 'a> {
	fn new(
		emitter: &'e Emitter<'a>,
		params: &[EcoString],
		outer: std::collections::BTreeMap<String, ActivationLocation>,
	) -> Self {
		let frame = emitter.gensym();
		let mut parameters = std::collections::BTreeMap::new();
		for (slot, param) in params.iter().enumerate() {
			parameters.insert(
				param.to_string(),
				ActivationLocation {
					frame: frame.clone(),
					slot,
				},
			);
		}
		Self {
			emitter,
			frame,
			next_slot: params.len(),
			source_scopes: vec![outer, parameters],
			temporaries: std::collections::BTreeMap::new(),
			states: vec![None],
			loops: Vec::new(),
			blocks: Vec::new(),
			cleanup_depth: 1,
		}
	}

	fn finish(mut self, body: &HirExpr) -> ActivationPlan {
		let result = self.temporary();
		let terminal = self.state(Vec::new(), ActivationTerminal::Return(self.local(&result)));
		let entry = self.compile_expr(body, result, terminal);
		self.states[0] = Some(self.activation_state(Vec::new(), ActivationTerminal::Goto(entry)));
		ActivationPlan {
			frame: self.frame,
			states: self
				.states
				.into_iter()
				.map(|state| state.expect("every activation state is initialized"))
				.collect(),
		}
	}

	fn activation_state(
		&self,
		actions: Vec<ActivationAction>,
		terminal: ActivationTerminal,
	) -> ActivationState {
		ActivationState {
			environment: self.environment(),
			pattern_bindings: std::collections::BTreeSet::new(),
			actions,
			terminal,
		}
	}

	fn environment(&self) -> std::collections::BTreeMap<String, ActivationLocation> {
		let mut environment = std::collections::BTreeMap::new();
		for scope in &self.source_scopes {
			environment.extend(scope.clone());
		}
		environment.extend(self.temporaries.clone());
		environment
	}

	fn state(&mut self, actions: Vec<ActivationAction>, terminal: ActivationTerminal) -> u32 {
		let id = self.states.len() as u32;
		self
			.states
			.push(Some(self.activation_state(actions, terminal)));
		id
	}

	fn reserve_state(&mut self) -> u32 {
		let id = self.states.len() as u32;
		self.states.push(None);
		id
	}

	fn fill_state(&mut self, id: u32, actions: Vec<ActivationAction>, terminal: ActivationTerminal) {
		let state = self.activation_state(actions, terminal);
		let target = self
			.states
			.get_mut(id as usize)
			.expect("reserved activation state exists");
		assert!(
			target.replace(state).is_none(),
			"activation state is filled once"
		);
	}

	fn push_scope(&mut self) {
		self.source_scopes.push(std::collections::BTreeMap::new());
	}

	fn pop_scope(&mut self) {
		assert!(self.source_scopes.pop().is_some());
	}

	fn location(&mut self) -> ActivationLocation {
		let location = ActivationLocation {
			frame: self.frame.clone(),
			slot: self.next_slot,
		};
		self.next_slot += 1;
		location
	}

	fn temporary(&mut self) -> ActivationLocation {
		let name = self.emitter.gensym();
		let location = self.location();
		self.temporaries.insert(name, location.clone());
		location
	}

	fn temporary_named(&mut self) -> (EcoString, ActivationLocation) {
		let name: EcoString = self.emitter.gensym().into();
		let location = self.location();
		self.temporaries.insert(name.to_string(), location.clone());
		(name, location)
	}

	fn bind(&mut self, name: &str) -> ActivationLocation {
		let location = self.location();
		self
			.source_scopes
			.last_mut()
			.expect("activation source scope")
			.insert(name.to_string(), location.clone());
		location
	}

	fn local(&self, location: &ActivationLocation) -> HirExpr {
		let name = self
			.environment()
			.into_iter()
			.find_map(|(name, candidate)| (candidate == *location).then_some(name))
			.expect("activation location has a compiler-local name");
		HirExpr::Local(name.into())
	}

	fn store_state(&mut self, target: ActivationLocation, value: HirExpr, next: u32) -> u32 {
		self.state(
			vec![ActivationAction::Store { target, value }],
			ActivationTerminal::Goto(next),
		)
	}

	fn compile_values<F>(
		&mut self,
		values: Vec<&HirExpr>,
		target: ActivationLocation,
		next: u32,
		build: F,
	) -> u32
	where
		F: FnOnce(Vec<HirExpr>) -> HirExpr,
	{
		let temporaries = values
			.iter()
			.map(|_| self.temporary_named())
			.collect::<Vec<_>>();
		let rebuilt = build(
			temporaries
				.iter()
				.map(|(name, _)| HirExpr::Local(name.clone()))
				.collect(),
		);
		let mut entry = self.store_state(target, rebuilt, next);
		for (value, (_, temporary)) in values.into_iter().zip(temporaries).rev() {
			entry = self.compile_expr(value, temporary, entry);
		}
		entry
	}

	fn compile_transfer<F>(
		&mut self,
		values: Vec<&HirExpr>,
		target: ActivationLocation,
		next: u32,
		build: F,
	) -> u32
	where
		F: FnOnce(Vec<HirExpr>) -> HirExpr,
	{
		let temporaries = values
			.iter()
			.map(|_| self.temporary_named())
			.collect::<Vec<_>>();
		let call = build(
			temporaries
				.iter()
				.map(|(name, _)| HirExpr::Local(name.clone()))
				.collect(),
		);
		let mut entry = self.state(
			Vec::new(),
			ActivationTerminal::Transfer {
				call,
				resume_state: next,
				result_slot: target.slot,
			},
		);
		for (value, (_, temporary)) in values.into_iter().zip(temporaries).rev() {
			entry = self.compile_expr(value, temporary, entry);
		}
		entry
	}

	fn compile_expr(&mut self, expr: &HirExpr, target: ActivationLocation, next: u32) -> u32 {
		if let HirExpr::Call { callee, args } = expr {
			return self.compile_activation_call(callee, args, HirCallMode::Push, 0, target, next);
		}
		if let HirExpr::ProtocolDisplay(value) = expr {
			self
				.emitter
				.box_runtime_bindings
				.borrow_mut()
				.insert("nymphProtocolDisplayStep".to_string());
			return self.compile_transfer(vec![value], target, next, |mut args| {
				HirExpr::ActivationCall {
					callee: Box::new(HirExpr::Local("nymphProtocolDisplayStep".into())),
					args: vec![args.remove(0)],
					mode: HirCallMode::Push,
					source: 0,
				}
			});
		}
		if let HirExpr::ExternCall {
			module,
			symbol,
			args,
			..
		} = expr
		{
			let step = activation_protocol_step(module, symbol);
			if let Some(step) = step {
				self
					.emitter
					.box_runtime_bindings
					.borrow_mut()
					.insert(step.to_string());
				let step: EcoString = step.into();
				return self.compile_transfer(args.iter().collect(), target, next, move |args| {
					HirExpr::ActivationCall {
						callee: Box::new(HirExpr::Local(step)),
						args,
						mode: HirCallMode::Push,
						source: 0,
					}
				});
			}
		}
		if let HirExpr::TaskOperation {
			operation,
			operands,
		} = expr
			&& operation.suspends()
		{
			let operation = *operation;
			return self.compile_transfer(operands.iter().collect(), target, next, move |operands| {
				HirExpr::TaskOperation {
					operation,
					operands,
				}
			});
		}
		let control = matches!(
			expr,
			HirExpr::Block { .. }
				| HirExpr::LabeledBlock { .. }
				| HirExpr::If { .. }
				| HirExpr::For { .. }
				| HirExpr::Break { .. }
				| HirExpr::Continue { .. }
				| HirExpr::Match { .. }
				| HirExpr::StructFresh { .. }
		);
		if !control && !requires_activation_split(expr) {
			return self.store_state(target, expr.clone(), next);
		}
		match expr {
			HirExpr::ActivationCall {
				callee,
				args,
				mode,
				source,
			} => self.compile_activation_call(callee, args, *mode, *source, target, next),
			HirExpr::StaticEnumDispatch {
				owner,
				method,
				receiver,
				args,
				mode,
				source,
			} => {
				let owner = owner.clone();
				let method = method.clone();
				let mode = *mode;
				let source = *source;
				let mut values = vec![receiver.as_ref()];
				values.extend(args.iter());
				self.compile_transfer(values, target, next, move |mut values| {
					let receiver = Box::new(values.remove(0));
					HirExpr::StaticEnumDispatch {
						owner,
						method,
						receiver,
						args: values,
						mode,
						source,
					}
				})
			}
			HirExpr::BoundDispatch {
				interface,
				method,
				receiver,
				argument,
				hidden_arguments,
				cases,
				mode,
				source,
			} => {
				let interface = interface.clone();
				let method = method.clone();
				let cases = cases.clone();
				let mode = *mode;
				let source = *source;
				let mut values = vec![receiver.as_ref(), argument.as_ref()];
				values.extend(hidden_arguments.iter());
				self.compile_transfer(values, target, next, move |mut values| {
					let receiver = Box::new(values.remove(0));
					let argument = Box::new(values.remove(0));
					HirExpr::BoundDispatch {
						interface,
						method,
						receiver,
						argument,
						hidden_arguments: values,
						cases,
						mode,
						source,
					}
				})
			}
			HirExpr::UnaryBoundDispatch {
				interface,
				method,
				receiver,
				hidden_arguments,
				cases,
				mode,
				source,
			} => {
				let interface = interface.clone();
				let method = method.clone();
				let cases = cases.clone();
				let mode = *mode;
				let source = *source;
				let mut values = vec![receiver.as_ref()];
				values.extend(hidden_arguments.iter());
				self.compile_transfer(values, target, next, move |mut values| {
					let receiver = Box::new(values.remove(0));
					HirExpr::UnaryBoundDispatch {
						interface,
						method,
						receiver,
						hidden_arguments: values,
						cases,
						mode,
						source,
					}
				})
			}
			HirExpr::Block { stmts, tail } => self.compile_block(stmts, tail.as_deref(), target, next),
			HirExpr::LabeledBlock {
				target: block_target,
				body,
			} => {
				self.blocks.push(ActivationBlock {
					target: *block_target,
					result: target.clone(),
					exit_state: next,
					cleanup_depth: self.cleanup_depth,
				});
				let entry = self.compile_expr(body, target, next);
				self.blocks.pop();
				entry
			}
			HirExpr::If {
				cond,
				then,
				otherwise,
			} => {
				let then_state = self.compile_expr(then, target.clone(), next);
				let else_state = if let Some(otherwise) = otherwise {
					self.compile_expr(otherwise, target.clone(), next)
				} else {
					self.store_state(target.clone(), HirExpr::Undefined, next)
				};
				let (condition_name, condition) = self.temporary_named();
				let branch = self.state(
					Vec::new(),
					ActivationTerminal::Branch {
						condition: HirExpr::Local(condition_name),
						then_state,
						else_state,
					},
				);
				self.compile_expr(cond, condition, branch)
			}
			HirExpr::StateLoop {
				target: loop_target,
				bindings,
				body,
			} => self.compile_state_loop(*loop_target, bindings, body, target, next),
			HirExpr::For {
				target: loop_target,
				iterator_name,
				successor_name,
				iterator,
				next: next_step,
				pat,
				body,
				iteration,
				option,
				..
			} => self.compile_for(
				*loop_target,
				iterator_name,
				successor_name,
				iterator,
				next_step,
				pat,
				body,
				iteration,
				option.as_ref(),
				target,
				next,
			),
			HirExpr::Break {
				target: loop_target,
				value,
			} => self.compile_break(*loop_target, value),
			HirExpr::Continue {
				target: loop_target,
			} => {
				let loop_ = self
					.loops
					.iter()
					.rev()
					.find(|loop_| loop_.target == *loop_target)
					.cloned()
					.expect("continue target is active");
				self.state(
					vec![ActivationAction::UnwindCleanupScopes(loop_.cleanup_depth)],
					ActivationTerminal::Goto(loop_.continue_state),
				)
			}
			HirExpr::ContinueTransition {
				target: loop_target,
				replacements,
			} => {
				let loop_ = self
					.loops
					.iter()
					.rev()
					.find(|loop_| loop_.target == *loop_target)
					.cloned()
					.expect("state-loop continue target is active");
				let state = loop_
					.state
					.expect("continue transition targets a state loop");
				self.compile_state_transition(&state, replacements)
			}
			HirExpr::Match { scrutinee, arms } => self.compile_match(scrutinee, arms, target, next),
			HirExpr::Binary {
				op: op @ (BinOp::And | BinOp::Or),
				lhs,
				rhs,
				..
			} => self.compile_logical(*op, lhs, rhs, target, next),
			_ => self.compile_composite(expr, target, next),
		}
	}

	fn compile_activation_call(
		&mut self,
		callee: &HirExpr,
		args: &[HirExpr],
		mode: HirCallMode,
		source: u32,
		target: ActivationLocation,
		next: u32,
	) -> u32 {
		if let HirExpr::Field { recv, name } = callee {
			let name = name.clone();
			let mut values = vec![recv.as_ref()];
			values.extend(args.iter());
			return self.compile_transfer(values, target, next, move |mut values| {
				let recv = Box::new(values.remove(0));
				HirExpr::ActivationCall {
					callee: Box::new(HirExpr::Field { recv, name }),
					args: values,
					mode,
					source,
				}
			});
		}
		let mut values = vec![callee];
		values.extend(args.iter());
		self.compile_transfer(values, target, next, move |mut values| {
			HirExpr::ActivationCall {
				callee: Box::new(values.remove(0)),
				args: values,
				mode,
				source,
			}
		})
	}

	fn compile_block(
		&mut self,
		stmts: &[HirStmt],
		tail: Option<&HirExpr>,
		target: ActivationLocation,
		next: u32,
	) -> u32 {
		self.push_scope();
		let managed = stmts.iter().any(|stmt| {
			matches!(
				stmt,
				HirStmt::Let {
					cleanup: Some(_),
					..
				}
			)
		});
		let outer_depth = self.cleanup_depth;
		let normal_next = if managed {
			self.state(
				vec![ActivationAction::UnwindCleanupScopes(outer_depth)],
				ActivationTerminal::Goto(next),
			)
		} else {
			next
		};
		if managed {
			self.cleanup_depth += 1;
		}
		let body = self.compile_statements(stmts, 0, tail, target, normal_next);
		self.cleanup_depth = outer_depth;
		self.pop_scope();
		if managed {
			self.state(
				vec![ActivationAction::EnterCleanupScope],
				ActivationTerminal::Goto(body),
			)
		} else {
			body
		}
	}

	fn compile_statements(
		&mut self,
		stmts: &[HirStmt],
		index: usize,
		tail: Option<&HirExpr>,
		target: ActivationLocation,
		next: u32,
	) -> u32 {
		let Some(stmt) = stmts.get(index) else {
			return if let Some(tail) = tail {
				self.compile_expr(tail, target, next)
			} else {
				self.store_state(target, HirExpr::Undefined, next)
			};
		};
		match stmt {
			HirStmt::Let {
				name,
				value,
				cleanup,
				..
			} => {
				let before_binding = self.source_scopes.clone();
				let binding = self.bind(name);
				let remainder = self.compile_statements(stmts, index + 1, tail, target, next);
				let after_binding = self.source_scopes.clone();
				let (value_name, value_slot) = self.temporary_named();
				let mut actions = vec![ActivationAction::Store {
					target: binding,
					value: HirExpr::Local(value_name),
				}];
				if let Some(cleanup) = cleanup {
					actions.push(ActivationAction::RegisterCleanup(cleanup.clone()));
				}
				let initialize = self.state(actions, ActivationTerminal::Goto(remainder));
				self.source_scopes = before_binding;
				let entry = self.compile_expr(value, value_slot, initialize);
				self.source_scopes = after_binding;
				entry
			}
			HirStmt::Expr(expr) => {
				let remainder = self.compile_statements(stmts, index + 1, tail, target, next);
				let discarded = self.temporary();
				self.compile_expr(expr, discarded, remainder)
			}
			HirStmt::Return { value, target } => self.compile_return(value.as_ref(), *target),
		}
	}

	fn compile_return(&mut self, value: Option<&HirExpr>, target: HirReturnTarget) -> u32 {
		match target {
			HirReturnTarget::Callable => {
				let (name, result) = self.temporary_named();
				let terminal = self.state(Vec::new(), ActivationTerminal::Return(HirExpr::Local(name)));
				if let Some(value) = value {
					self.compile_expr(value, result, terminal)
				} else {
					self.store_state(result, HirExpr::Undefined, terminal)
				}
			}
			HirReturnTarget::Block(target) => {
				let block = self
					.blocks
					.iter()
					.rev()
					.find(|block| block.target == target)
					.cloned()
					.expect("block return target is active");
				let (name, result) = self.temporary_named();
				let exit = self.state(
					vec![
						ActivationAction::Store {
							target: block.result,
							value: HirExpr::Local(name),
						},
						ActivationAction::UnwindCleanupScopes(block.cleanup_depth),
					],
					ActivationTerminal::Goto(block.exit_state),
				);
				if let Some(value) = value {
					self.compile_expr(value, result, exit)
				} else {
					self.store_state(result, HirExpr::Undefined, exit)
				}
			}
		}
	}

	fn compile_logical(
		&mut self,
		op: BinOp,
		lhs: &HirExpr,
		rhs: &HirExpr,
		target: ActivationLocation,
		next: u32,
	) -> u32 {
		let rhs_state = self.compile_expr(rhs, target.clone(), next);
		let (lhs_name, lhs_slot) = self.temporary_named();
		let short = self.store_state(target, HirExpr::Local(lhs_name.clone()), next);
		let (then_state, else_state) = if op == BinOp::And {
			(rhs_state, short)
		} else {
			(short, rhs_state)
		};
		let branch = self.state(
			Vec::new(),
			ActivationTerminal::Branch {
				condition: HirExpr::Local(lhs_name),
				then_state,
				else_state,
			},
		);
		self.compile_expr(lhs, lhs_slot, branch)
	}

	fn compile_for(
		&mut self,
		target_id: nymph_hir::hir::LoopTarget,
		iterator_name: &EcoString,
		successor_name: &EcoString,
		iterator: &HirExpr,
		next_step: &HirExpr,
		pattern: &HirPat,
		body: &HirExpr,
		iteration: &nymph_hir::hir::HirIterationAbi,
		option: Option<&HirOptionAbi>,
		target: ActivationLocation,
		next: u32,
	) -> u32 {
		self.push_scope();
		let iterator_location = self.bind(iterator_name);
		let item_name: EcoString = self.emitter.gensym().into();
		self.bind(&item_name);
		self.bind(successor_name);

		let normal_value = option.map_or(HirExpr::Undefined, |option| HirExpr::VariantRef {
			enum_name: option.enum_name.clone(),
			variant: option.none.clone(),
			prototype: None,
		});
		let normal_exit = self.store_state(target.clone(), normal_value, next);
		let loop_head = self.reserve_state();
		self.loops.push(ActivationLoop {
			target: target_id,
			result: target.clone(),
			exit_state: next,
			continue_state: loop_head,
			option: option.cloned(),
			cleanup_depth: self.cleanup_depth,
			state: None,
		});
		let discarded = self.temporary();
		let body_entry = self.compile_expr(
			&HirExpr::Match {
				scrutinee: Box::new(HirExpr::Local(item_name.clone())),
				arms: vec![HirArm {
					pat: pattern.clone(),
					guard: None,
					body: body.clone(),
				}],
			},
			discarded,
			loop_head,
		);
		self.loops.pop();
		let advance = self.state(
			vec![ActivationAction::Store {
				target: iterator_location.clone(),
				value: HirExpr::Local(successor_name.clone()),
			}],
			ActivationTerminal::Goto(body_entry),
		);

		let (matched_name, matched_slot) = self.temporary_named();
		let branch = self.state(
			Vec::new(),
			ActivationTerminal::Branch {
				condition: HirExpr::Local(matched_name),
				then_state: advance,
				else_state: normal_exit,
			},
		);
		let (step_name, step_slot) = self.temporary_named();
		let matched = HirExpr::Match {
			scrutinee: Box::new(HirExpr::Local(step_name)),
			arms: vec![
				HirArm {
					pat: HirPat::Variant {
						enum_name: iteration.enum_name.clone(),
						variant: iteration.yield_.clone(),
						fields: vec![
							(
								iteration.item.clone(),
								HirPat::Binding {
									name: item_name.clone(),
									sub: None,
								},
							),
							(
								iteration.next.clone(),
								HirPat::Binding {
									name: successor_name.clone(),
									sub: None,
								},
							),
						],
					},
					guard: None,
					body: HirExpr::Bool(true),
				},
				HirArm {
					pat: HirPat::Wildcard,
					guard: None,
					body: HirExpr::Bool(false),
				},
			],
		};
		let attempt = self.store_state(matched_slot, matched, branch);
		self.states[attempt as usize]
			.as_mut()
			.expect("iterator step state")
			.pattern_bindings = [item_name.to_string(), successor_name.to_string()]
			.into_iter()
			.collect();
		let next_entry = self.compile_expr(next_step, step_slot, attempt);
		self.fill_state(loop_head, Vec::new(), ActivationTerminal::Goto(next_entry));

		let (initial_name, initial_slot) = self.temporary_named();
		let initialized = self.state(
			vec![ActivationAction::Store {
				target: iterator_location,
				value: HirExpr::Local(initial_name),
			}],
			ActivationTerminal::Goto(loop_head),
		);
		let entry = self.compile_expr(iterator, initial_slot, initialized);
		self.pop_scope();
		entry
	}

	fn compile_state_loop(
		&mut self,
		target_id: nymph_hir::hir::LoopTarget,
		bindings: &[nymph_hir::hir::HirStateBinding],
		body: &HirExpr,
		target: ActivationLocation,
		next: u32,
	) -> u32 {
		self.push_scope();
		let outer_depth = self.cleanup_depth;
		self.cleanup_depth += 1;
		let loop_head = self.reserve_state();
		let state_bindings = bindings
			.iter()
			.map(|binding| ActivationStateBinding {
				name: binding.name.to_string(),
				location: self.bind(&binding.name),
				cleanup: binding.cleanup.clone(),
				cleanup_handle: binding.cleanup.as_ref().map(|_| self.temporary()),
			})
			.collect::<Vec<_>>();
		let state = ActivationStateLoop {
			bindings: state_bindings,
			header_depth: self.cleanup_depth,
			loop_head,
		};
		let normal_continue = self.compile_state_transition(&state, &[]);
		self.loops.push(ActivationLoop {
			target: target_id,
			result: target,
			exit_state: next,
			continue_state: normal_continue,
			option: None,
			cleanup_depth: outer_depth,
			state: Some(state.clone()),
		});
		let discarded = self.temporary();
		let body_entry = self.compile_expr(body, discarded, normal_continue);
		self.loops.pop();
		self.fill_state(loop_head, Vec::new(), ActivationTerminal::Goto(body_entry));

		let mut initialize = loop_head;
		for (binding, source) in state.bindings.iter().zip(bindings).rev() {
			let value = self.temporary();
			let mut actions = vec![ActivationAction::Store {
				target: binding.location.clone(),
				value: self.local(&value),
			}];
			if let (Some(cleanup), Some(handle)) = (&binding.cleanup, &binding.cleanup_handle) {
				actions.push(ActivationAction::RegisterStateCleanup {
					cleanup: cleanup.clone(),
					binding: binding.name.clone(),
					value: binding.location.clone(),
					handle: handle.clone(),
				});
			}
			let installed = self.state(actions, ActivationTerminal::Goto(initialize));
			initialize = self.compile_expr(&source.value, value, installed);
		}
		self.cleanup_depth = outer_depth;
		self.pop_scope();
		self.state(
			vec![ActivationAction::EnterCleanupScope],
			ActivationTerminal::Goto(initialize),
		)
	}

	fn compile_state_transition(
		&mut self,
		state: &ActivationStateLoop,
		replacements: &[(EcoString, HirExpr)],
	) -> u32 {
		let values = state
			.bindings
			.iter()
			.map(|_| self.temporary())
			.collect::<Vec<_>>();
		let mut actions = Vec::new();
		let mut managed = Vec::new();
		for binding in &state.bindings {
			if replacements
				.iter()
				.any(|(name, _)| name.as_str() == binding.name)
				&& binding.cleanup.is_some()
			{
				let new_handle = self.temporary();
				managed.push((
					binding.name.clone(),
					binding
						.cleanup_handle
						.clone()
						.expect("managed cleanup handle"),
					new_handle,
				));
			}
		}
		if managed.is_empty() {
			actions.push(ActivationAction::UnwindCleanupScopes(state.header_depth));
		} else {
			actions.push(ActivationAction::CommitStateTransition {
				header_depth: state.header_depth,
				replacements: managed
					.iter()
					.map(|(_, old, new)| (old.clone(), new.clone()))
					.collect(),
			});
			for (_, old, new) in &managed {
				actions.push(ActivationAction::Store {
					target: old.clone(),
					value: self.local(&new),
				});
			}
		}
		for (binding, value) in state.bindings.iter().zip(&values) {
			actions.push(ActivationAction::Store {
				target: binding.location.clone(),
				value: self.local(value),
			});
		}
		let commit = self.state(actions, ActivationTerminal::Goto(state.loop_head));
		let mut evaluate = commit;
		for (name, replacement) in replacements.iter().rev() {
			let index = state
				.bindings
				.iter()
				.position(|binding| binding.name == name.as_str())
				.expect("checked state replacement");
			if let Some((_, _, handle)) = managed
				.iter()
				.find(|(binding, _, _)| binding == name.as_str())
			{
				evaluate = self.state(
					vec![ActivationAction::RegisterStateCleanup {
						cleanup: state.bindings[index]
							.cleanup
							.clone()
							.expect("managed binding"),
						binding: state.bindings[index].name.clone(),
						value: values[index].clone(),
						handle: handle.clone(),
					}],
					ActivationTerminal::Goto(evaluate),
				);
			}
			evaluate = self.compile_expr(replacement, values[index].clone(), evaluate);
		}
		let mut snapshots = Vec::new();
		if !replacements.is_empty()
			&& state.bindings.iter().any(|binding| {
				replacements
					.iter()
					.any(|(name, _)| name.as_str() == binding.name)
					&& binding.cleanup.is_some()
			}) {
			snapshots.push(ActivationAction::EnterCleanupScope);
		}
		for (binding, value) in state.bindings.iter().zip(values) {
			snapshots.push(ActivationAction::Store {
				target: value,
				value: HirExpr::Local(binding.name.as_str().into()),
			});
		}
		self.state(snapshots, ActivationTerminal::Goto(evaluate))
	}

	fn compile_break(&mut self, target: nymph_hir::hir::LoopTarget, value: &HirExpr) -> u32 {
		let loop_ = self
			.loops
			.iter()
			.rev()
			.find(|loop_| loop_.target == target)
			.cloned()
			.expect("break target is active");
		let (value_name, value_slot) = self.temporary_named();
		let completed = if let Some(option) = loop_.option {
			HirExpr::VariantNew {
				enum_name: option.enum_name,
				variant: option.some,
				fields: vec![(option.some_value, HirExpr::Local(value_name))],
				prototype: None,
			}
		} else {
			HirExpr::Local(value_name)
		};
		let exit = self.state(
			vec![
				ActivationAction::Store {
					target: loop_.result,
					value: completed,
				},
				ActivationAction::UnwindCleanupScopes(loop_.cleanup_depth),
			],
			ActivationTerminal::Goto(loop_.exit_state),
		);
		self.compile_expr(value, value_slot, exit)
	}

	fn compile_match(
		&mut self,
		scrutinee: &HirExpr,
		arms: &[HirArm],
		target: ActivationLocation,
		next: u32,
	) -> u32 {
		let (scrutinee_name, scrutinee_slot) = self.temporary_named();
		// Refutable `for` bindings lower to a `Some(pattern)` arm that skips a
		// source item when its nested pattern does not match.
		let mut fallback = self.store_state(target.clone(), HirExpr::Undefined, next);
		for arm in arms.iter().rev() {
			self.push_scope();
			let mut bindings = std::collections::BTreeSet::new();
			pattern_binding_names(&arm.pat, &mut bindings);
			for binding in &bindings {
				self.bind(binding);
			}
			let body = self.compile_expr(&arm.body, target.clone(), next);
			let committed = if let Some(guard) = &arm.guard {
				let (guard_name, guard_slot) = self.temporary_named();
				let branch = self.state(
					Vec::new(),
					ActivationTerminal::Branch {
						condition: HirExpr::Local(guard_name),
						then_state: body,
						else_state: fallback,
					},
				);
				self.compile_expr(guard, guard_slot, branch)
			} else {
				body
			};
			let (matched_name, matched_slot) = self.temporary_named();
			let matched = HirExpr::Match {
				scrutinee: Box::new(HirExpr::Local(scrutinee_name.clone())),
				arms: vec![
					HirArm {
						pat: arm.pat.clone(),
						guard: None,
						body: HirExpr::Bool(true),
					},
					HirArm {
						pat: HirPat::Wildcard,
						guard: None,
						body: HirExpr::Bool(false),
					},
				],
			};
			let branch = self.state(
				Vec::new(),
				ActivationTerminal::Branch {
					condition: HirExpr::Local(matched_name),
					then_state: committed,
					else_state: fallback,
				},
			);
			let attempt = self.store_state(matched_slot, matched, branch);
			self.states[attempt as usize]
				.as_mut()
				.expect("match attempt state")
				.pattern_bindings = bindings.iter().map(ToString::to_string).collect();
			fallback = attempt;
			self.pop_scope();
		}
		self.compile_expr(scrutinee, scrutinee_slot, fallback)
	}

	fn compile_composite(&mut self, expr: &HirExpr, target: ActivationLocation, next: u32) -> u32 {
		match expr {
			HirExpr::InterpolatedString(items) => {
				let parts = items
					.iter()
					.map(|item| match item {
						HirExpr::Str(text) => Some(text.clone()),
						_ => None,
					})
					.collect::<Vec<_>>();
				let values = items
					.iter()
					.filter(|item| !matches!(item, HirExpr::Str(_)))
					.collect();
				self.compile_values(values, target, next, move |values| {
					let mut values = values.into_iter();
					HirExpr::InterpolatedString(
						parts
							.into_iter()
							.map(|part| {
								part.map_or_else(|| values.next().expect("interpolation value"), HirExpr::Str)
							})
							.collect(),
					)
				})
			}
			HirExpr::RuntimeTypeObject {
				binding,
				box_runtime,
				is_enum,
				arguments,
			} => {
				let binding = binding.clone();
				let box_runtime = *box_runtime;
				let is_enum = *is_enum;
				self.compile_values(arguments.iter().collect(), target, next, move |arguments| {
					HirExpr::RuntimeTypeObject {
						binding,
						box_runtime,
						is_enum,
						arguments,
					}
				})
			}
			HirExpr::RuntimeTypeProjection { receiver, path } => {
				let path = path.clone();
				self.compile_values(vec![receiver], target, next, move |mut values| {
					HirExpr::RuntimeTypeProjection {
						receiver: Box::new(values.remove(0)),
						path,
					}
				})
			}
			HirExpr::WithPrototype { value, prototype } => {
				self.compile_values(vec![value, prototype], target, next, |mut values| {
					HirExpr::WithPrototype {
						value: Box::new(values.remove(0)),
						prototype: Box::new(values.remove(0)),
					}
				})
			}
			HirExpr::RuntimeTypeAttachment { object, method } => {
				let method = method.clone();
				self.compile_values(vec![object], target, next, move |mut values| {
					HirExpr::RuntimeTypeAttachment {
						object: Box::new(values.remove(0)),
						method,
					}
				})
			}
			HirExpr::Call { callee, args } => {
				if let HirExpr::Field { recv, name } = callee.as_ref() {
					let name = name.clone();
					return self.compile_values(
						std::iter::once(recv.as_ref()).chain(args).collect(),
						target,
						next,
						move |mut values| HirExpr::Call {
							callee: Box::new(HirExpr::Field {
								recv: Box::new(values.remove(0)),
								name,
							}),
							args: values,
						},
					);
				}
				self.compile_values(
					std::iter::once(callee.as_ref()).chain(args).collect(),
					target,
					next,
					|mut values| HirExpr::Call {
						callee: Box::new(values.remove(0)),
						args: values,
					},
				)
			}
			HirExpr::ExternCall {
				module,
				symbol,
				args,
				call_mode,
				argument_marshals,
				return_marshal,
			} => {
				let module = *module;
				let symbol = *symbol;
				let call_mode = *call_mode;
				let argument_marshals = argument_marshals.clone();
				let return_marshal = *return_marshal;
				self.compile_values(args.iter().collect(), target, next, move |args| {
					HirExpr::ExternCall {
						module,
						symbol,
						args,
						call_mode,
						argument_marshals,
						return_marshal,
					}
				})
			}
			HirExpr::ListConstruct(items) => {
				let spread = items
					.iter()
					.map(|item| matches!(item, HirArrayElem::Spread(_)))
					.collect::<Vec<_>>();
				let values = items
					.iter()
					.map(|item| match item {
						HirArrayElem::Item(value) | HirArrayElem::Spread(value) => value,
					})
					.collect();
				self.compile_values(values, target, next, move |values| {
					HirExpr::ListConstruct(
						values
							.into_iter()
							.zip(spread)
							.map(|(value, spread)| {
								if spread {
									HirArrayElem::Spread(value)
								} else {
									HirArrayElem::Item(value)
								}
							})
							.collect(),
					)
				})
			}
			HirExpr::ListRead { recv, index, mode } => {
				let mode = *mode;
				self.compile_values(vec![recv, index], target, next, move |mut values| {
					HirExpr::ListRead {
						recv: Box::new(values.remove(0)),
						index: Box::new(values.remove(0)),
						mode,
					}
				})
			}
			HirExpr::ListAppend { recv, value } => {
				self.compile_values(vec![recv, value], target, next, |mut values| {
					HirExpr::ListAppend {
						recv: Box::new(values.remove(0)),
						value: Box::new(values.remove(0)),
					}
				})
			}
			HirExpr::ListReplace { recv, index, value } => {
				self.compile_values(vec![recv, index, value], target, next, |mut values| {
					HirExpr::ListReplace {
						recv: Box::new(values.remove(0)),
						index: Box::new(values.remove(0)),
						value: Box::new(values.remove(0)),
					}
				})
			}
			HirExpr::ListSlice { recv, start, end } => {
				self.compile_values(vec![recv, start, end], target, next, |mut values| {
					HirExpr::ListSlice {
						recv: Box::new(values.remove(0)),
						start: Box::new(values.remove(0)),
						end: Box::new(values.remove(0)),
					}
				})
			}
			HirExpr::Array { kind, items } => {
				let kind = *kind;
				self.compile_values(items.iter().collect(), target, next, move |items| {
					HirExpr::Array { kind, items }
				})
			}
			HirExpr::ArraySpread { kind, elems } => {
				let kind = *kind;
				let spread = elems
					.iter()
					.map(|item| matches!(item, HirArrayElem::Spread(_)))
					.collect::<Vec<_>>();
				let values = elems
					.iter()
					.map(|item| match item {
						HirArrayElem::Item(value) | HirArrayElem::Spread(value) => value,
					})
					.collect();
				self.compile_values(values, target, next, move |values| HirExpr::ArraySpread {
					kind,
					elems: values
						.into_iter()
						.zip(spread)
						.map(|(value, spread)| {
							if spread {
								HirArrayElem::Spread(value)
							} else {
								HirArrayElem::Item(value)
							}
						})
						.collect(),
				})
			}
			HirExpr::MapLit(entries) => {
				let values = entries
					.iter()
					.flat_map(|(key, value)| [key, value])
					.collect();
				self.compile_values(values, target, next, |values| {
					HirExpr::MapLit(
						values
							.chunks_exact(2)
							.map(|pair| (pair[0].clone(), pair[1].clone()))
							.collect(),
					)
				})
			}
			HirExpr::MapSpread(items) => {
				let entry = items
					.iter()
					.map(|item| matches!(item, HirMapElem::Entry(_, _)))
					.collect::<Vec<_>>();
				let values = items
					.iter()
					.flat_map(|item| match item {
						HirMapElem::Entry(key, value) => vec![key, value],
						HirMapElem::Spread(value) => vec![value],
					})
					.collect();
				self.compile_values(values, target, next, move |values| {
					let mut values = values.into_iter();
					HirExpr::MapSpread(
						entry
							.into_iter()
							.map(|entry| {
								let first = values.next().expect("map element value");
								if entry {
									HirMapElem::Entry(first, values.next().expect("map entry value"))
								} else {
									HirMapElem::Spread(first)
								}
							})
							.collect(),
					)
				})
			}
			HirExpr::Index { recv, index, mode } => {
				let mode = *mode;
				self.compile_values(vec![recv, index], target, next, move |mut values| {
					HirExpr::Index {
						recv: Box::new(values.remove(0)),
						index: Box::new(values.remove(0)),
						mode,
					}
				})
			}
			HirExpr::Slice {
				recv,
				start,
				end,
				inclusive,
				string,
				mode,
			} => {
				let has_start = start.is_some();
				let has_end = end.is_some();
				let inclusive = *inclusive;
				let string = *string;
				let mode = *mode;
				let mut values = vec![recv.as_ref()];
				values.extend(start.as_deref());
				values.extend(end.as_deref());
				self.compile_values(values, target, next, move |mut values| HirExpr::Slice {
					recv: Box::new(values.remove(0)),
					start: has_start.then(|| Box::new(values.remove(0))),
					end: has_end.then(|| Box::new(values.remove(0))),
					inclusive,
					string,
					mode,
				})
			}
			HirExpr::MapGet { recv, key } => {
				self.compile_values(vec![recv, key], target, next, |mut values| {
					HirExpr::MapGet {
						recv: Box::new(values.remove(0)),
						key: Box::new(values.remove(0)),
					}
				})
			}
			HirExpr::New {
				class,
				fields,
				prototype,
			} => {
				let class = class.clone();
				let field_names = fields
					.iter()
					.map(|(name, _)| name.clone())
					.collect::<Vec<_>>();
				let has_prototype = prototype.is_some();
				let field_count = field_names.len();
				let mut values = fields.iter().map(|(_, value)| value).collect::<Vec<_>>();
				values.extend(prototype.as_deref());
				self.compile_values(values, target, next, move |mut values| HirExpr::New {
					class,
					fields: field_names
						.into_iter()
						.zip(values.drain(..field_count))
						.collect(),
					prototype: has_prototype.then(|| Box::new(values.remove(0))),
				})
			}
			HirExpr::StructFresh {
				class,
				fields,
				prototype,
			} => {
				let class = class.clone();
				let mut field_names = fields
					.iter()
					.map(|(name, _)| name.clone())
					.collect::<Vec<_>>();
				let has_prototype = prototype.is_some();
				let mut values = fields.iter().map(|(_, value)| value).collect::<Vec<_>>();
				let defaults = self
					.emitter
					.class_defaults
					.borrow()
					.get(class.as_str())
					.cloned()
					.unwrap_or_default();
				for (name, value) in &defaults {
					if !field_names.contains(name) {
						field_names.push(name.clone());
						values.push(value);
					}
				}
				let field_count = field_names.len();
				values.extend(prototype.as_deref());
				self.compile_values(values, target, next, move |mut values| {
					HirExpr::StructFresh {
						class,
						fields: field_names
							.into_iter()
							.zip(values.drain(..field_count))
							.collect(),
						prototype: has_prototype.then(|| Box::new(values.remove(0))),
					}
				})
			}
			HirExpr::StructCloneUpdate {
				class,
				source,
				replacements,
				prototype,
			} => {
				let class = class.clone();
				let names = replacements
					.iter()
					.map(|(name, _)| name.clone())
					.collect::<Vec<_>>();
				let has_prototype = prototype.is_some();
				let mut values = vec![source.as_ref()];
				values.extend(replacements.iter().map(|(_, value)| value));
				values.extend(prototype.as_deref());
				self.compile_values(values, target, next, move |mut values| {
					let source = Box::new(values.remove(0));
					let replacements = names
						.iter()
						.cloned()
						.zip(values.drain(..names.len()))
						.collect();
					HirExpr::StructCloneUpdate {
						class,
						source,
						replacements,
						prototype: has_prototype.then(|| Box::new(values.remove(0))),
					}
				})
			}
			HirExpr::Field { recv, name } => {
				let name = name.clone();
				self.compile_values(vec![recv], target, next, move |mut values| HirExpr::Field {
					recv: Box::new(values.remove(0)),
					name,
				})
			}
			HirExpr::VariantNew {
				enum_name,
				variant,
				fields,
				prototype,
			} => {
				let enum_name = enum_name.clone();
				let variant = variant.clone();
				let names = fields
					.iter()
					.map(|(name, _)| name.clone())
					.collect::<Vec<_>>();
				let has_prototype = prototype.is_some();
				let mut values = fields.iter().map(|(_, value)| value).collect::<Vec<_>>();
				values.extend(prototype.as_deref());
				self.compile_values(values, target, next, move |mut values| {
					let fields = names
						.iter()
						.cloned()
						.zip(values.drain(..names.len()))
						.collect();
					HirExpr::VariantNew {
						enum_name,
						variant,
						fields,
						prototype: has_prototype.then(|| Box::new(values.remove(0))),
					}
				})
			}
			HirExpr::VariantRef {
				enum_name,
				variant,
				prototype,
			} => {
				let enum_name = enum_name.clone();
				let variant = variant.clone();
				self.compile_values(
					prototype.iter().map(AsRef::as_ref).collect(),
					target,
					next,
					move |mut values| HirExpr::VariantRef {
						enum_name,
						variant,
						prototype: values.pop().map(Box::new),
					},
				)
			}
			HirExpr::Binary {
				op,
				result,
				mode,
				lhs,
				rhs,
			} => {
				let op = *op;
				let result = *result;
				let mode = *mode;
				self.compile_values(vec![lhs, rhs], target, next, move |mut values| {
					HirExpr::Binary {
						op,
						result,
						mode,
						lhs: Box::new(values.remove(0)),
						rhs: Box::new(values.remove(0)),
					}
				})
			}
			HirExpr::Unary {
				op,
				result,
				operand,
			} => {
				let op = *op;
				let result = *result;
				self.compile_values(vec![operand], target, next, move |mut values| {
					HirExpr::Unary {
						op,
						result,
						operand: Box::new(values.remove(0)),
					}
				})
			}
			HirExpr::ScalarCast {
				kind,
				operand,
				mode,
			} => {
				let kind = *kind;
				let mode = *mode;
				self.compile_values(vec![operand], target, next, move |mut values| {
					HirExpr::ScalarCast {
						kind,
						operand: Box::new(values.remove(0)),
						mode,
					}
				})
			}
			HirExpr::Echo { operand, site } => {
				let site = site.clone();
				self.compile_values(vec![operand], target, next, move |mut values| {
					HirExpr::Echo {
						operand: Box::new(values.remove(0)),
						site,
					}
				})
			}
			HirExpr::TaskOperation {
				operation,
				operands,
			} => {
				let operation = *operation;
				self.compile_values(operands.iter().collect(), target, next, move |operands| {
					HirExpr::TaskOperation {
						operation,
						operands,
					}
				})
			}
			HirExpr::Int(_)
			| HirExpr::UInt(_)
			| HirExpr::Num(..)
			| HirExpr::Str(_)
			| HirExpr::Bool(_)
			| HirExpr::Char(_)
			| HirExpr::Undefined
			| HirExpr::Local(_)
			| HirExpr::ExternValue { .. }
			| HirExpr::This
			| HirExpr::Closure { .. }
			| HirExpr::TaskRecipe { .. }
			| HirExpr::ProtocolDisplay(_)
			| HirExpr::ActivationCall { .. }
			| HirExpr::StaticEnumDispatch { .. }
			| HirExpr::BoundDispatch { .. }
			| HirExpr::UnaryBoundDispatch { .. }
			| HirExpr::Block { .. }
			| HirExpr::LabeledBlock { .. }
			| HirExpr::If { .. }
			| HirExpr::StateLoop { .. }
			| HirExpr::For { .. }
			| HirExpr::Break { .. }
			| HirExpr::Continue { .. }
			| HirExpr::ContinueTransition { .. }
			| HirExpr::Match { .. } => unreachable!("non-composite activation expression"),
		}
	}
}

/// The JavaScript representation selected for declarations that must be
/// registered with the transactional REPL runtime.
enum RepresentationPolicy {
	Direct,
	Transactional,
}

impl RepresentationPolicy {
	fn transactional() -> Self {
		Self::Transactional
	}

	fn is_transactional(&self) -> bool {
		matches!(self, Self::Transactional)
	}

	fn class_type(&self) -> ClassType {
		match self {
			Self::Direct => ClassType::ClassDeclaration,
			Self::Transactional => ClassType::ClassExpression,
		}
	}
}

impl<'a> JsValue<'a> {
	/// Collapse into a single JS expression.
	/// If there are leading statements, wrap in an IIFE:
	/// `(() => { ...stmts; return expr; })()`
	fn into_expression(self, ast: AstBuilder<'a>) -> Expression<'a> {
		if self.stmts.is_empty() {
			return self.expr;
		}

		let mut body_stmts = self.stmts;
		body_stmts.push(Statement::ReturnStatement(ReturnStatement::boxed(
			SPAN,
			Some(self.expr),
			&ast,
		)));

		let body = FunctionBody::new(SPAN, ArenaVec::new_in(&ast), body_stmts, &ast);
		let params = FormalParameters::new(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			ArenaVec::new_in(&ast),
			oxc::ast::NONE,
			&ast,
		);
		let arrow = Expression::ArrowFunctionExpression(ArrowFunctionExpression::boxed(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			body,
			&ast,
		));

		Expression::CallExpression(CallExpression::boxed(
			SPAN,
			arrow,
			oxc::ast::NONE,
			ArenaVec::new_in(&ast),
			false,
			&ast,
		))
	}
}

pub struct Emitter<'a> {
	ast: AstBuilder<'a>,
	/// Counter for fresh temporary names (result temporaries for value-position
	/// control flow). `Cell` keeps the emit methods `&self`.
	gensym: std::cell::Cell<u32>,
	/// Counter in a compiler-reserved namespace for completion packets and catch
	/// bindings. Nymph source identifiers cannot contain `$`, so these names
	/// cannot shadow user locals referenced from a generated completion scope.
	completion_gensym: std::cell::Cell<u32>,
	/// Set while emitting a control-flow expression's (`Block`/`If`/`While`/
	/// `Match`) generated IIFE body. Returns encountered there use the active
	/// callable's private completion token rather than targeting the IIFE.
	/// Save/restore around each use keeps nested callable and expression scopes
	/// independent.
	in_iife_subexpr: std::cell::Cell<bool>,
	/// Every `(module, symbol)` pair a `HirExpr::ExternCall` lowered during
	/// this emit run needs imported (Gap 3, L0) — populated by the
	/// `HirExpr::ExternCall` arm of [`Self::emit_expr`], drained by
	/// [`Self::emit_module`] into a deduped, deterministically-ordered
	/// `import { symbol } from "module";` per pair, prepended ahead of every
	/// other top-level statement. A `BTreeSet` (not a `HashSet`) so the
	/// prepended import order — and therefore the emitted JS text — stays
	/// stable across runs, which the golden/e2e tests rely on.
	needed_imports: std::cell::RefCell<std::collections::BTreeSet<(String, String, String)>>,
	/// Imports supplied by project linking rather than discovered from an
	/// `ExternCall`/`ExternValue`. Strict REPL validation subtracts these before
	/// applying the host-effect policy to the exact emitted external inventory.
	provided_imports: std::collections::BTreeSet<(String, String, String)>,
	/// Runtime bindings referenced by emitted code. Standalone emission prepends
	/// the inline runtime when this is non-empty; project emission imports only
	/// these bindings from the canonical `std/box` virtual module.
	box_runtime_bindings: std::cell::RefCell<std::collections::BTreeSet<String>>,
	/// Private per-loop completion tokens active while that loop's body is
	/// emitted. A generated local object, rather than a forgeable numeric id,
	/// distinguishes compiler control transfers from unrelated exceptions.
	loop_completion_tokens: std::cell::RefCell<Vec<(nymph_hir::hir::LoopTarget, &'a str)>>,
	/// Private completion tokens for returns that must cross a generated
	/// expression IIFE. The boolean records whether a callable actually needs
	/// the corresponding catch boundary.
	return_completion_tokens: std::cell::RefCell<Vec<(&'a str, bool)>>,
	block_completion_tokens: std::cell::RefCell<Vec<(nymph_hir::hir::BlockTarget, &'a str)>>,
	/// Source and compiler-local names visible while an explicit activation state
	/// is emitted. Each location names the retained frame and live-local slot
	/// that owns the value, including captured slots in an enclosing frame.
	activation_environment:
		std::cell::RefCell<std::collections::BTreeMap<String, ActivationLocation>>,
	/// Bindings introduced by the one-pattern match used to enter an activation
	/// arm. Those bindings are copied into retained frame slots instead of being
	/// left as native block locals.
	activation_pattern_bindings: std::cell::RefCell<std::collections::BTreeSet<String>>,
	class_defaults: std::cell::RefCell<std::collections::BTreeMap<String, Vec<(EcoString, HirExpr)>>>,
	import_box_runtime: bool,
	current_module: Option<String>,
	representation: RepresentationPolicy,
	echo_emission: EchoEmission,
}

impl<'a> Emitter<'a> {
	pub fn new(alloc: &'a Allocator) -> Self {
		Emitter {
			ast: AstBuilder::new(alloc),
			gensym: std::cell::Cell::new(0),
			completion_gensym: std::cell::Cell::new(0),
			in_iife_subexpr: std::cell::Cell::new(false),
			needed_imports: std::cell::RefCell::new(std::collections::BTreeSet::new()),
			provided_imports: std::collections::BTreeSet::new(),
			box_runtime_bindings: std::cell::RefCell::new(std::collections::BTreeSet::new()),
			loop_completion_tokens: std::cell::RefCell::new(Vec::new()),
			return_completion_tokens: std::cell::RefCell::new(Vec::new()),
			block_completion_tokens: std::cell::RefCell::new(Vec::new()),
			activation_environment: std::cell::RefCell::new(std::collections::BTreeMap::new()),
			activation_pattern_bindings: std::cell::RefCell::new(std::collections::BTreeSet::new()),
			class_defaults: std::cell::RefCell::new(std::collections::BTreeMap::new()),
			import_box_runtime: false,
			current_module: None,
			representation: RepresentationPolicy::Direct,
			echo_emission: EchoEmission::Development {
				source_name: "<expr>".to_string(),
				source_uri: None,
				source: String::new(),
			},
		}
	}

	pub fn for_module(alloc: &'a Allocator, module: &str) -> Self {
		let mut emitter = Self::new(alloc);
		emitter.current_module = Some(module.to_string());
		emitter.echo_emission = EchoEmission::Development {
			source_name: format!("{module}.nym"),
			source_uri: None,
			source: String::new(),
		};
		emitter
	}

	pub fn with_echo_emission(mut self, echo_emission: EchoEmission) -> Self {
		self.echo_emission = echo_emission;
		self
	}

	pub fn for_project_module(alloc: &'a Allocator, module: &str) -> Self {
		let mut emitter = Self::for_module(alloc, module);
		emitter.import_box_runtime = true;
		emitter
	}

	pub fn for_transactional_project_module(
		alloc: &'a Allocator,
		module: &str,
		_imported_top_level_lets: &[String],
	) -> Self {
		let mut emitter = Self::for_project_module(alloc, module);
		emitter.representation = RepresentationPolicy::transactional();
		emitter
	}

	fn transaction_call(&self, helper: &str, args: Vec<Expression<'a>>) -> Expression<'a> {
		self
			.box_runtime_bindings
			.borrow_mut()
			.insert(helper.to_string());
		let mut arguments = ArenaVec::new_in(&self.ast);
		arguments.extend(args.into_iter().map(Argument::from));
		Expression::new_call_expression(
			SPAN,
			Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(helper), &self.ast),
			oxc::ast::NONE,
			arguments,
			false,
			&self.ast,
		)
	}

	fn binding_declaration(&self, name: &str, init: Expression<'a>) -> Statement<'a> {
		let kind = VariableDeclarationKind::Const;
		let pat =
			BindingPattern::new_binding_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast);
		let declarator = VariableDeclarator::new(
			SPAN,
			kind,
			pat,
			oxc::ast::NONE,
			Some(init),
			false,
			&self.ast,
		);
		let decl = VariableDeclaration::new(
			SPAN,
			kind,
			ArenaVec::from_value_in(declarator, &self.ast),
			false,
			&self.ast,
		);
		Statement::from(Declaration::VariableDeclaration(ArenaBox::new_in(
			decl, &self.ast,
		)))
	}

	fn local_read(&self, name: &str) -> Expression<'a> {
		if let Some(location) = self.activation_environment.borrow().get(name).cloned() {
			return self.activation_slot(&location);
		}
		Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast)
	}

	fn activation_frame_member(&self, frame: &str, member: &str) -> Expression<'a> {
		Expression::new_static_member_expression(
			SPAN,
			Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(frame), &self.ast),
			IdentifierName::new(SPAN, self.ast.allocator.alloc_str(member), &self.ast),
			false,
			&self.ast,
		)
	}

	fn activation_slot(&self, location: &ActivationLocation) -> Expression<'a> {
		Expression::from(MemberExpression::ComputedMemberExpression(
			ComputedMemberExpression::boxed(
				SPAN,
				self.activation_frame_member(&location.frame, "liveLocals"),
				Expression::new_numeric_literal(
					SPAN,
					location.slot as f64,
					None,
					NumberBase::Decimal,
					&self.ast,
				),
				false,
				&self.ast,
			),
		))
	}

	fn activation_slot_target(&self, location: &ActivationLocation) -> AssignmentTarget<'a> {
		AssignmentTarget::from(MemberExpression::ComputedMemberExpression(
			ComputedMemberExpression::boxed(
				SPAN,
				self.activation_frame_member(&location.frame, "liveLocals"),
				Expression::new_numeric_literal(
					SPAN,
					location.slot as f64,
					None,
					NumberBase::Decimal,
					&self.ast,
				),
				false,
				&self.ast,
			),
		))
	}

	fn object_assign(&self, args: Vec<Expression<'a>>) -> Expression<'a> {
		match &self.representation {
			RepresentationPolicy::Direct => self.member_call(
				Expression::new_identifier(SPAN, "Object", &self.ast),
				"assign",
				args,
			),
			RepresentationPolicy::Transactional => self.transaction_call("nymphAssign", args),
		}
	}

	fn set_prototype(&self, value: Expression<'a>, prototype: Expression<'a>) -> Expression<'a> {
		let args = vec![value, prototype];
		match &self.representation {
			RepresentationPolicy::Direct => self.member_call(
				Expression::new_identifier(SPAN, "Object", &self.ast),
				"setPrototypeOf",
				args,
			),
			RepresentationPolicy::Transactional => self.transaction_call("nymphSetPrototypeOf", args),
		}
	}

	pub(crate) fn with_needed_imports(mut self, imports: &[(String, String, String)]) -> Self {
		self.provided_imports.extend(imports.iter().cloned());
		self
			.needed_imports
			.get_mut()
			.extend(imports.iter().cloned());
		self
	}

	pub(crate) fn unaudited_external(&self) -> Option<(String, String)> {
		self.needed_imports.borrow().iter().find_map(|import| {
			(!self.provided_imports.contains(import)
				&& nymph_hir::linkage::external_effect(&import.0, &import.1)
					== nymph_hir::linkage::ExternalEffect::UnauditedStateful)
				.then(|| (import.0.clone(), import.1.clone()))
		})
	}

	/// A fresh compiler-reserved temporary name. `$` is not valid in a Nymph
	/// identifier, so generated bindings cannot collide with source locals.
	fn gensym(&self) -> String {
		let n = self.gensym.get();
		self.gensym.set(n + 1);
		format!("$nymph$temp${n}")
	}

	fn completion_name(&self) -> String {
		let n = self.completion_gensym.get();
		self.completion_gensym.set(n + 1);
		format!("$nymph$completion${n}")
	}

	pub fn emit_module(&self, module: &HirModule) -> String {
		self.class_defaults.borrow_mut().extend(
			module
				.classes
				.iter()
				.map(|class| (class.name.to_string(), class.defaults.clone())),
		);
		let mut stmts = ArenaVec::new_in(&self.ast);
		// The shared discriminant symbol, emitted once if the module has any enum.
		if !module.enums.is_empty() {
			stmts.push(self.emit_tag_const());
			for hir_enum in &module.enums {
				stmts.push(self.emit_enum(hir_enum));
			}
		}
		// Classes next so constructors are in scope for the functions that build them.
		for class in &module.classes {
			stmts.push(self.emit_class(class));
			for method in &class.methods {
				stmts.push(self.mark_class_callable(&class.name, &method.name, false));
			}
			for method in &class.statics {
				stmts.push(self.mark_class_callable(&class.name, &method.name, true));
			}
		}
		// Activation callables are initialized before top-level values so a value
		// initializer can call one without crossing a `const` TDZ.
		for func in &module.funcs {
			stmts.push(self.emit_func(func));
		}
		// Top-level `let`s remain in source order after declarations they may use.
		let mut external_values: std::collections::BTreeMap<_, EcoString> =
			std::collections::BTreeMap::new();
		for let_ in &module.lets {
			if let HirExpr::ExternValue {
				module,
				symbol,
				marshal,
			} = let_.value
				&& let Some(canonical) = external_values.get(&(module, symbol, marshal))
			{
				let alias = HirLet {
					name: let_.name.clone(),
					value: HirExpr::Local(canonical.clone()),
				};
				stmts.push(self.emit_module_let(&alias));
			} else {
				if let HirExpr::ExternValue {
					module,
					symbol,
					marshal,
				} = let_.value
				{
					external_values.insert((module, symbol, marshal), let_.name.clone());
				}
				stmts.push(self.emit_module_let(let_));
			}
		}
		// Gap 3 (L0): prepend one deduped, deterministically-ordered `import`
		// per `(module, symbol)` pair any `HirExpr::ExternCall` above
		// recorded — emitted INTO the returned module string rather than via
		// a changed `emit`/`emit_module` return shape (a `-> String` many
		// call sites depend on), so an import line can ride along even though
		// `emit_module` still returns one flat `String`. Valid ES: an
		// `import` hoists regardless of where in the module body it's
		// textually written.
		let imports = self.needed_imports.borrow();
		if !imports.is_empty() {
			let mut with_imports = ArenaVec::new_in(&self.ast);
			for (module_specifier, symbol, local) in imports.iter() {
				with_imports.push(self.build_import_statement(module_specifier, symbol, local));
			}
			with_imports.extend(stmts);
			stmts = with_imports;
		}
		let program = Program::new(
			SPAN,
			SourceType::mjs(),
			"",
			ArenaVec::new_in(&self.ast),
			None,
			ArenaVec::new_in(&self.ast),
			stmts,
			&self.ast,
		);
		let code = Codegen::new().build(&program).code;
		// Uniform value boxing (slice #2): standalone modules carry the runtime
		// inline for direct Node execution, while project modules import their
		// exact bindings from the canonical virtual module. Modules that never
		// reference the runtime remain byte-identical.
		let box_runtime_bindings = self.box_runtime_bindings.borrow();
		if box_runtime_bindings.is_empty() {
			code
		} else if self.import_box_runtime {
			format!(
				"import {{ {} }} from \"{}\";\n{code}",
				box_runtime_bindings
					.iter()
					.cloned()
					.collect::<Vec<_>>()
					.join(", "),
				box_rt::BOX_MODULE_KEY,
			)
		} else {
			let preamble = if box_runtime_bindings.contains("nymphEcho") {
				box_rt::box_preamble()
			} else {
				box_rt::box_preamble_release()
			};
			format!("{preamble}{code}")
		}
	}

	/// Build `import { <symbol> as <local> } from "<module_specifier>";` (Gap 3, L0).
	fn build_import_statement(
		&self,
		module_specifier: &str,
		symbol: &str,
		local: &str,
	) -> Statement<'a> {
		let imported = ModuleExportName::IdentifierName(IdentifierName::new(
			SPAN,
			self.ast.allocator.alloc_str(symbol),
			&self.ast,
		));
		let local = BindingIdentifier::new(SPAN, self.ast.allocator.alloc_str(local), &self.ast);
		let mut specifiers = ArenaVec::new_in(&self.ast);
		specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
			ImportSpecifier::boxed(SPAN, imported, local, ImportOrExportKind::Value, &self.ast),
		));
		let source = StringLiteral::new(
			SPAN,
			self.ast.allocator.alloc_str(module_specifier),
			None,
			&self.ast,
		);
		Statement::ImportDeclaration(ImportDeclaration::boxed(
			SPAN,
			Some(specifiers),
			source,
			None,
			oxc::ast::NONE,
			ImportOrExportKind::Value,
			&self.ast,
		))
	}

	fn activation_assignment(
		&self,
		target: AssignmentTarget<'a>,
		value: Expression<'a>,
	) -> Statement<'a> {
		let assignment = Expression::new_assignment_expression(
			SPAN,
			AssignmentOperator::Assign,
			target,
			value,
			&self.ast,
		);
		Statement::new_expression_statement(SPAN, assignment, &self.ast)
	}

	fn activation_resume_target(&self, frame: &str) -> AssignmentTarget<'a> {
		AssignmentTarget::from(MemberExpression::StaticMemberExpression(
			StaticMemberExpression::boxed(
				SPAN,
				Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(frame), &self.ast),
				IdentifierName::new(SPAN, "resumeState", &self.ast),
				false,
				&self.ast,
			),
		))
	}

	fn activation_state_number(&self, state: u32) -> Expression<'a> {
		Expression::new_numeric_literal(SPAN, f64::from(state), None, NumberBase::Decimal, &self.ast)
	}

	fn activation_goto(&self, frame: &str, state: u32) -> ArenaVec<'a, Statement<'a>> {
		let mut statements = ArenaVec::new_in(&self.ast);
		statements.push(self.activation_assignment(
			self.activation_resume_target(frame),
			self.activation_state_number(state),
		));
		statements.push(Statement::new_continue_statement(SPAN, None, &self.ast));
		statements
	}

	fn activation_packet(
		&self,
		callee: Expression<'a>,
		receiver: Expression<'a>,
		args: Vec<Expression<'a>>,
		mode: HirCallMode,
		source: u32,
		resume_state: u32,
		result_slot: usize,
	) -> Expression<'a> {
		let mut packet = vec![
			callee,
			receiver,
			self.emit_activation_args(args),
			self.emit_activation_source(source),
		];
		let helper = if mode == HirCallMode::Tail {
			"nymphTailCall"
		} else {
			packet.push(self.activation_state_number(resume_state));
			packet.push(Expression::new_numeric_literal(
				SPAN,
				result_slot as f64,
				None,
				NumberBase::Decimal,
				&self.ast,
			));
			"nymphPush"
		};
		self.runtime_call(helper, packet)
	}

	fn activation_member_packet(
		&self,
		receiver: Expression<'a>,
		member: &str,
		args: Vec<Expression<'a>>,
		mode: HirCallMode,
		source: u32,
		resume_state: u32,
		result_slot: usize,
	) -> Expression<'a> {
		let callee = Expression::new_static_member_expression(
			SPAN,
			receiver.clone_in(self.ast.allocator),
			IdentifierName::new(SPAN, self.ast.allocator.alloc_str(member), &self.ast),
			false,
			&self.ast,
		);
		self.activation_packet(
			callee,
			receiver,
			args,
			mode,
			source,
			resume_state,
			result_slot,
		)
	}

	fn emit_activation_transfer(
		&self,
		call: &HirExpr,
		resume_state: u32,
		result_slot: usize,
	) -> Expression<'a> {
		match call {
			HirExpr::TaskOperation {
				operation,
				operands,
			} => {
				assert!(operation.suspends());
				let effect = self.zero_argument_arrow(self.emit_task_operation(*operation, operands));
				self.runtime_call(
					"nymphSuspend",
					vec![
						effect,
						self.activation_state_number(resume_state),
						self.activation_state_number(result_slot as u32),
					],
				)
			}
			HirExpr::ActivationCall {
				callee,
				args,
				mode,
				source,
			} => {
				let args = args.iter().map(|arg| self.emit_expr(arg)).collect();
				if let HirExpr::Field { recv, name } = callee.as_ref() {
					return self.activation_member_packet(
						self.emit_expr(recv),
						name,
						args,
						*mode,
						*source,
						resume_state,
						result_slot,
					);
				}
				self.activation_packet(
					self.emit_expr(callee),
					Expression::new_identifier(SPAN, "undefined", &self.ast),
					args,
					*mode,
					*source,
					resume_state,
					result_slot,
				)
			}
			HirExpr::StaticEnumDispatch {
				owner,
				method,
				receiver,
				args,
				mode,
				source,
			} => {
				let prototype = Expression::new_static_member_expression(
					SPAN,
					self.local_read(owner),
					IdentifierName::new(SPAN, "$nymph$type", &self.ast),
					false,
					&self.ast,
				);
				let callee = Expression::new_static_member_expression(
					SPAN,
					prototype,
					IdentifierName::new(SPAN, self.ast.allocator.alloc_str(method), &self.ast),
					false,
					&self.ast,
				);
				self.activation_packet(
					callee,
					self.emit_expr(receiver),
					args.iter().map(|arg| self.emit_expr(arg)).collect(),
					*mode,
					*source,
					resume_state,
					result_slot,
				)
			}
			HirExpr::BoundDispatch {
				method,
				receiver,
				argument,
				hidden_arguments,
				cases,
				mode,
				source,
				..
			} => self.emit_bound_dispatch(
				method,
				receiver,
				argument,
				hidden_arguments,
				cases,
				*mode,
				*source,
				Some((resume_state, result_slot)),
			),
			HirExpr::UnaryBoundDispatch {
				method,
				receiver,
				hidden_arguments,
				cases,
				mode,
				source,
				..
			} => self.emit_unary_bound_dispatch(
				method,
				receiver,
				hidden_arguments,
				cases,
				*mode,
				*source,
				Some((resume_state, result_slot)),
			),
			_ => unreachable!("activation transfer terminal must contain a generated call"),
		}
	}

	fn emit_activation_plan(&self, plan: ActivationPlan) -> Expression<'a> {
		let mut loop_body = ArenaVec::new_in(&self.ast);
		for (state_number, state) in plan.states.into_iter().enumerate() {
			let old_environment = self.activation_environment.replace(state.environment);
			let old_patterns = self
				.activation_pattern_bindings
				.replace(state.pattern_bindings);
			let mut statements = ArenaVec::new_in(&self.ast);
			for action in state.actions {
				match action {
					ActivationAction::Store { target, value } => {
						let value = self.emit_expr(&value);
						statements
							.push(self.activation_assignment(self.activation_slot_target(&target), value));
					}
					ActivationAction::RegisterCleanup(cleanup) => {
						let cleanup = self.zero_argument_arrow(self.emit_expr(&cleanup));
						let call = self.runtime_call("nymphRegisterCleanup", vec![cleanup]);
						statements.push(Statement::new_expression_statement(SPAN, call, &self.ast));
					}
					ActivationAction::RegisterStateCleanup {
						cleanup,
						binding,
						value,
						handle,
					} => {
						let previous = self
							.activation_environment
							.borrow_mut()
							.insert(binding.clone(), value);
						let cleanup = self.zero_argument_arrow(self.emit_expr(&cleanup));
						if let Some(previous) = previous {
							self
								.activation_environment
								.borrow_mut()
								.insert(binding, previous);
						} else {
							self.activation_environment.borrow_mut().remove(&binding);
						}
						let call = self.runtime_call("nymphRegisterCleanup", vec![cleanup]);
						statements.push(self.activation_assignment(self.activation_slot_target(&handle), call));
					}
					ActivationAction::CommitStateTransition {
						header_depth,
						replacements,
					} => {
						let mut pairs = ArenaVec::new_in(&self.ast);
						for (old, new) in replacements {
							pairs.push(ArrayExpressionElement::from(self.activation_slot(&old)));
							pairs.push(ArrayExpressionElement::from(self.activation_slot(&new)));
						}
						let pairs = Expression::new_array_expression(SPAN, pairs, &self.ast);
						let depth = Expression::new_numeric_literal(
							SPAN,
							header_depth as f64,
							None,
							NumberBase::Decimal,
							&self.ast,
						);
						let call = self.runtime_call("nymphCommitStateTransition", vec![depth, pairs]);
						statements.push(Statement::new_expression_statement(SPAN, call, &self.ast));
					}
					ActivationAction::EnterCleanupScope => {
						let call = self.runtime_call("nymphEnterCleanupScope", vec![]);
						statements.push(Statement::new_expression_statement(SPAN, call, &self.ast));
					}
					ActivationAction::UnwindCleanupScopes(depth) => {
						let depth = Expression::new_numeric_literal(
							SPAN,
							depth as f64,
							None,
							NumberBase::Decimal,
							&self.ast,
						);
						let call = self.runtime_call("nymphUnwindCleanupScopes", vec![depth]);
						statements.push(Statement::new_expression_statement(SPAN, call, &self.ast));
					}
				}
			}
			match state.terminal {
				ActivationTerminal::Goto(next) => {
					statements.extend(self.activation_goto(&plan.frame, next))
				}
				ActivationTerminal::Branch {
					condition,
					then_state,
					else_state,
				} => {
					let next = Expression::new_conditional_expression(
						SPAN,
						self.emit_cond(&condition),
						self.activation_state_number(then_state),
						self.activation_state_number(else_state),
						&self.ast,
					);
					statements
						.push(self.activation_assignment(self.activation_resume_target(&plan.frame), next));
					statements.push(Statement::new_continue_statement(SPAN, None, &self.ast));
				}
				ActivationTerminal::Transfer {
					call,
					resume_state,
					result_slot,
				} => statements.push(Statement::new_return_statement(
					SPAN,
					Some(self.emit_activation_transfer(&call, resume_state, result_slot)),
					&self.ast,
				)),
				ActivationTerminal::Return(value) => statements.push(Statement::new_return_statement(
					SPAN,
					Some(self.runtime_call("nymphReturn", vec![self.emit_expr(&value)])),
					&self.ast,
				)),
			}
			self.activation_environment.replace(old_environment);
			self.activation_pattern_bindings.replace(old_patterns);

			let condition = Expression::new_binary_expression(
				SPAN,
				self.activation_frame_member(&plan.frame, "resumeState"),
				BinaryOperator::StrictEquality,
				self.activation_state_number(state_number as u32),
				&self.ast,
			);
			loop_body.push(Statement::new_if_statement(
				SPAN,
				condition,
				Statement::new_block_statement(SPAN, statements, &self.ast),
				None,
				&self.ast,
			));
		}
		let invalid = self.runtime_call(
			"nymphDefect",
			vec![Expression::new_identifier(SPAN, "undefined", &self.ast)],
		);
		loop_body.push(Statement::new_return_statement(
			SPAN,
			Some(invalid),
			&self.ast,
		));
		let while_loop = Statement::new_while_statement(
			SPAN,
			Expression::new_boolean_literal(SPAN, true, &self.ast),
			Statement::new_block_statement(SPAN, loop_body, &self.ast),
			&self.ast,
		);
		let params = FormalParameters::new(
			SPAN,
			FormalParameterKind::FormalParameter,
			ArenaVec::from_value_in(
				FormalParameter::new_plain(
					SPAN,
					BindingPattern::new_binding_identifier(
						SPAN,
						self.ast.allocator.alloc_str(&plan.frame),
						&self.ast,
					),
					&self.ast,
				),
				&self.ast,
			),
			oxc::ast::NONE,
			&self.ast,
		);
		Expression::FunctionExpression(Function::boxed(
			SPAN,
			FunctionType::FunctionExpression,
			None,
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			Some(FunctionBody::new(
				SPAN,
				ArenaVec::new_in(&self.ast),
				ArenaVec::from_value_in(while_loop, &self.ast),
				&self.ast,
			)),
			&self.ast,
		))
	}

	fn activation_outer_environment(&self) -> std::collections::BTreeMap<String, ActivationLocation> {
		self.activation_environment.borrow().clone()
	}

	fn activation_callable(
		&self,
		params: &[EcoString],
		body: &HirExpr,
		capture_receiver: bool,
	) -> Expression<'a> {
		let mut outer = self.activation_outer_environment();
		let capture_frame = self.gensym();
		let mut captured = Vec::new();
		if capture_receiver {
			for location in outer.values_mut() {
				captured.push(self.activation_slot(location));
				location.frame = capture_frame.clone();
				location.slot = captured.len() - 1;
			}
		}
		let plan = ActivationPlanner::new(self, params, outer).finish(body);
		let mut step = self.emit_activation_plan(plan);
		if capture_receiver {
			step = self.member_call(
				step,
				"bind",
				vec![Expression::ThisExpression(ThisExpression::boxed(
					SPAN, &self.ast,
				))],
			);
		}
		let callable = self.runtime_call("nymphCallable", vec![step]);
		if captured.is_empty() {
			callable
		} else {
			let capture_frame = self.ast.allocator.alloc_str(&capture_frame);
			let frame = self.runtime_call(
				"nymphCaptureFrame",
				vec![self.emit_activation_args(captured)],
			);
			self.arrow_iife(capture_frame, callable, frame)
		}
	}

	fn emit_func(&self, func: &HirFunc) -> Statement<'a> {
		let callable = self.activation_callable(&func.params, &func.body, false);
		self.plain_decl(&func.name, callable, VariableDeclarationKind::Let)
	}

	/// Emit an immutable top-level binding.
	fn emit_module_let(&self, let_: &HirLet) -> Statement<'a> {
		self.binding_declaration(&let_.name, self.emit_expr(&let_.value))
	}

	/// Emit a struct as `class <Name> { constructor(fields) { … } }`.
	///
	/// The object-argument constructor lets construction pass labeled fields as a
	/// plain object (`new Point({ x, y })`) without depending on field order.
	/// Owner defaults run in declaration order only for properties absent from
	/// the incoming object. Clone/update objects already contain every field, so
	/// they never re-run defaults. `Object.assign` then copies all fields.
	fn mark_class_callable(&self, class: &str, method: &str, static_: bool) -> Statement<'a> {
		let class = Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(class), &self.ast);
		let owner = if static_ {
			class
		} else {
			Expression::new_static_member_expression(
				SPAN,
				class,
				IdentifierName::new(SPAN, "prototype", &self.ast),
				false,
				&self.ast,
			)
		};
		let callable = Expression::new_static_member_expression(
			SPAN,
			owner,
			IdentifierName::new(SPAN, self.ast.allocator.alloc_str(method), &self.ast),
			false,
			&self.ast,
		);
		Statement::new_expression_statement(
			SPAN,
			self.runtime_call("nymphMarkCallable", vec![callable]),
			&self.ast,
		)
	}

	fn emit_class(&self, class: &HirClass) -> Statement<'a> {
		self
			.box_runtime_bindings
			.borrow_mut()
			.insert("nymphStructuralValue".to_string());
		let assign_call = self.object_assign(vec![
			Expression::ThisExpression(ThisExpression::boxed(SPAN, &self.ast)),
			Expression::Identifier(IdentifierReference::boxed(SPAN, "fields", &self.ast)),
		]);
		let mut ctor_stmts = ArenaVec::new_in(&self.ast);
		for (name, default) in &class.defaults {
			let fields = || Expression::new_identifier(SPAN, "fields", &self.ast);
			let has_own = self.member_call(
				Expression::new_identifier(SPAN, "Object", &self.ast),
				"hasOwn",
				vec![
					fields(),
					Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(name), None, &self.ast),
				],
			);
			let missing =
				Expression::new_unary_expression(SPAN, UnaryOperator::LogicalNot, has_own, &self.ast);
			let member = StaticMemberExpression::boxed(
				SPAN,
				fields(),
				IdentifierName::new(SPAN, self.ast.allocator.alloc_str(name), &self.ast),
				false,
				&self.ast,
			);
			let assign = Expression::new_assignment_expression(
				SPAN,
				AssignmentOperator::Assign,
				AssignmentTarget::from(MemberExpression::StaticMemberExpression(member)),
				self.emit_expr(default),
				&self.ast,
			);
			ctor_stmts.push(Statement::new_if_statement(
				SPAN,
				missing,
				Statement::new_expression_statement(SPAN, assign, &self.ast),
				None,
				&self.ast,
			));
		}
		ctor_stmts.push(Statement::new_expression_statement(
			SPAN,
			assign_call,
			&self.ast,
		));
		let identity = format!("struct:{}", class.name);
		let structural_value = self.structural_value(
			Expression::ThisExpression(ThisExpression::boxed(SPAN, &self.ast)),
			&identity,
			&class.fields,
		);
		ctor_stmts.push(Statement::new_expression_statement(
			SPAN,
			structural_value,
			&self.ast,
		));

		// constructor(fields) { … }
		let mut ctor_params = ArenaVec::new_in(&self.ast);
		let fields_pat = BindingPattern::new_binding_identifier(SPAN, "fields", &self.ast);
		ctor_params.push(FormalParameter::new_plain(SPAN, fields_pat, &self.ast));
		let params = FormalParameters::new(
			SPAN,
			FormalParameterKind::FormalParameter,
			ctor_params,
			oxc::ast::NONE,
			&self.ast,
		);
		let ctor_body = FunctionBody::new(SPAN, ArenaVec::new_in(&self.ast), ctor_stmts, &self.ast);
		let ctor_fn = Function::boxed(
			SPAN,
			FunctionType::FunctionExpression,
			None,
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			Some(ctor_body),
			&self.ast,
		);
		let ctor = ClassElement::new_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			ArenaVec::new_in(&self.ast),
			PropertyKey::new_static_identifier(SPAN, "constructor", &self.ast),
			ctor_fn,
			MethodDefinitionKind::Constructor,
			false,
			false,
			false,
			false,
			None,
			&self.ast,
		);

		let mut elements = ArenaVec::new_in(&self.ast);
		elements.push(ctor);
		for method in &class.methods {
			elements.push(self.emit_method(method, false));
		}
		for method in &class.statics {
			elements.push(self.emit_method(method, true));
		}
		let body = ClassBody::new(SPAN, elements, &self.ast);
		let class_name = class.name.to_string();
		let name = BindingIdentifier::new(SPAN, self.ast.allocator.alloc_str(&class.name), &self.ast);
		let class = Class::boxed(
			SPAN,
			self.representation.class_type(),
			ArenaVec::new_in(&self.ast),
			Some(name),
			oxc::ast::NONE,
			None,
			oxc::ast::NONE,
			ArenaVec::new_in(&self.ast),
			body,
			false,
			false,
			&self.ast,
		);
		if !self.representation.is_transactional() {
			return Statement::ClassDeclaration(class);
		}
		let module = self.current_module.as_deref().unwrap_or_default();
		let init = self.transaction_call(
			"nymphRuntimeClass",
			vec![
				Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(module), None, &self.ast),
				Expression::new_string_literal(
					SPAN,
					self.ast.allocator.alloc_str(&class_name),
					None,
					&self.ast,
				),
				Expression::ClassExpression(class),
			],
		);
		let declarator = VariableDeclarator::new(
			SPAN,
			VariableDeclarationKind::Const,
			BindingPattern::new_binding_identifier(
				SPAN,
				self.ast.allocator.alloc_str(&class_name),
				&self.ast,
			),
			oxc::ast::NONE,
			Some(init),
			false,
			&self.ast,
		);
		Statement::from(Declaration::VariableDeclaration(ArenaBox::new_in(
			VariableDeclaration::new(
				SPAN,
				VariableDeclarationKind::Const,
				ArenaVec::from_value_in(declarator, &self.ast),
				false,
				&self.ast,
			),
			&self.ast,
		)))
	}

	/// Build a method's params/body into a plain JS `FunctionExpression`
	/// (`(<params>) { return <body>; }`), independent of how the caller wraps
	/// it — a class method definition (struct/class instance methods) or an
	/// object-literal method property (the enum prototype ABI, Slice 4D) both
	/// share this exactly. Mirrors [`Self::emit_func`]'s param/body handling.
	/// Deliberately a plain function, never an arrow: prototype methods need
	/// their own `this` bound to the receiver at call time.
	fn method_function(&self, method: &HirMethod) -> ArenaBox<'a, Function<'a>> {
		let plan = ActivationPlanner::new(self, &method.params, std::collections::BTreeMap::new())
			.finish(&method.body);
		let step = self.emit_activation_plan(plan);
		let bridge = self.runtime_call(
			"nymphMethodStep",
			vec![
				Expression::ThisExpression(ThisExpression::boxed(SPAN, &self.ast)),
				Expression::new_string_literal(
					SPAN,
					self.ast.allocator.alloc_str(&method.name),
					None,
					&self.ast,
				),
				Expression::new_identifier(SPAN, "arguments", &self.ast),
				step,
			],
		);
		let body_stmts = ArenaVec::from_value_in(
			Statement::new_return_statement(SPAN, Some(bridge), &self.ast),
			&self.ast,
		);
		let mut js_params = ArenaVec::new_in(&self.ast);
		for param in &method.params {
			let pat = BindingPattern::new_binding_identifier(
				SPAN,
				self.ast.allocator.alloc_str(param),
				&self.ast,
			);
			js_params.push(FormalParameter::new_plain(SPAN, pat, &self.ast));
		}
		let params = FormalParameters::new(
			SPAN,
			FormalParameterKind::FormalParameter,
			js_params,
			oxc::ast::NONE,
			&self.ast,
		);
		let fn_body = FunctionBody::new(SPAN, ArenaVec::new_in(&self.ast), body_stmts, &self.ast);
		Function::boxed(
			SPAN,
			FunctionType::FunctionExpression,
			None,
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			Some(fn_body),
			&self.ast,
		)
	}

	/// Emit an inherent instance method as a class method `<name>(<params>) { return
	/// <body>; }`. When `is_static`, emits a `namespace func` static function
	/// (Slice 4J) as a JS `static` class method instead — `Type.func(args)` then
	/// resolves to it natively, with zero call-site changes needed.
	fn emit_method(&self, method: &HirMethod, is_static: bool) -> ClassElement<'a> {
		let func = self.method_function(method);
		ClassElement::new_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			ArenaVec::new_in(&self.ast),
			PropertyKey::new_static_identifier(
				SPAN,
				self.ast.allocator.alloc_str(&method.name),
				&self.ast,
			),
			func,
			MethodDefinitionKind::Method,
			false,
			is_static,
			false,
			false,
			None,
			&self.ast,
		)
	}

	/// Emit an instance method as an object-literal method property (shorthand
	/// `<name>(<params>) { … }` syntax), used for the enum prototype ABI's
	/// `const proto = { … };` object (Slice 4D). Must stay a plain
	/// `FunctionExpression` (never an arrow) so each call gets its own `this`.
	fn emit_method_property(&self, method: &HirMethod) -> ObjectPropertyKind<'a> {
		let func = self.method_function(method);
		let key = PropertyKey::new_static_identifier(
			SPAN,
			self.ast.allocator.alloc_str(&method.name),
			&self.ast,
		);
		ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
			SPAN,
			PropertyKind::Init,
			key,
			self.runtime_call(
				"nymphMarkCallable",
				vec![Expression::FunctionExpression(func)],
			),
			false,
			false,
			false,
			&self.ast,
		))
	}

	// ── Enum Symbol-tag ABI ────────────────────────────────────────────────────

	/// `const TAG = Symbol.for("nymph.tag");` — the shared discriminant key, the
	/// same symbol in every module via the global registry.
	fn emit_tag_const(&self) -> Statement<'a> {
		let symbol_for = Expression::new_static_member_expression(
			SPAN,
			Expression::new_identifier(SPAN, "Symbol", &self.ast),
			IdentifierName::new(SPAN, "for", &self.ast),
			false,
			&self.ast,
		);
		let mut args = ArenaVec::new_in(&self.ast);
		args.push(Argument::from(Expression::new_string_literal(
			SPAN,
			self.ast.allocator.alloc_str("nymph.tag"),
			None,
			&self.ast,
		)));
		let init =
			Expression::new_call_expression(SPAN, symbol_for, oxc::ast::NONE, args, false, &self.ast);
		self.const_decl("TAG", init)
	}

	/// Emit an enum as `const <E> = (() => { const t0 = Symbol.for("E.V0"); …
	/// return { V0: <factory|singleton>, … }; })();`. The IIFE scopes each
	/// variant's own symbol BINDING; field variants become object-arg
	/// factories, nullary variants frozen singletons — each carrying `[TAG]`
	/// so a matcher can compare identity.
	///
	/// L1 (external linkage's Option ABI seam): the variant discriminant is
	/// `Symbol.for(label)` — the GLOBAL symbol registry, keyed by the exact
	/// string `label` (`"<enum-name>.<variant-name>"`, enum name emitted
	/// UNMANGLED) — not a bare `Symbol(label)` call, which mints a FRESH,
	/// non-global symbol every time it runs. `Option` (and every other
	/// prelude enum) is materialized INLINE, once per module that references
	/// it (`Lowerer::materialize_referenced_prelude_enums`) — with a bare
	/// `Symbol(..)`, two DIFFERENT modules' own inline `Option` IIFEs mint
	/// two DIFFERENT `Symbol("Option.Some")` values, so a `Some` built in one
	/// module fails an `=== ` tag comparison against `Option.Some[TAG]` read
	/// from another module's own inline `Option` — cross-module (and,
	/// crucially for this slice, intrinsic-runtime-built) `Option`/enum
	/// values silently mismatch every `match`, EVEN THOUGH the checker
	/// already treats them as the identical type. `Symbol.for` fixes this:
	/// the same string always resolves to the same global symbol, so any two
	/// independently-emitted (or independently hand-built, see
	/// `nymph-compiler`'s `HostRuntimeGraph`-injected `std/option` virtual module)
	/// values of "the same" enum variant compare equal by construction. The
	/// TAG KEY itself (`emit_tag_const`, above) was already global via
	/// `Symbol.for("nymph.tag")` — only the per-variant discriminant VALUE
	/// was the gap.
	///
	/// X1: every enum has a `const proto = { … };` object (built the
	/// same way as struct class methods, see [`Self::emit_method_property`]) is
	/// also emitted inside the IIFE, and every variant value is created with
	/// `Object.create(proto)` as its prototype instead of a plain object literal
	/// — so `c.m()` and `this` inside a method work natively, while methodless
	/// enums still have a canonical runtime type object.
	fn emit_enum(&self, hir_enum: &HirEnum) -> Statement<'a> {
		self
			.box_runtime_bindings
			.borrow_mut()
			.insert("nymphStructuralValue".to_string());
		let mut stmts = ArenaVec::new_in(&self.ast);
		let has_methods = true;
		// The prototype is also the enum's canonical runtime type object, so it
		// exists even when there are no instance methods.
		stmts.push(self.emit_enum_proto(&hir_enum.methods));
		let mut props = ArenaVec::new_in(&self.ast);
		for (i, variant) in hir_enum.variants.iter().enumerate() {
			let t_name = format!("t{i}");
			// const t<i> = Symbol("<E>.<V>");
			let label = format!("{}.{}", hir_enum.name, variant.name);
			// `Symbol.for(label)`, not a bare `Symbol(label)` call — see this
			// method's own doc comment for why the discriminant must be the
			// GLOBAL symbol registry entry, mirroring `emit_tag_const`'s own
			// `Symbol.for("nymph.tag")` shape.
			let symbol_for = Expression::new_static_member_expression(
				SPAN,
				Expression::new_identifier(SPAN, "Symbol", &self.ast),
				IdentifierName::new(SPAN, "for", &self.ast),
				false,
				&self.ast,
			);
			let mut sym_args = ArenaVec::new_in(&self.ast);
			sym_args.push(Argument::from(Expression::new_string_literal(
				SPAN,
				self.ast.allocator.alloc_str(&label),
				None,
				&self.ast,
			)));
			let sym_call = Expression::new_call_expression(
				SPAN,
				symbol_for,
				oxc::ast::NONE,
				sym_args,
				false,
				&self.ast,
			);
			stmts.push(self.const_decl(&t_name, sym_call));

			// The `{ [TAG]: t<i> }` object both variant shapes carry.
			let mut tag_props = ArenaVec::new_in(&self.ast);
			tag_props.push(self.tag_prop(&t_name));
			let tag_obj = Expression::new_object_expression(SPAN, tag_props, &self.ast);
			// The variant's value: a factory (fields) or a frozen singleton (nullary).
			let value = if variant.fields.is_empty() {
				let base = if has_methods {
					self.object_create_and_assign("proto", tag_obj)
				} else {
					tag_obj
				};
				let base = self.structural_value(base, &format!("variant:{label}"), &variant.fields);
				self.member_call(
					Expression::new_identifier(SPAN, "Object", &self.ast),
					"freeze",
					vec![base],
				)
			} else {
				let factory = self.variant_factory(
					&t_name,
					has_methods,
					&format!("variant:{label}"),
					&variant.fields,
				);
				self.object_assign(vec![factory, tag_obj])
			};

			let key = PropertyKey::new_static_identifier(
				SPAN,
				self.ast.allocator.alloc_str(&variant.name),
				&self.ast,
			);
			props.push(ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
				SPAN,
				PropertyKind::Init,
				key,
				value,
				false,
				false,
				false,
				&self.ast,
			)));
		}
		// `namespace func` static functions (Slice 4J) become OBJECT-level
		// method properties on the returned object itself, alongside the
		// variant keys — NOT on `proto` (only reachable through a constructed
		// variant instance, never through the enum name; call sites emit
		// `E.func(..)` against the object `E`, which is this returned object,
		// see `HirEnum::statics`'s doc comment).
		for method in &hir_enum.statics {
			props.push(self.emit_method_property(method));
		}
		// The canonical enum prototype is also its compiler-only runtime type
		// object. Exposing this unspellable property lets hidden generic arguments
		// share the exact object used by every variant instance.
		props.push(ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
			SPAN,
			PropertyKind::Init,
			PropertyKey::new_static_identifier(SPAN, "$nymph$type", &self.ast),
			Expression::new_identifier(SPAN, "proto", &self.ast),
			false,
			false,
			false,
			&self.ast,
		)));
		let return_obj = Expression::new_object_expression(SPAN, props, &self.ast);
		let mut iife = JsValue {
			stmts,
			expr: return_obj,
		}
		.into_expression(self.ast);
		if self.representation.is_transactional() {
			let module = self.current_module.as_deref().unwrap_or_default();
			iife = self.transaction_call(
				"nymphRuntimeEnum",
				vec![
					Expression::new_string_literal(
						SPAN,
						self.ast.allocator.alloc_str(module),
						None,
						&self.ast,
					),
					Expression::new_string_literal(
						SPAN,
						self.ast.allocator.alloc_str(&hir_enum.name),
						None,
						&self.ast,
					),
					iife,
				],
			);
		}
		self.const_decl(hir_enum.name.as_str(), iife)
	}

	/// `const proto = { m1(…) { … }, … };` — the shared prototype object an
	/// enum's methodful variants are `Object.create`d against (Slice 4D, X1).
	/// Each method is built the same way a struct's class method is (via
	/// [`Self::method_function`]), just wrapped as an object-literal method
	/// property instead of a class element.
	fn emit_enum_proto(&self, methods: &[HirMethod]) -> Statement<'a> {
		let mut props = ArenaVec::new_in(&self.ast);
		for method in methods {
			props.push(self.emit_method_property(method));
		}
		let obj = Expression::new_object_expression(SPAN, props, &self.ast);
		self.const_decl("proto", obj)
	}

	/// `Object.assign(Object.create(<proto_name>), <props>)` — a methodful
	/// variant's value, sharing `proto_name`'s prototype while still carrying
	/// `props`' own properties (the `[TAG]` / fields).
	fn object_create_and_assign(&self, proto_name: &'a str, props: Expression<'a>) -> Expression<'a> {
		let create_call = self.member_call(
			Expression::new_identifier(SPAN, "Object", &self.ast),
			"create",
			vec![Expression::new_identifier(SPAN, proto_name, &self.ast)],
		);
		self.object_assign(vec![create_call, props])
	}

	/// `(fields) => { return { [TAG]: <t_name>, ...fields }; }` — a field variant's
	/// object-argument factory. When `has_methods`, the returned object is instead
	/// `Object.assign(Object.create(proto), { [TAG]: <t_name>, ...fields })` so the
	/// constructed value carries the shared prototype's methods (Slice 4D, X1).
	fn variant_factory(
		&self,
		t_name: &str,
		has_methods: bool,
		identity: &str,
		fields: &[EcoString],
	) -> Expression<'a> {
		let mut obj_props = ArenaVec::new_in(&self.ast);
		obj_props.push(self.tag_prop(t_name));
		obj_props.push(ObjectPropertyKind::new_spread_property(
			SPAN,
			Expression::new_identifier(SPAN, "fields", &self.ast),
			&self.ast,
		));
		let obj = Expression::new_object_expression(SPAN, obj_props, &self.ast);
		let ret_expr = if has_methods {
			self.object_create_and_assign("proto", obj)
		} else {
			obj
		};
		let ret_expr = self.structural_value(ret_expr, identity, fields);
		let mut body_stmts = ArenaVec::new_in(&self.ast);
		body_stmts.push(Statement::new_return_statement(
			SPAN,
			Some(ret_expr),
			&self.ast,
		));
		let body = FunctionBody::new(SPAN, ArenaVec::new_in(&self.ast), body_stmts, &self.ast);
		let mut params = ArenaVec::new_in(&self.ast);
		params.push(FormalParameter::new_plain(
			SPAN,
			BindingPattern::new_binding_identifier(SPAN, "fields", &self.ast),
			&self.ast,
		));
		let formal = FormalParameters::new(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			params,
			oxc::ast::NONE,
			&self.ast,
		);
		Expression::new_arrow_function_expression(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			formal,
			oxc::ast::NONE,
			body,
			&self.ast,
		)
	}

	fn structural_value(
		&self,
		value: Expression<'a>,
		identity: &str,
		fields: &[EcoString],
	) -> Expression<'a> {
		let mut elements = ArenaVec::new_in(&self.ast);
		for field in fields {
			elements.push(ArrayExpressionElement::from(
				Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(field), None, &self.ast),
			));
		}
		let mut args = ArenaVec::new_in(&self.ast);
		args.push(Argument::from(value));
		args.push(Argument::from(Expression::new_string_literal(
			SPAN,
			self.ast.allocator.alloc_str(identity),
			None,
			&self.ast,
		)));
		args.push(Argument::from(Expression::new_array_expression(
			SPAN, elements, &self.ast,
		)));
		Expression::new_call_expression(
			SPAN,
			Expression::new_identifier(SPAN, "nymphStructuralValue", &self.ast),
			oxc::ast::NONE,
			args,
			false,
			&self.ast,
		)
	}

	/// A computed `[TAG]: <t_name>` object property.
	fn tag_prop(&self, t_name: &str) -> ObjectPropertyKind<'a> {
		let key = PropertyKey::from(Expression::new_identifier(SPAN, "TAG", &self.ast));
		let value = Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(t_name), &self.ast);
		ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
			SPAN,
			PropertyKind::Init,
			key,
			value,
			false,
			false,
			true,
			&self.ast,
		))
	}

	/// `const <name> = <init>;`
	fn const_decl(&self, name: &str, init: Expression<'a>) -> Statement<'a> {
		self.plain_decl(name, init, VariableDeclarationKind::Const)
	}

	fn plain_decl(
		&self,
		name: &str,
		init: Expression<'a>,
		kind: VariableDeclarationKind,
	) -> Statement<'a> {
		let pat =
			BindingPattern::new_binding_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast);
		let declarator = VariableDeclarator::new(
			SPAN,
			kind,
			pat,
			oxc::ast::NONE,
			Some(init),
			false,
			&self.ast,
		);
		let decl = VariableDeclaration::new(
			SPAN,
			kind,
			ArenaVec::from_value_in(declarator, &self.ast),
			false,
			&self.ast,
		);
		Statement::from(Declaration::VariableDeclaration(ArenaBox::new_in(
			decl, &self.ast,
		)))
	}

	/// `<enum>.<variant>` — a member access on the enum's ABI object (a factory or
	/// a frozen singleton).
	fn variant_member(&self, enum_name: &str, variant: &str) -> Expression<'a> {
		Expression::new_static_member_expression(
			SPAN,
			Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(enum_name), &self.ast),
			IdentifierName::new(SPAN, self.ast.allocator.alloc_str(variant), &self.ast),
			false,
			&self.ast,
		)
	}

	/// `new <class>(<payload>)` — a boxed primitive value (uniform value boxing,
	/// slice #2). `class` is a box wrapper name (`NInt`/`NString`/…); the wrapper
	/// stores `payload` in `.v` and carries its type discriminant on its
	/// prototype. Records the runtime binding so [`Self::emit_module`] can either
	/// provide the inline runtime or import it from the project runtime module.
	fn new_box(&self, class: &str, payload: Expression<'a>) -> Expression<'a> {
		self
			.box_runtime_bindings
			.borrow_mut()
			.insert(class.to_string());
		let callee = Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(class), &self.ast);
		let mut args = ArenaVec::new_in(&self.ast);
		args.push(Argument::from(payload));
		Expression::new_new_expression(SPAN, callee, oxc::ast::NONE, args, &self.ast)
	}

	fn direct_integer_box(&self, class: &str, payload: Expression<'a>) -> Expression<'a> {
		self
			.box_runtime_bindings
			.borrow_mut()
			.insert(class.to_string());
		let callee = Expression::new_static_member_expression(
			SPAN,
			Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(class), &self.ast),
			IdentifierName::new(SPAN, "direct", &self.ast),
			false,
			&self.ast,
		);
		let mut args = ArenaVec::new_in(&self.ast);
		args.push(Argument::from(payload));
		Expression::new_call_expression(SPAN, callee, oxc::ast::NONE, args, false, &self.ast)
	}

	/// `<expr>.v` — read a boxed value's raw payload (uniform value boxing,
	/// ADR-0002). The condition/logical-operator unwrap of slice #4:
	/// `ToBoolean(object)` is unconditionally `true`, so a user `boolean` in an
	/// `if`/`while`/guard condition or an `&&`/`||`/`!` slot must consult its raw
	/// `.v` payload rather than the (always-truthy) box.
	fn unwrap_v(&self, expr: Expression<'a>) -> Expression<'a> {
		Expression::new_static_member_expression(
			SPAN,
			expr,
			IdentifierName::new(SPAN, "v", &self.ast),
			false,
			&self.ast,
		)
	}

	/// Whether `cond`, in a condition slot, already evaluates to a raw JS boolean
	/// and so must not be `.v`-unwrapped. Only compiler-generated operator nodes
	/// carry `BuiltinResult::Raw`; user comparisons now produce boxed `NBool`s.
	fn cond_is_raw(cond: &HirExpr) -> bool {
		matches!(
			cond,
			HirExpr::Binary {
				result: BuiltinResult::Raw,
				..
			}
		)
	}

	/// Emit a user-condition/guard expression as a RAW JS boolean ready to drop
	/// into an `if`/`while`/ternary test: `emit_expr` then `.v`-unwrap, unless the
	/// expression already produces a raw boolean ([`Self::cond_is_raw`]).
	fn emit_cond(&self, cond: &HirExpr) -> Expression<'a> {
		let expr = self.emit_expr(cond);
		if Self::cond_is_raw(cond) {
			expr
		} else {
			self.unwrap_v(expr)
		}
	}

	/// `a && b` / `a || b` under uniform value boxing (ADR-0002). The boolean
	/// operands are boxed (`NBool`), so native `&&`/`||` can't short-circuit on the
	/// raw payload (a box is always truthy). Lower to the operand-reuse ternary
	/// that preserves short-circuit AND returns a box: `a && b` → `a.v ? b : a`,
	/// `a || b` → `a.v ? a : b`. `a` is used twice (as the test and as one branch),
	/// so a side-effecting `a` must be evaluated exactly once: a non-trivial `a` is
	/// bound once in an arrow-IIFE (`((t) => t.v ? b : t)(a)`), while a plain local
	/// or `this` — which have no side effects — is re-emitted directly. `b` sits in
	/// a ternary branch, so it is only evaluated when the branch is taken
	/// (short-circuit preserved).
	fn emit_logical(&self, op: BinOp, lhs: &HirExpr, rhs: &HirExpr) -> Expression<'a> {
		debug_assert!(matches!(op, BinOp::And | BinOp::Or));
		let right = self.emit_expr(rhs);
		let ternary = |this: &Self, test: Expression<'a>, reuse: Expression<'a>, right| {
			let (consequent, alternate) = if op == BinOp::And {
				(right, reuse)
			} else {
				(reuse, right)
			};
			Expression::new_conditional_expression(SPAN, test, consequent, alternate, &this.ast)
		};
		if matches!(lhs, HirExpr::Local(_) | HirExpr::This) {
			// Side-effect-free: re-emit the operand directly for both uses.
			let test = self.unwrap_v(self.emit_expr(lhs));
			ternary(self, test, self.emit_expr(lhs), right)
		} else {
			// Bind the operand once in an arrow-IIFE and reuse the gensym param.
			let param = self.ast.allocator.alloc_str(&self.gensym());
			let test = self.unwrap_v(self.ident(param));
			let body = ternary(self, test, self.ident(param), right);
			self.arrow_iife(param, body, self.emit_expr(lhs))
		}
	}

	/// `<callee>(<arg>)` — a single-argument call.
	fn call1(&self, callee: Expression<'a>, arg: Expression<'a>) -> Expression<'a> {
		let mut args = ArenaVec::new_in(&self.ast);
		args.push(Argument::from(arg));
		Expression::new_call_expression(SPAN, callee, oxc::ast::NONE, args, false, &self.ast)
	}

	fn runtime_call(&self, name: &str, args: Vec<Expression<'a>>) -> Expression<'a> {
		self
			.box_runtime_bindings
			.borrow_mut()
			.insert(name.to_string());
		let mut arguments = ArenaVec::new_in(&self.ast);
		for arg in args {
			arguments.push(Argument::from(arg));
		}
		Expression::new_call_expression(
			SPAN,
			self.ident(self.ast.allocator.alloc_str(name)),
			oxc::ast::NONE,
			arguments,
			false,
			&self.ast,
		)
	}

	/// `object.method(...args)`.
	fn member_call(
		&self,
		object: Expression<'a>,
		method: &str,
		args: Vec<Expression<'a>>,
	) -> Expression<'a> {
		let callee = Expression::new_static_member_expression(
			SPAN,
			object,
			IdentifierName::new(SPAN, self.ast.allocator.alloc_str(method), &self.ast),
			false,
			&self.ast,
		);
		let mut js_args = ArenaVec::new_in(&self.ast);
		for a in args {
			js_args.push(Argument::from(a));
		}
		Expression::new_call_expression(SPAN, callee, oxc::ast::NONE, js_args, false, &self.ast)
	}

	/// A bare global identifier reference or compiler-generated local.
	fn ident(&self, name: &'a str) -> Expression<'a> {
		Expression::new_identifier(SPAN, name, &self.ast)
	}

	/// `Math.<method>(<arg>)`.
	fn math_call(&self, method: &str, arg: Expression<'a>) -> Expression<'a> {
		let math = self.ident("Math");
		self.member_call(math, method, vec![arg])
	}

	/// `<left> === <right>`.
	fn strict_eq(&self, left: Expression<'a>, right: Expression<'a>) -> Expression<'a> {
		Expression::BinaryExpression(BinaryExpression::boxed(
			SPAN,
			left,
			BinaryOperator::StrictEquality,
			right,
			&self.ast,
		))
	}

	/// The numeric literal `0`.
	fn zero(&self) -> Expression<'a> {
		Expression::new_numeric_literal(SPAN, 0.0, None, NumberBase::Decimal, &self.ast)
	}

	fn bigint_literal(&self, value: impl ToString) -> Expression<'a> {
		let value = self.ast.allocator.alloc_str(&value.to_string());
		Expression::new_big_int_literal(SPAN, value, None, BigintBase::Decimal, &self.ast)
	}

	fn i64_literal(&self, value: i64) -> Expression<'a> {
		Expression::new_numeric_literal(SPAN, value as f64, None, NumberBase::Decimal, &self.ast)
	}

	/// `((<param>) => <body>)(<operand>)` — an arrow-IIFE that evaluates `operand`
	/// exactly once, as the sole call argument. `param` is a gensym, so it can
	/// never collide with a user identifier.
	fn arrow_iife(
		&self,
		param: &'a str,
		body: Expression<'a>,
		operand: Expression<'a>,
	) -> Expression<'a> {
		let mut body_stmts = ArenaVec::new_in(&self.ast);
		body_stmts.push(Statement::new_return_statement(SPAN, Some(body), &self.ast));
		let function_body = FunctionBody::new(SPAN, ArenaVec::new_in(&self.ast), body_stmts, &self.ast);
		let mut params = ArenaVec::new_in(&self.ast);
		params.push(FormalParameter::new_plain(
			SPAN,
			BindingPattern::new_binding_identifier(SPAN, param, &self.ast),
			&self.ast,
		));
		let formal = FormalParameters::new(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			params,
			oxc::ast::NONE,
			&self.ast,
		);
		let arrow = Expression::new_arrow_function_expression(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			formal,
			oxc::ast::NONE,
			function_body,
			&self.ast,
		);
		self.call1(arrow, operand)
	}

	fn emit_bound_dispatch(
		&self,
		method: &str,
		receiver: &HirExpr,
		argument: &HirExpr,
		hidden_arguments: &[HirExpr],
		cases: &[HirBoundDispatchCase],
		mode: nymph_hir::hir::HirCallMode,
		source: u32,
		continuation: Option<(u32, usize)>,
	) -> Expression<'a> {
		if let Some((resume_state, result_slot)) = continuation {
			return self.emit_bound_dispatch_state(
				method,
				receiver,
				argument,
				hidden_arguments,
				cases,
				mode,
				source,
				resume_state,
				result_slot,
			);
		}
		let receiver_param = self.gensym();
		let receiver_param = self.ast.allocator.alloc_str(&receiver_param);
		let argument_param = self.gensym();
		let argument_param = self.ast.allocator.alloc_str(&argument_param);

		let hidden_params = hidden_arguments
			.iter()
			.map(|_| self.ast.allocator.alloc_str(&self.gensym()))
			.collect::<Vec<_>>();
		let fallback_args = std::iter::once(self.ident(argument_param))
			.chain(hidden_params.iter().map(|name| self.ident(name)))
			.collect::<Vec<_>>();
		let mut body = if let Some((resume_state, result_slot)) = continuation {
			self.activation_member_packet(
				self.ident(receiver_param),
				method,
				fallback_args,
				mode,
				source,
				resume_state,
				result_slot,
			)
		} else {
			self.emit_member_activation(
				self.ident(receiver_param),
				method,
				fallback_args,
				mode,
				source,
			)
		};
		for case in cases.iter().rev() {
			let receiver_matches = self.strict_eq(
				self.tag_read(self.ident(receiver_param), true),
				self.global_symbol(&case.receiver_tag),
			);
			let argument_matches = self.strict_eq(
				self.tag_read(self.ident(argument_param), true),
				self.global_symbol(&case.argument_tag),
			);
			let test = Expression::new_logical_expression(
				SPAN,
				receiver_matches,
				LogicalOperator::And,
				argument_matches,
				&self.ast,
			);
			let dispatched = match &case.target {
				HirBoundDispatchTarget::TopLevel { module, name } => {
					let target_name = self.route_module_symbol(module, name, false);
					let args = std::iter::once(self.ident(receiver_param))
						.chain(std::iter::once(self.ident(argument_param)))
						.chain(hidden_params.iter().map(|name| self.ident(name)))
						.collect();
					if let Some((resume_state, result_slot)) = continuation {
						self.activation_packet(
							self.ident(self.ast.allocator.alloc_str(&target_name)),
							Expression::new_identifier(SPAN, "undefined", &self.ast),
							args,
							mode,
							source,
							resume_state,
							result_slot,
						)
					} else {
						self.emit_target_activation(
							self.ident(self.ast.allocator.alloc_str(&target_name)),
							args,
							mode,
							source,
						)
					}
				}
				HirBoundDispatchTarget::Extern {
					module,
					symbol,
					call_mode,
				} => {
					let target_name = self.route_module_symbol(module, symbol, true);
					let values = std::iter::once(self.ident(receiver_param))
						.chain(std::iter::once(self.ident(argument_param)))
						.chain(hidden_params.iter().map(|name| self.ident(name)))
						.collect::<Vec<_>>();
					let mut args = ArenaVec::new_in(&self.ast);
					args.extend(values.into_iter().map(Argument::from));
					if *call_mode == nymph_hir::hir::ExternalCallMode::Cancellable {
						args.push(Argument::from(
							self.runtime_call("nymphCurrentExecutionSignal", vec![]),
						));
					}
					let call = Expression::new_call_expression(
						SPAN,
						self.ident(self.ast.allocator.alloc_str(&target_name)),
						oxc::ast::NONE,
						args,
						false,
						&self.ast,
					);
					self.activation_direct_result(call, mode, continuation)
				}
			};
			body = Expression::new_conditional_expression(SPAN, test, dispatched, body, &self.ast);
		}

		for (name, argument) in hidden_params.iter().zip(hidden_arguments).rev() {
			body = self.arrow_iife(name, body, self.emit_expr(argument));
		}
		let argument_iife = self.arrow_iife(argument_param, body, self.emit_expr(argument));
		self.arrow_iife(receiver_param, argument_iife, self.emit_expr(receiver))
	}

	fn emit_unary_bound_dispatch(
		&self,
		method: &str,
		receiver: &HirExpr,
		hidden_arguments: &[HirExpr],
		cases: &[HirBoundDispatchCase],
		mode: nymph_hir::hir::HirCallMode,
		source: u32,
		continuation: Option<(u32, usize)>,
	) -> Expression<'a> {
		if let Some((resume_state, result_slot)) = continuation {
			return self.emit_unary_bound_dispatch_state(
				method,
				receiver,
				hidden_arguments,
				cases,
				mode,
				source,
				resume_state,
				result_slot,
			);
		}
		let receiver_param = self.gensym();
		let receiver_param = self.ast.allocator.alloc_str(&receiver_param);
		let hidden_params = hidden_arguments
			.iter()
			.map(|_| self.ast.allocator.alloc_str(&self.gensym()))
			.collect::<Vec<_>>();
		let fallback_args = hidden_params
			.iter()
			.map(|name| self.ident(name))
			.collect::<Vec<_>>();
		let mut body = if let Some((resume_state, result_slot)) = continuation {
			self.activation_member_packet(
				self.ident(receiver_param),
				method,
				fallback_args,
				mode,
				source,
				resume_state,
				result_slot,
			)
		} else {
			self.emit_member_activation(
				self.ident(receiver_param),
				method,
				fallback_args,
				mode,
				source,
			)
		};
		for case in cases.iter().rev() {
			let test = self.strict_eq(
				self.tag_read(self.ident(receiver_param), true),
				self.global_symbol(&case.receiver_tag),
			);
			let dispatched = match &case.target {
				HirBoundDispatchTarget::TopLevel { module, name } => {
					let target_name = self.route_module_symbol(module, name, false);
					let args = std::iter::once(self.ident(receiver_param))
						.chain(hidden_params.iter().map(|name| self.ident(name)))
						.collect();
					if let Some((resume_state, result_slot)) = continuation {
						self.activation_packet(
							self.ident(self.ast.allocator.alloc_str(&target_name)),
							Expression::new_identifier(SPAN, "undefined", &self.ast),
							args,
							mode,
							source,
							resume_state,
							result_slot,
						)
					} else {
						self.emit_target_activation(
							self.ident(self.ast.allocator.alloc_str(&target_name)),
							args,
							mode,
							source,
						)
					}
				}
				HirBoundDispatchTarget::Extern {
					module,
					symbol,
					call_mode,
				} => {
					let target_name = self.route_module_symbol(module, symbol, true);
					let values = std::iter::once(self.ident(receiver_param))
						.chain(hidden_params.iter().map(|name| self.ident(name)))
						.collect::<Vec<_>>();
					let mut args = ArenaVec::new_in(&self.ast);
					args.extend(values.into_iter().map(Argument::from));
					if *call_mode == nymph_hir::hir::ExternalCallMode::Cancellable {
						args.push(Argument::from(
							self.runtime_call("nymphCurrentExecutionSignal", vec![]),
						));
					}
					let call = Expression::new_call_expression(
						SPAN,
						self.ident(self.ast.allocator.alloc_str(&target_name)),
						oxc::ast::NONE,
						args,
						false,
						&self.ast,
					);
					self.activation_direct_result(call, mode, continuation)
				}
			};
			body = Expression::new_conditional_expression(SPAN, test, dispatched, body, &self.ast);
		}
		for (name, argument) in hidden_params.iter().zip(hidden_arguments).rev() {
			body = self.arrow_iife(name, body, self.emit_expr(argument));
		}
		self.arrow_iife(receiver_param, body, self.emit_expr(receiver))
	}

	fn emit_bound_dispatch_state(
		&self,
		method: &str,
		receiver: &HirExpr,
		argument: &HirExpr,
		hidden_arguments: &[HirExpr],
		cases: &[HirBoundDispatchCase],
		mode: HirCallMode,
		source: u32,
		resume_state: u32,
		result_slot: usize,
	) -> Expression<'a> {
		let receiver = self.emit_expr(receiver);
		let argument = self.emit_expr(argument);
		let hidden = hidden_arguments
			.iter()
			.map(|argument| self.emit_expr(argument))
			.collect::<Vec<_>>();
		let fallback_args = std::iter::once(argument.clone_in(self.ast.allocator))
			.chain(
				hidden
					.iter()
					.map(|value| value.clone_in(self.ast.allocator)),
			)
			.collect();
		let mut body = self.activation_member_packet(
			receiver.clone_in(self.ast.allocator),
			method,
			fallback_args,
			mode,
			source,
			resume_state,
			result_slot,
		);
		for case in cases.iter().rev() {
			let receiver_matches = self.strict_eq(
				self.tag_read(receiver.clone_in(self.ast.allocator), true),
				self.global_symbol(&case.receiver_tag),
			);
			let argument_matches = self.strict_eq(
				self.tag_read(argument.clone_in(self.ast.allocator), true),
				self.global_symbol(&case.argument_tag),
			);
			let test = Expression::new_logical_expression(
				SPAN,
				receiver_matches,
				LogicalOperator::And,
				argument_matches,
				&self.ast,
			);
			let values = std::iter::once(receiver.clone_in(self.ast.allocator))
				.chain(std::iter::once(argument.clone_in(self.ast.allocator)))
				.chain(
					hidden
						.iter()
						.map(|value| value.clone_in(self.ast.allocator)),
				)
				.collect::<Vec<_>>();
			let dispatched = match &case.target {
				HirBoundDispatchTarget::TopLevel { module, name } => {
					let target_name = self.route_module_symbol(module, name, false);
					self.activation_packet(
						self.ident(self.ast.allocator.alloc_str(&target_name)),
						Expression::new_identifier(SPAN, "undefined", &self.ast),
						values,
						mode,
						source,
						resume_state,
						result_slot,
					)
				}
				HirBoundDispatchTarget::Extern {
					module,
					symbol,
					call_mode,
				} => {
					let target_name = self.route_module_symbol(module, symbol, true);
					let mut args = ArenaVec::new_in(&self.ast);
					args.extend(values.into_iter().map(Argument::from));
					if *call_mode == nymph_hir::hir::ExternalCallMode::Cancellable {
						args.push(Argument::from(
							self.runtime_call("nymphCurrentExecutionSignal", vec![]),
						));
					}
					let call = Expression::new_call_expression(
						SPAN,
						self.ident(self.ast.allocator.alloc_str(&target_name)),
						oxc::ast::NONE,
						args,
						false,
						&self.ast,
					);
					self.activation_direct_result(call, mode, Some((resume_state, result_slot)))
				}
			};
			body = Expression::new_conditional_expression(SPAN, test, dispatched, body, &self.ast);
		}
		body
	}

	fn emit_unary_bound_dispatch_state(
		&self,
		method: &str,
		receiver: &HirExpr,
		hidden_arguments: &[HirExpr],
		cases: &[HirBoundDispatchCase],
		mode: HirCallMode,
		source: u32,
		resume_state: u32,
		result_slot: usize,
	) -> Expression<'a> {
		let receiver = self.emit_expr(receiver);
		let hidden = hidden_arguments
			.iter()
			.map(|argument| self.emit_expr(argument))
			.collect::<Vec<_>>();
		let mut body = self.activation_member_packet(
			receiver.clone_in(self.ast.allocator),
			method,
			hidden
				.iter()
				.map(|value| value.clone_in(self.ast.allocator))
				.collect(),
			mode,
			source,
			resume_state,
			result_slot,
		);
		for case in cases.iter().rev() {
			let test = self.strict_eq(
				self.tag_read(receiver.clone_in(self.ast.allocator), true),
				self.global_symbol(&case.receiver_tag),
			);
			let values = std::iter::once(receiver.clone_in(self.ast.allocator))
				.chain(
					hidden
						.iter()
						.map(|value| value.clone_in(self.ast.allocator)),
				)
				.collect::<Vec<_>>();
			let dispatched = match &case.target {
				HirBoundDispatchTarget::TopLevel { module, name } => {
					let target_name = self.route_module_symbol(module, name, false);
					self.activation_packet(
						self.ident(self.ast.allocator.alloc_str(&target_name)),
						Expression::new_identifier(SPAN, "undefined", &self.ast),
						values,
						mode,
						source,
						resume_state,
						result_slot,
					)
				}
				HirBoundDispatchTarget::Extern {
					module,
					symbol,
					call_mode,
				} => {
					let target_name = self.route_module_symbol(module, symbol, true);
					let mut args = ArenaVec::new_in(&self.ast);
					args.extend(values.into_iter().map(Argument::from));
					if *call_mode == nymph_hir::hir::ExternalCallMode::Cancellable {
						args.push(Argument::from(
							self.runtime_call("nymphCurrentExecutionSignal", vec![]),
						));
					}
					let call = Expression::new_call_expression(
						SPAN,
						self.ident(self.ast.allocator.alloc_str(&target_name)),
						oxc::ast::NONE,
						args,
						false,
						&self.ast,
					);
					self.activation_direct_result(call, mode, Some((resume_state, result_slot)))
				}
			};
			body = Expression::new_conditional_expression(SPAN, test, dispatched, body, &self.ast);
		}
		body
	}

	fn activation_direct_result(
		&self,
		value: Expression<'a>,
		mode: HirCallMode,
		continuation: Option<(u32, usize)>,
	) -> Expression<'a> {
		let Some((resume_state, result_slot)) = continuation else {
			return value;
		};
		if mode == HirCallMode::Tail {
			return self.runtime_call("nymphReturn", vec![value]);
		}
		self.runtime_call(
			"nymphResume",
			vec![
				value,
				self.activation_state_number(resume_state),
				Expression::new_numeric_literal(
					SPAN,
					result_slot as f64,
					None,
					NumberBase::Decimal,
					&self.ast,
				),
			],
		)
	}

	fn route_module_symbol(&self, module: &str, symbol: &str, external: bool) -> String {
		if self.current_module.as_deref() == Some(module) && !external {
			return symbol.to_string();
		}
		let import_module = if self.current_module.as_deref() == Some(module) {
			format!("{module}$intrinsics")
		} else {
			module.to_string()
		};
		let local = if external {
			external_alias(module, symbol, "call$")
		} else {
			symbol.to_string()
		};
		self
			.needed_imports
			.borrow_mut()
			.insert((import_module, symbol.to_string(), local.clone()));
		local
	}

	fn numeric_to_char(&self, operand: Expression<'a>, truncate: bool) -> Expression<'a> {
		let param = self.gensym();
		let param = self.ast.allocator.alloc_str(&param);
		let value = || {
			if truncate {
				self.math_call("trunc", self.ident(param))
			} else {
				self.ident(param)
			}
		};
		let ge_zero = Expression::new_binary_expression(
			SPAN,
			value(),
			BinaryOperator::GreaterEqualThan,
			self.zero(),
			&self.ast,
		);
		let le_max = Expression::new_binary_expression(
			SPAN,
			value(),
			BinaryOperator::LessEqualThan,
			self.i64_literal(0x10_FFFF),
			&self.ast,
		);
		let below_surrogates = Expression::new_binary_expression(
			SPAN,
			value(),
			BinaryOperator::LessThan,
			self.i64_literal(0xD800),
			&self.ast,
		);
		let above_surrogates = Expression::new_binary_expression(
			SPAN,
			value(),
			BinaryOperator::GreaterThan,
			self.i64_literal(0xDFFF),
			&self.ast,
		);
		let in_range =
			Expression::new_logical_expression(SPAN, ge_zero, LogicalOperator::And, le_max, &self.ast);
		let outside_surrogates = Expression::new_logical_expression(
			SPAN,
			below_surrogates,
			LogicalOperator::Or,
			above_surrogates,
			&self.ast,
		);
		let valid = Expression::new_logical_expression(
			SPAN,
			in_range,
			LogicalOperator::And,
			outside_surrogates,
			&self.ast,
		);
		let string = || Expression::new_identifier(SPAN, "String", &self.ast);
		let converted = self.member_call(string(), "fromCodePoint", vec![value()]);
		// Use the host's canonical `RangeError: Invalid code point` path after our
		// stricter Unicode-scalar check (JS itself accepts lone surrogates).
		let rejected = self.member_call(string(), "fromCodePoint", vec![self.i64_literal(-1)]);
		let body = Expression::new_conditional_expression(SPAN, valid, converted, rejected, &self.ast);
		self.arrow_iife(param, body, operand)
	}

	fn emit_echo_site(&self, site: &nymph_hir::hir::EchoSite) -> Expression<'a> {
		let EchoEmission::Development {
			source_name,
			source_uri,
			source,
		} = &self.echo_emission
		else {
			unreachable!("release echo sites are erased before site emission")
		};
		let mut offset = usize::try_from(site.start)
			.unwrap_or(usize::MAX)
			.min(source.len());
		while !source.is_char_boundary(offset) {
			offset -= 1;
		}
		let prefix = &source[..offset];
		let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
		let column = prefix
			.rsplit_once('\n')
			.map_or(prefix, |(_, tail)| tail)
			.chars()
			.count()
			+ 1;
		let file = source_name
			.rsplit(['/', '\\'])
			.next()
			.filter(|name| !name.is_empty())
			.unwrap_or(source_name);
		let mut properties = ArenaVec::new_in(&self.ast);
		let mut property = |name: &'a str, value: Expression<'a>| {
			properties.push(ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
				SPAN,
				PropertyKind::Init,
				PropertyKey::new_static_identifier(SPAN, name, &self.ast),
				value,
				false,
				false,
				false,
				&self.ast,
			)));
		};
		property(
			"file",
			Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(file), None, &self.ast),
		);
		property(
			"line",
			Expression::new_numeric_literal(SPAN, line as f64, None, NumberBase::Decimal, &self.ast),
		);
		property(
			"column",
			Expression::new_numeric_literal(SPAN, column as f64, None, NumberBase::Decimal, &self.ast),
		);
		property(
			"uri",
			source_uri.as_ref().map_or_else(
				|| Expression::new_null_literal(SPAN, &self.ast),
				|uri| {
					Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(uri), None, &self.ast)
				},
			),
		);
		Expression::new_object_expression(SPAN, properties, &self.ast)
	}

	fn emit_expr(&self, expr: &HirExpr) -> Expression<'a> {
		match expr {
			// Exact integers are BigInt payloads. `NumKind::Raw` is compiler-internal
			// scaffolding and stays an unboxed Number; floats are boxed Numbers.
			HirExpr::Int(value) => self.new_box("NInt", self.bigint_literal(value)),
			HirExpr::UInt(value) => self.new_box("NUint", self.bigint_literal(value)),
			HirExpr::Num(value, kind) => {
				let raw =
					Expression::new_numeric_literal(SPAN, *value, None, NumberBase::Decimal, &self.ast);
				match kind {
					NumKind::Raw => raw,
					NumKind::Float => self.new_box(box_rt::num_box_class(*kind), raw),
				}
			}
			// String/char/bool literals have an unambiguous box type.
			HirExpr::Str(s) => {
				let raw =
					Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(s), None, &self.ast);
				self.new_box("NString", raw)
			}
			HirExpr::InterpolatedString(segments) => {
				let mut segments = segments.iter().map(|segment| match segment {
					HirExpr::Str(text) => Expression::new_string_literal(
						SPAN,
						self.ast.allocator.alloc_str(text),
						None,
						&self.ast,
					),
					other => self.unwrap_v(self.emit_expr(other)),
				});
				let first = segments
					.next()
					.unwrap_or_else(|| Expression::new_string_literal(SPAN, "", None, &self.ast));
				let raw = segments.fold(first, |left, right| {
					Expression::BinaryExpression(BinaryExpression::boxed(
						SPAN,
						left,
						BinaryOperator::Addition,
						right,
						&self.ast,
					))
				});
				self.new_box("NString", raw)
			}
			HirExpr::ProtocolDisplay(value) => {
				self
					.box_runtime_bindings
					.borrow_mut()
					.insert("nymphProtocolDisplay".to_string());
				self.runtime_call("nymphProtocolDisplay", vec![self.emit_expr(value)])
			}
			HirExpr::Bool(b) => {
				let raw = Expression::new_boolean_literal(SPAN, *b, &self.ast);
				self.new_box("NBool", raw)
			}
			HirExpr::Char(c) => {
				// A Nymph char is a single-character JS string.
				let s = self.ast.allocator.alloc_str(&c.to_string());
				let raw = Expression::new_string_literal(SPAN, s, None, &self.ast);
				self.new_box("NChar", raw)
			}
			HirExpr::Undefined => Expression::new_unary_expression(
				SPAN,
				UnaryOperator::Void,
				Expression::new_numeric_literal(SPAN, 0.0, None, NumberBase::Decimal, &self.ast),
				&self.ast,
			),
			HirExpr::Local(name) => self.local_read(name),
			HirExpr::Echo { operand, site } => {
				if matches!(self.echo_emission, EchoEmission::Release) {
					self.emit_expr(operand)
				} else {
					self
						.box_runtime_bindings
						.borrow_mut()
						.insert("nymphEcho".to_string());
					let mut arguments = ArenaVec::new_in(&self.ast);
					arguments.push(Argument::from(self.emit_expr(operand)));
					arguments.push(Argument::from(self.emit_echo_site(site)));
					Expression::new_call_expression(
						SPAN,
						Expression::new_identifier(SPAN, "nymphEcho", &self.ast),
						oxc::ast::NONE,
						arguments,
						false,
						&self.ast,
					)
				}
			}
			HirExpr::RuntimeTypeObject {
				binding,
				box_runtime,
				is_enum,
				arguments,
			} => {
				if *box_runtime {
					self
						.box_runtime_bindings
						.borrow_mut()
						.insert(binding.to_string());
				}
				let object =
					Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(binding), &self.ast);
				let base = Expression::new_static_member_expression(
					SPAN,
					object,
					IdentifierName::new(
						SPAN,
						self
							.ast
							.allocator
							.alloc_str(if *is_enum { "$nymph$type" } else { "prototype" }),
						&self.ast,
					),
					false,
					&self.ast,
				);
				if arguments.is_empty() {
					base
				} else {
					self
						.box_runtime_bindings
						.borrow_mut()
						.insert("nymphType".to_string());
					let mut elements = ArenaVec::new_in(&self.ast);
					for argument in arguments {
						elements.push(oxc::ast::ast::ArrayExpressionElement::from(
							self.emit_expr(argument),
						));
					}
					let array = Expression::new_array_expression(SPAN, elements, &self.ast);
					let mut call_args = ArenaVec::new_in(&self.ast);
					call_args.push(Argument::from(base));
					call_args.push(Argument::from(array));
					Expression::new_call_expression(
						SPAN,
						Expression::new_identifier(SPAN, "nymphType", &self.ast),
						oxc::ast::NONE,
						call_args,
						false,
						&self.ast,
					)
				}
			}
			HirExpr::RuntimeTypeProjection { receiver, path } => {
				self
					.box_runtime_bindings
					.borrow_mut()
					.insert("nymphTypeProjection".to_string());
				let mut elements = ArenaVec::new_in(&self.ast);
				for index in path {
					elements.push(oxc::ast::ast::ArrayExpressionElement::from(
						Expression::new_numeric_literal(
							SPAN,
							*index as f64,
							None,
							NumberBase::Decimal,
							&self.ast,
						),
					));
				}
				let mut args = ArenaVec::new_in(&self.ast);
				args.push(Argument::from(self.emit_expr(receiver)));
				args.push(Argument::from(Expression::new_array_expression(
					SPAN, elements, &self.ast,
				)));
				Expression::new_call_expression(
					SPAN,
					Expression::new_identifier(SPAN, "nymphTypeProjection", &self.ast),
					oxc::ast::NONE,
					args,
					false,
					&self.ast,
				)
			}
			HirExpr::WithPrototype { value, prototype } => {
				self.set_prototype(self.emit_expr(value), self.emit_expr(prototype))
			}
			HirExpr::RuntimeTypeAttachment { object, method } => {
				let object = self.emit_expr(object);
				let mut properties = ArenaVec::new_in(&self.ast);
				properties.push(self.emit_method_property(method));
				let methods = Expression::new_object_expression(SPAN, properties, &self.ast);
				self.object_assign(vec![object, methods])
			}
			// The `this` receiver.
			HirExpr::This => Expression::new_this_expression(SPAN, &self.ast),
			// `&&`/`||` need the boxed operands' raw payloads to short-circuit, so
			// they lower to an operand-reuse ternary (`emit_logical`), NOT a native
			// JS logical op — see its doc comment. Every other binary op composes its
			// already-emitted operands in `emit_binary`.
			HirExpr::Binary {
				op: op @ (BinOp::And | BinOp::Or),
				lhs,
				rhs,
				..
			} => self.emit_logical(*op, lhs, rhs),
			HirExpr::Binary {
				op,
				result,
				mode,
				lhs,
				rhs,
			} => {
				let left = self.emit_expr(lhs);
				let right = self.emit_expr(rhs);
				self.emit_binary(*op, *result, *mode, left, right)
			}
			// `!x` reads the raw boolean payload, negates it, and re-boxes:
			// `new NBool(!x.v)` — `!box` is always `false` (a box is truthy), so the
			// native operator can't run on the box (uniform value boxing, ADR-0002).
			// `Neg`/`BitNot` are arithmetic (still broken until slice #10a) and stay
			// as bare native unary ops over the operand.
			HirExpr::Unary {
				op: UnOp::Not,
				operand,
				..
			} => {
				let raw = self.emit_cond(operand);
				let negated =
					Expression::new_unary_expression(SPAN, UnaryOperator::LogicalNot, raw, &self.ast);
				self.new_box("NBool", negated)
			}
			HirExpr::Unary {
				op,
				result,
				operand,
			} => {
				let inner = self.emit_expr(operand);
				let inner = if *result == BuiltinResult::Raw {
					inner
				} else {
					self.unwrap_v(inner)
				};
				let operator = match op {
					UnOp::Neg => UnaryOperator::UnaryNegation,
					UnOp::Not => unreachable!("UnOp::Not is handled above"),
					UnOp::BitNot => UnaryOperator::BitwiseNot,
				};
				let raw = Expression::new_unary_expression(SPAN, operator, inner, &self.ast);
				self.box_builtin_result(*result, raw)
			}
			HirExpr::Call { callee, args } => {
				let callee = self.emit_expr(callee);
				let mut arguments = ArenaVec::new_in(&self.ast);
				for arg in args {
					arguments.push(Argument::from(self.emit_expr(arg)));
				}
				Expression::new_call_expression(SPAN, callee, oxc::ast::NONE, arguments, false, &self.ast)
			}
			HirExpr::ActivationCall {
				callee,
				args,
				mode,
				source,
			} => self.emit_activation_call(callee, args, *mode, *source),
			HirExpr::TaskRecipe { body, context } => {
				let callable = self.activation_callable(&[], body, true);
				let nested = Expression::new_boolean_literal(
					SPAN,
					*context == nymph_hir::hir::HirTaskContext::Nested,
					&self.ast,
				);
				self.runtime_call("nymphTaskRecipe", vec![callable, nested])
			}
			HirExpr::TaskOperation {
				operation,
				operands,
			} => self.emit_task_operation(*operation, operands),
			HirExpr::StaticEnumDispatch {
				owner,
				method,
				receiver,
				args,
				mode,
				source,
			} => {
				let prototype = Expression::new_static_member_expression(
					SPAN,
					self.local_read(owner),
					IdentifierName::new(SPAN, "$nymph$type", &self.ast),
					false,
					&self.ast,
				);
				let method = Expression::new_static_member_expression(
					SPAN,
					prototype,
					IdentifierName::new(SPAN, self.ast.allocator.alloc_str(method), &self.ast),
					false,
					&self.ast,
				);
				let mut elements = ArenaVec::new_in(&self.ast);
				for argument in args {
					elements.push(ArrayExpressionElement::from(self.emit_expr(argument)));
				}
				let arguments = Expression::new_array_expression(SPAN, elements, &self.ast);
				let source = Expression::new_numeric_literal(
					SPAN,
					f64::from(*source),
					None,
					NumberBase::Decimal,
					&self.ast,
				);
				self.runtime_call(
					if *mode == nymph_hir::hir::HirCallMode::Tail {
						"nymphTailCall"
					} else {
						"nymphActivate"
					},
					vec![method, self.emit_expr(receiver), arguments, source],
				)
			}
			// Gap 3 (L0/L1): a call resolved through the linkage registry —
			// `module`/`symbol` are already the resolved `Linked` fields
			// (lowering did the receiver-tag-disambiguated lookup; see
			// `HirExpr::ExternCall`'s own doc comment for why emit never
			// re-`lookup`s by marker). Emit a plain call to the linked JS
			// symbol, `$_this`-first (`args` already carries the receiver as
			// its first element), and record the `(module, symbol)` pair so
			// `emit_module` can prepend the `import` it needs.
			HirExpr::ExternCall {
				module,
				symbol,
				args,
				call_mode,
				argument_marshals,
				return_marshal,
			} => {
				let callee_name = self.route_module_symbol(module, symbol, true);
				let callee =
					Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(&callee_name), &self.ast);
				let mut arguments = ArenaVec::new_in(&self.ast);
				for (arg, marshal) in args.iter().zip(argument_marshals) {
					let argument = self.emit_expr(arg);
					let argument = match marshal {
						Some(nymph_hir::hir::MarshalKind::Int | nymph_hir::hir::MarshalKind::UInt) => {
							self.unwrap_v(argument)
						}
						Some(nymph_hir::hir::MarshalKind::Opaque(identity)) => {
							let identity = self.bigint_literal(identity);
							self.runtime_call("nymphUnboxOpaque", vec![identity, argument])
						}
						_ => argument,
					};
					arguments.push(Argument::from(argument));
				}
				if *call_mode == nymph_hir::hir::ExternalCallMode::Cancellable {
					arguments.push(Argument::from(
						self.runtime_call("nymphCurrentExecutionSignal", vec![]),
					));
				}
				let call = Expression::new_call_expression(
					SPAN,
					callee,
					oxc::ast::NONE,
					arguments,
					false,
					&self.ast,
				);
				match return_marshal {
					Some(nymph_hir::hir::MarshalKind::Int) => {
						let checked = self.runtime_call("nymphTrustedInt", vec![call]);
						self.new_box("NInt", checked)
					}
					Some(nymph_hir::hir::MarshalKind::UInt) => {
						let checked = self.runtime_call("nymphTrustedUInt", vec![call]);
						self.new_box("NUint", checked)
					}
					Some(nymph_hir::hir::MarshalKind::Opaque(identity)) => {
						let identity = self.bigint_literal(identity);
						self.runtime_call("nymphBoxOpaque", vec![identity, call])
					}
					_ => call,
				}
			}
			HirExpr::ExternValue {
				module,
				symbol,
				marshal,
			} => {
				if let Some(step) = activation_protocol_step(module, symbol) {
					self
						.box_runtime_bindings
						.borrow_mut()
						.insert(step.to_string());
					return Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(step), &self.ast);
				}
				let local = external_alias(module, symbol, "value$");
				self.needed_imports.borrow_mut().insert((
					module.to_string(),
					symbol.to_string(),
					local.clone(),
				));
				let raw = Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(&local), &self.ast);
				let raw = match marshal {
					nymph_hir::hir::MarshalKind::Int => self.runtime_call("nymphTrustedInt", vec![raw]),
					nymph_hir::hir::MarshalKind::UInt => self.runtime_call("nymphTrustedUInt", vec![raw]),
					nymph_hir::hir::MarshalKind::Opaque(identity) => {
						let identity = self.bigint_literal(identity);
						return self.runtime_call("nymphBoxOpaque", vec![identity, raw]);
					}
					_ => raw,
				};
				let class = match marshal {
					nymph_hir::hir::MarshalKind::Int => "NInt",
					nymph_hir::hir::MarshalKind::UInt => "NUint",
					nymph_hir::hir::MarshalKind::Float => "NFloat",
					nymph_hir::hir::MarshalKind::Char => "NChar",
					nymph_hir::hir::MarshalKind::String => "NString",
					nymph_hir::hir::MarshalKind::Boolean => "NBool",
					nymph_hir::hir::MarshalKind::List => "NList",
					nymph_hir::hir::MarshalKind::Tuple => "NTuple",
					nymph_hir::hir::MarshalKind::Map => "NMap",
					nymph_hir::hir::MarshalKind::Opaque(_) => unreachable!(),
				};
				self.new_box(class, raw)
			}
			HirExpr::BoundDispatch {
				method,
				receiver,
				argument,
				hidden_arguments,
				cases,
				mode,
				source,
				..
			} => self.emit_bound_dispatch(
				method,
				receiver,
				argument,
				hidden_arguments,
				cases,
				*mode,
				*source,
				None,
			),
			HirExpr::UnaryBoundDispatch {
				method,
				receiver,
				hidden_arguments,
				cases,
				mode,
				source,
				..
			} => self.emit_unary_bound_dispatch(
				method,
				receiver,
				hidden_arguments,
				cases,
				*mode,
				*source,
				None,
			),
			HirExpr::ListConstruct(elems) => {
				let mut items = ArenaVec::new_in(&self.ast);
				for elem in elems {
					match elem {
						HirArrayElem::Item(value) => {
							items.push(ArrayExpressionElement::from(self.emit_expr(value)));
						}
						HirArrayElem::Spread(value) => {
							items.push(ArrayExpressionElement::new_spread_element(
								SPAN,
								self.emit_expr(value),
								&self.ast,
							));
						}
					}
				}
				let array = Expression::new_array_expression(SPAN, items, &self.ast);
				self.new_box("NList", array)
			}
			HirExpr::ListRead { recv, index, mode } => {
				let object = self.emit_expr(recv);
				let key = self.emit_expr(index);
				let method = if *mode == OperationMode::Direct {
					"indexDirect"
				} else {
					"index"
				};
				self.member_call(object, method, vec![key])
			}
			HirExpr::ListAppend { recv, value } => {
				let object = self.emit_expr(recv);
				let value = self.emit_expr(value);
				self.member_call(object, "appended", vec![value])
			}
			HirExpr::ListReplace { recv, index, value } => {
				let object = self.emit_expr(recv);
				let index = self.emit_expr(index);
				let value = self.emit_expr(value);
				self.member_call(object, "replaced", vec![index, value])
			}
			HirExpr::ListSlice { recv, start, end } => {
				let object = self.emit_expr(recv);
				let start = self.emit_expr(start);
				let end = self.emit_expr(end);
				self.member_call(object, "slice", vec![start, end])
			}
			// Collection literals own a native array payload; compiler-internal
			// accumulators remain raw arrays.
			HirExpr::Array { kind, items } => {
				let mut elems = ArenaVec::new_in(&self.ast);
				for item in items {
					elems.push(ArrayExpressionElement::from(self.emit_expr(item)));
				}
				let array = Expression::new_array_expression(SPAN, elems, &self.ast);
				match kind {
					HirArrayKind::List => self.new_box("NList", array),
					HirArrayKind::Tuple => self.new_box("NTuple", array),
					HirArrayKind::Raw => array,
				}
			}
			// A list literal with at least one spread element (SS1) → a JS array
			// `[a, ...xs, b]`, preserving left-to-right source order. Each
			// `HirArrayElem::Spread` payload is already a JS-array-valued
			// expression (a native source or a `lower_spread_source` drain IIFE),
			// so it always emits with JS spread syntax.
			HirExpr::ArraySpread { kind, elems } => {
				let mut arr = ArenaVec::new_in(&self.ast);
				for elem in elems {
					match elem {
						HirArrayElem::Item(e) => arr.push(ArrayExpressionElement::from(self.emit_expr(e))),
						HirArrayElem::Spread(e) => {
							let argument = self.emit_expr(e);
							arr.push(ArrayExpressionElement::new_spread_element(
								SPAN, argument, &self.ast,
							));
						}
					}
				}
				let array = Expression::new_array_expression(SPAN, arr, &self.ast);
				match kind {
					HirArrayKind::List => self.new_box("NList", array),
					HirArrayKind::Tuple => self.new_box("NTuple", array),
					HirArrayKind::Raw => array,
				}
			}
			// A map literal → a boxed value-equality HAMT.
			HirExpr::MapLit(pairs) => {
				let mut entries = ArenaVec::new_in(&self.ast);
				for (k, v) in pairs {
					let mut pair = ArenaVec::new_in(&self.ast);
					pair.push(ArrayExpressionElement::from(self.emit_expr(k)));
					pair.push(ArrayExpressionElement::from(self.emit_expr(v)));
					let arr = Expression::new_array_expression(SPAN, pair, &self.ast);
					entries.push(ArrayExpressionElement::from(arr));
				}
				let outer = Expression::new_array_expression(SPAN, entries, &self.ast);
				self.new_box("NMap", outer)
			}
			// A map literal with at least one spread entry (SS1) → `new NMap([...])`
			// merging the spread entries in, left-to-right (a later duplicate key
			// wins — the `Map` constructor processes its entries array in order,
			// SS4). Each `HirMapElem::Spread` payload is already an array of
			// `[k, v]` pairs (an `NMap` iterates as `[k, v]` pairs, or a
			// `lower_spread_source` drain IIFE), so it always emits with JS spread
			// syntax inside the entries array.
			HirExpr::MapSpread(elems) => {
				let mut entries = ArenaVec::new_in(&self.ast);
				for elem in elems {
					match elem {
						HirMapElem::Entry(k, v) => {
							let mut pair = ArenaVec::new_in(&self.ast);
							pair.push(ArrayExpressionElement::from(self.emit_expr(k)));
							pair.push(ArrayExpressionElement::from(self.emit_expr(v)));
							let arr = Expression::new_array_expression(SPAN, pair, &self.ast);
							entries.push(ArrayExpressionElement::from(arr));
						}
						HirMapElem::Spread(e) => {
							let argument = self.emit_expr(e);
							entries.push(ArrayExpressionElement::new_spread_element(
								SPAN, argument, &self.ast,
							));
						}
					}
				}
				let outer = Expression::new_array_expression(SPAN, entries, &self.ast);
				self.new_box("NMap", outer)
			}
			// A collection subscript dispatches through its boxed wrapper.
			HirExpr::Index { recv, index, mode } => {
				let object = self.emit_expr(recv);
				let key = self.emit_expr(index);
				self.member_call(
					object,
					if *mode == OperationMode::Direct {
						"indexDirect"
					} else {
						"index"
					},
					vec![key],
				)
			}
			HirExpr::Slice {
				recv,
				start,
				end,
				inclusive,
				string,
				mode,
			} => {
				let recv = self.emit_expr(recv);
				let start = start.as_ref().map_or_else(
					|| Expression::new_null_literal(SPAN, &self.ast),
					|value| self.unwrap_v(self.emit_expr(value)),
				);
				let end = end.as_ref().map_or_else(
					|| Expression::new_null_literal(SPAN, &self.ast),
					|value| self.unwrap_v(self.emit_expr(value)),
				);
				self.runtime_call(
					if *string {
						"nymphStringSlice"
					} else {
						"nymphListSlice"
					},
					vec![
						recv,
						start,
						end,
						Expression::new_boolean_literal(SPAN, *inclusive, &self.ast),
						Expression::new_boolean_literal(SPAN, *mode == OperationMode::Checked, &self.ast),
					],
				)
			}
			// Struct construction → `new <class>({ field: value, … })`.
			HirExpr::New {
				class,
				fields,
				prototype,
			}
			| HirExpr::StructFresh {
				class,
				fields,
				prototype,
			} => {
				if class == "NymphRange" {
					self
						.box_runtime_bindings
						.borrow_mut()
						.insert(class.to_string());
				}
				let mut props = ArenaVec::new_in(&self.ast);
				for (name, value) in fields {
					let key =
						PropertyKey::new_static_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast);
					let val = self.emit_expr(value);
					props.push(ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
						SPAN,
						PropertyKind::Init,
						key,
						val,
						false,
						false,
						false,
						&self.ast,
					)));
				}
				let obj = Expression::new_object_expression(SPAN, props, &self.ast);
				let callee =
					Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(class), &self.ast);
				let mut args = ArenaVec::new_in(&self.ast);
				args.push(Argument::from(obj));
				let value = Expression::new_new_expression(SPAN, callee, oxc::ast::NONE, args, &self.ast);
				if let Some(prototype) = prototype {
					self.set_prototype(value, self.emit_expr(prototype))
				} else {
					value
				}
			}
			HirExpr::StructCloneUpdate {
				class,
				source,
				replacements,
				prototype,
			} => {
				let mut props = ArenaVec::new_in(&self.ast);
				props.push(ObjectPropertyKind::new_spread_property(
					SPAN,
					self.emit_expr(source),
					&self.ast,
				));
				for (name, value) in replacements {
					props.push(ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
						SPAN,
						PropertyKind::Init,
						PropertyKey::new_static_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast),
						self.emit_expr(value),
						false,
						false,
						false,
						&self.ast,
					)));
				}
				let object = Expression::new_object_expression(SPAN, props, &self.ast);
				let callee =
					Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(class), &self.ast);
				let mut args = ArenaVec::new_in(&self.ast);
				args.push(Argument::from(object));
				let value = Expression::new_new_expression(SPAN, callee, oxc::ast::NONE, args, &self.ast);
				if let Some(prototype) = prototype {
					self.set_prototype(value, self.emit_expr(prototype))
				} else {
					value
				}
			}
			// Field access → `recv.name`.
			HirExpr::Field { recv, name } => {
				let object = self.emit_expr(recv);
				Expression::new_static_member_expression(
					SPAN,
					object,
					IdentifierName::new(SPAN, self.ast.allocator.alloc_str(name), &self.ast),
					false,
					&self.ast,
				)
			}
			// Variant construction → `<enum>.<variant>({ field: value, … })`.
			HirExpr::VariantNew {
				enum_name,
				variant,
				fields,
				prototype,
			} => {
				let mut props = ArenaVec::new_in(&self.ast);
				for (name, value) in fields {
					let key =
						PropertyKey::new_static_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast);
					let val = self.emit_expr(value);
					props.push(ObjectPropertyKind::ObjectProperty(ObjectProperty::boxed(
						SPAN,
						PropertyKind::Init,
						key,
						val,
						false,
						false,
						false,
						&self.ast,
					)));
				}
				let obj = Expression::new_object_expression(SPAN, props, &self.ast);
				let callee = self.variant_member(enum_name, variant);
				let value = self.call1(callee, obj);
				if let Some(prototype) = prototype {
					self.set_prototype(value, self.emit_expr(prototype))
				} else {
					value
				}
			}
			// Nullary variant reference → `<enum>.<variant>` (the frozen singleton).
			HirExpr::VariantRef {
				enum_name,
				variant,
				prototype,
			} => {
				let value = self.variant_member(enum_name, variant);
				if let Some(prototype) = prototype {
					self
						.box_runtime_bindings
						.borrow_mut()
						.insert("nymphVariant".to_string());
					self.member_call(
						Expression::new_identifier(SPAN, "nymphVariant", &self.ast),
						"call",
						vec![
							Expression::new_null_literal(SPAN, &self.ast),
							self.emit_expr(prototype),
							value,
						],
					)
				} else {
					value
				}
			}
			// A map lookup → `recv.get(key)`.
			HirExpr::MapGet { recv, key } => {
				let object = self.emit_expr(recv);
				let member = Expression::new_static_member_expression(
					SPAN,
					object,
					IdentifierName::new(SPAN, "get", &self.ast),
					false,
					&self.ast,
				);
				let mut args = ArenaVec::new_in(&self.ast);
				args.push(Argument::from(self.emit_expr(key)));
				Expression::new_call_expression(SPAN, member, oxc::ast::NONE, args, false, &self.ast)
			}
			HirExpr::Break { target, value } => {
				let token = self
					.loop_completion_tokens
					.borrow()
					.iter()
					.rev()
					.find_map(|(found, token)| (*found == *target).then_some(*token))
					.expect("break target is not active while emitting its loop body");
				let thrown = self.completion_transfer(
					token,
					Some(Expression::new_numeric_literal(
						SPAN,
						0.0,
						None,
						NumberBase::Decimal,
						&self.ast,
					)),
					Some(self.emit_expr(value)),
				);
				JsValue {
					stmts: ArenaVec::from_value_in(thrown, &self.ast),
					expr: Expression::new_identifier(SPAN, "undefined", &self.ast),
				}
				.into_expression(self.ast)
			}
			HirExpr::Continue { target } => {
				let token = self
					.loop_completion_tokens
					.borrow()
					.iter()
					.rev()
					.find_map(|(found, token)| (*found == *target).then_some(*token))
					.expect("continue target is not active while emitting its loop body");
				let thrown = self.completion_transfer(
					token,
					Some(Expression::new_numeric_literal(
						SPAN,
						1.0,
						None,
						NumberBase::Decimal,
						&self.ast,
					)),
					None,
				);
				JsValue {
					stmts: ArenaVec::from_value_in(thrown, &self.ast),
					expr: Expression::new_identifier(SPAN, "undefined", &self.ast),
				}
				.into_expression(self.ast)
			}
			// Control-flow expressions in value position collapse to an expression
			// (an IIFE when they carry leading statements). Mark that we're inside
			// that IIFE's body while building it — a `return` reached anywhere
			// underneath (e.g. a braced match-arm body used as a subexpression)
			// would target this IIFE, not the enclosing function, so
			// `emit_stmt`'s `HirStmt::Return` arm asserts against this flag.
			// Save/restore rather than a bare `set(true)`: this same fallthrough
			// arm can recurse (a match arm's own body can itself be a
			// subexpression-position `if`), and a bare set would leave the flag
			// stuck true once the outer call returns to a statement-position
			// caller (Slice 4E, Y1).
			HirExpr::Block { .. }
			| HirExpr::If { .. }
			| HirExpr::StateLoop { .. }
			| HirExpr::ContinueTransition { .. }
			| HirExpr::For { .. }
			| HirExpr::Match { .. } => {
				let prev = self.in_iife_subexpr.replace(true);
				let result = self.emit_value(expr).into_expression(self.ast);
				self.in_iife_subexpr.set(prev);
				result
			}
			// Dedicated scalar-cast nodes keep generated runtime calls from being
			// shadowed by user bindings.
			HirExpr::ScalarCast {
				kind,
				operand,
				mode,
			} => {
				let operand = self.unwrap_v(self.emit_expr(operand));
				match kind {
					ScalarCastKind::IdentityInt | ScalarCastKind::ToInt if *mode == OperationMode::Direct => {
						self.direct_integer_box("NInt", operand)
					}
					ScalarCastKind::IdentityInt | ScalarCastKind::ToInt => self.new_box("NInt", operand),
					ScalarCastKind::IdentityUInt | ScalarCastKind::IntToUInt
						if *mode == OperationMode::Direct =>
					{
						self.direct_integer_box("NUint", operand)
					}
					ScalarCastKind::IdentityUInt | ScalarCastKind::IntToUInt => {
						self.new_box("NUint", operand)
					}
					ScalarCastKind::IdentityFloat => self.new_box("NFloat", operand),
					ScalarCastKind::ToFloat => self.new_box(
						"NFloat",
						self.runtime_call("nymphIntegerToFloat", vec![operand]),
					),
					ScalarCastKind::IdentityChar => self.new_box("NChar", operand),
					ScalarCastKind::CheckedToInt => {
						let raw = self.runtime_call(
							"nymphFloatToInteger",
							vec![
								operand,
								Expression::new_boolean_literal(SPAN, false, &self.ast),
							],
						);
						self.new_box("NInt", raw)
					}
					ScalarCastKind::CheckedToUInt => {
						let raw = self.runtime_call(
							"nymphFloatToInteger",
							vec![
								operand,
								Expression::new_boolean_literal(SPAN, true, &self.ast),
							],
						);
						self.new_box("NUint", raw)
					}
					ScalarCastKind::CharToInt | ScalarCastKind::CharToUInt | ScalarCastKind::CharToFloat => {
						let zero =
							Expression::new_numeric_literal(SPAN, 0.0, None, NumberBase::Decimal, &self.ast);
						let raw = self.member_call(operand, "codePointAt", vec![zero]);
						let class = match kind {
							ScalarCastKind::CharToInt => "NInt",
							ScalarCastKind::CharToUInt => "NUint",
							ScalarCastKind::CharToFloat => "NFloat",
							_ => unreachable!(),
						};
						self.new_box(class, raw)
					}
					ScalarCastKind::NumToChar => {
						let number = self.runtime_call("nymphCharCode", vec![operand]);
						let raw = self.numeric_to_char(number, false);
						self.new_box("NChar", raw)
					}
					ScalarCastKind::FloatToChar => {
						let raw = self.numeric_to_char(operand, true);
						self.new_box("NChar", raw)
					}
				}
			}
			HirExpr::Closure { params, body } => self.activation_callable(params, body, true),
			HirExpr::LabeledBlock { target, body } => {
				let token = self.begin_return_completion();
				self
					.block_completion_tokens
					.borrow_mut()
					.push((*target, token));
				let previous_iife = self.in_iife_subexpr.replace(true);
				let value = self.emit_value(body);
				self.in_iife_subexpr.set(previous_iife);
				self.block_completion_tokens.borrow_mut().pop();
				let mut stmts = value.stmts;
				stmts.push(Statement::new_return_statement(
					SPAN,
					Some(value.expr),
					&self.ast,
				));
				JsValue {
					stmts: self.finish_return_completion(token, stmts),
					expr: Expression::new_identifier(SPAN, "undefined", &self.ast),
				}
				.into_expression(self.ast)
			}
		}
	}

	fn emit_activation_call(
		&self,
		callee: &HirExpr,
		args: &[HirExpr],
		mode: nymph_hir::hir::HirCallMode,
		source: u32,
	) -> Expression<'a> {
		let args = args
			.iter()
			.map(|argument| self.emit_expr(argument))
			.collect();
		if let HirExpr::Field { recv, name } = callee {
			return self.emit_member_activation(self.emit_expr(recv), name, args, mode, source);
		}
		self.emit_target_activation(self.emit_expr(callee), args, mode, source)
	}

	fn emit_activation_args(&self, args: Vec<Expression<'a>>) -> Expression<'a> {
		let mut elements = ArenaVec::new_in(&self.ast);
		for argument in args {
			elements.push(ArrayExpressionElement::from(argument));
		}
		let args = Expression::new_array_expression(SPAN, elements, &self.ast);
		args
	}

	fn zero_argument_arrow(&self, value: Expression<'a>) -> Expression<'a> {
		let body = FunctionBody::new(
			SPAN,
			ArenaVec::new_in(&self.ast),
			ArenaVec::from_value_in(
				Statement::new_return_statement(SPAN, Some(value), &self.ast),
				&self.ast,
			),
			&self.ast,
		);
		let params = FormalParameters::new(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			ArenaVec::new_in(&self.ast),
			oxc::ast::NONE,
			&self.ast,
		);
		Expression::new_arrow_function_expression(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			body,
			&self.ast,
		)
	}

	fn emit_task_operation(
		&self,
		operation: nymph_hir::hir::HirTaskOperation,
		operands: &[HirExpr],
	) -> Expression<'a> {
		use nymph_hir::hir::HirTaskOperation;

		let values = operands
			.iter()
			.map(|operand| self.emit_expr(operand))
			.collect::<Vec<_>>();
		match operation {
			HirTaskOperation::Drive => {
				assert_eq!(values.len(), 1);
				self.runtime_call("nymphTaskDrive", values)
			}
			HirTaskOperation::Spawn => {
				assert_eq!(values.len(), 1);
				self.runtime_call("nymphTaskSpawn", values)
			}
			HirTaskOperation::Observe => {
				assert_eq!(values.len(), 1);
				self.runtime_call("nymphHandleObserve", values)
			}
			HirTaskOperation::Cancel => {
				assert_eq!(values.len(), 1);
				self.runtime_call("nymphHandleCancel", values)
			}
			HirTaskOperation::Checkpoint => {
				assert!(values.is_empty());
				self.runtime_call("nymphCheckpoint", values)
			}
			HirTaskOperation::Select | HirTaskOperation::Race => {
				let values = self.emit_activation_args(values);
				self.runtime_call(
					if operation == HirTaskOperation::Select {
						"nymphTaskSelect"
					} else {
						"nymphTaskRace"
					},
					vec![values],
				)
			}
		}
	}

	fn emit_activation_source(&self, source: u32) -> Expression<'a> {
		let source = Expression::new_numeric_literal(
			SPAN,
			f64::from(source),
			None,
			NumberBase::Decimal,
			&self.ast,
		);
		source
	}

	fn emit_member_activation(
		&self,
		receiver: Expression<'a>,
		member: &str,
		args: Vec<Expression<'a>>,
		mode: nymph_hir::hir::HirCallMode,
		source: u32,
	) -> Expression<'a> {
		let tail = mode == nymph_hir::hir::HirCallMode::Tail;
		if tail {
			return self.runtime_call(
				"nymphTailCallMember",
				vec![
					receiver,
					Expression::new_string_literal(
						SPAN,
						self.ast.allocator.alloc_str(member),
						None,
						&self.ast,
					),
					self.emit_activation_args(args),
					self.emit_activation_source(source),
				],
			);
		}
		let receiver_name = self.gensym();
		let receiver_name = self.ast.allocator.alloc_str(&receiver_name);
		let callee = Expression::new_static_member_expression(
			SPAN,
			self.ident(receiver_name),
			IdentifierName::new(SPAN, self.ast.allocator.alloc_str(member), &self.ast),
			false,
			&self.ast,
		);
		let activation = self.runtime_call(
			"nymphActivate",
			vec![
				callee,
				self.ident(receiver_name),
				self.emit_activation_args(args),
				self.emit_activation_source(source),
			],
		);
		self.arrow_iife(receiver_name, activation, receiver)
	}

	fn emit_target_activation(
		&self,
		callee: Expression<'a>,
		args: Vec<Expression<'a>>,
		mode: nymph_hir::hir::HirCallMode,
		source: u32,
	) -> Expression<'a> {
		let tail = mode == nymph_hir::hir::HirCallMode::Tail;
		self.runtime_call(
			if tail {
				"nymphTailCall"
			} else {
				"nymphActivate"
			},
			vec![
				callee,
				Expression::new_identifier(SPAN, "undefined", &self.ast),
				self.emit_activation_args(args),
				self.emit_activation_source(source),
			],
		)
	}

	/// A simple-identifier assignment target for `<name> = …`.
	fn assign_target(&self, name: &'a str) -> AssignmentTarget<'a> {
		AssignmentTarget::AssignmentTargetIdentifier(IdentifierReference::boxed(SPAN, name, &self.ast))
	}

	/// `let <name>;` — an uninitialised binding for a control-flow result temporary.
	fn let_uninit(&self, name: &'a str) -> Statement<'a> {
		let pat = BindingPattern::new_binding_identifier(SPAN, name, &self.ast);
		let declarator = VariableDeclarator::new(
			SPAN,
			VariableDeclarationKind::Let,
			pat,
			oxc::ast::NONE,
			None,
			false,
			&self.ast,
		);
		let decl = VariableDeclaration::new(
			SPAN,
			VariableDeclarationKind::Let,
			ArenaVec::from_value_in(declarator, &self.ast),
			false,
			&self.ast,
		);
		Statement::from(Declaration::VariableDeclaration(ArenaBox::new_in(
			decl, &self.ast,
		)))
	}

	fn completion_slot(&self, token: &'a str, index: f64) -> AssignmentTarget<'a> {
		AssignmentTarget::from(MemberExpression::ComputedMemberExpression(
			ComputedMemberExpression::boxed(
				SPAN,
				Expression::new_identifier(SPAN, token, &self.ast),
				Expression::new_numeric_literal(SPAN, index, None, NumberBase::Decimal, &self.ast),
				false,
				&self.ast,
			),
		))
	}

	fn completion_transfer(
		&self,
		token: &'a str,
		kind: Option<Expression<'a>>,
		value: Option<Expression<'a>>,
	) -> Statement<'a> {
		let mut statements = ArenaVec::new_in(&self.ast);
		for (index, expression) in [(1.0, kind), (2.0, value)] {
			if let Some(expression) = expression {
				statements.push(Statement::new_expression_statement(
					SPAN,
					Expression::new_assignment_expression(
						SPAN,
						AssignmentOperator::Assign,
						self.completion_slot(token, index),
						expression,
						&self.ast,
					),
					&self.ast,
				));
			}
		}
		statements.push(Statement::new_throw_statement(
			SPAN,
			Expression::new_identifier(SPAN, token, &self.ast),
			&self.ast,
		));
		Statement::new_block_statement(SPAN, statements, &self.ast)
	}

	fn begin_return_completion(&self) -> &'a str {
		let token = self.ast.allocator.alloc_str(&self.completion_name());
		self
			.return_completion_tokens
			.borrow_mut()
			.push((token, false));
		token
	}

	/// Wrap a callable body only when one of its returns must cross a generated
	/// expression IIFE. The private packet is recognized by identity before any
	/// fields are read, so arbitrary user exceptions are rethrown unchanged.
	fn finish_return_completion(
		&self,
		token: &'a str,
		body: ArenaVec<'a, Statement<'a>>,
	) -> ArenaVec<'a, Statement<'a>> {
		let (found, used) = self
			.return_completion_tokens
			.borrow_mut()
			.pop()
			.expect("callable return-completion scope is active");
		assert_eq!(found, token);
		if !used {
			return body;
		}

		let completion = self.ast.allocator.alloc_str(&self.completion_name());
		let completion_at = |index: f64| {
			Expression::from(MemberExpression::ComputedMemberExpression(
				ComputedMemberExpression::boxed(
					SPAN,
					Expression::new_identifier(SPAN, completion, &self.ast),
					Expression::new_numeric_literal(SPAN, index, None, NumberBase::Decimal, &self.ast),
					false,
					&self.ast,
				),
			))
		};
		let wrong_token = Expression::new_binary_expression(
			SPAN,
			Expression::new_identifier(SPAN, completion, &self.ast),
			BinaryOperator::StrictInequality,
			Expression::new_identifier(SPAN, token, &self.ast),
			&self.ast,
		);
		let mut catch_stmts = ArenaVec::new_in(&self.ast);
		catch_stmts.push(Statement::new_if_statement(
			SPAN,
			wrong_token,
			Statement::new_throw_statement(
				SPAN,
				Expression::new_identifier(SPAN, completion, &self.ast),
				&self.ast,
			),
			None,
			&self.ast,
		));
		catch_stmts.push(Statement::new_return_statement(
			SPAN,
			Some(completion_at(2.0)),
			&self.ast,
		));
		let handler = CatchClause::boxed(
			SPAN,
			Some(CatchParameter::new(
				SPAN,
				BindingPattern::new_binding_identifier(SPAN, completion, &self.ast),
				oxc::ast::NONE,
				&self.ast,
			)),
			BlockStatement::new(SPAN, catch_stmts, &self.ast),
			&self.ast,
		);
		let try_stmt = Statement::new_try_statement(
			SPAN,
			BlockStatement::new(SPAN, body, &self.ast),
			Some(handler),
			None::<ArenaBox<'a, BlockStatement<'a>>>,
			&self.ast,
		);
		let mut wrapped = ArenaVec::new_in(&self.ast);
		wrapped.push(self.const_decl(
			token,
			Expression::new_array_expression(SPAN, ArenaVec::new_in(&self.ast), &self.ast),
		));
		wrapped.push(try_stmt);
		wrapped
	}

	/// `{ <branch stmts>; <name> = <branch value>; }` — a block that assigns an
	/// (optional) branch's value to `name` (or `undefined` when the branch is absent).
	fn assign_block(&self, name: &'a str, branch: Option<&HirExpr>) -> Statement<'a> {
		let val = match branch {
			Some(b) => self.emit_value(b),
			None => JsValue {
				stmts: ArenaVec::new_in(&self.ast),
				expr: Expression::new_identifier(SPAN, "undefined", &self.ast),
			},
		};
		let mut stmts = val.stmts;
		let assign = Expression::new_assignment_expression(
			SPAN,
			AssignmentOperator::Assign,
			self.assign_target(name),
			val.expr,
			&self.ast,
		);
		stmts.push(Statement::new_expression_statement(SPAN, assign, &self.ast));
		Statement::new_block_statement(SPAN, stmts, &self.ast)
	}

	/// A HIR expression emitted as a JS block statement, evaluating its value for
	/// effect (used for a `while` body, whose value is discarded).
	fn block_stmt(&self, expr: &HirExpr) -> Statement<'a> {
		let val = self.emit_value(expr);
		let mut stmts = val.stmts;
		stmts.push(Statement::new_expression_statement(
			SPAN, val.expr, &self.ast,
		));
		Statement::new_block_statement(SPAN, stmts, &self.ast)
	}

	/// Emit a single HIR statement as a JS statement.
	fn emit_stmt(&self, stmt: &HirStmt) -> Statement<'a> {
		match stmt {
			HirStmt::Let { name, value, .. } => self.binding_declaration(name, self.emit_expr(value)),
			// A statement-position control-flow expression flattens directly into a
			// plain JS `BlockStatement` via `block_stmt` (matching how a `while` body
			// already does), rather than going through `emit_expr`'s subexpression
			// fallthrough — which would otherwise wrap it in a needless IIFE, and
			// (post Slice 4E, Y1) trip the `return`-inside-IIFE guard for a
			// statement-position `if`/`while`/`match` that legitimately contains a
			// `return`. The `BlockStatement` still gives it its own JS scope
			// (unaffected by Y2 shadowing) and keeps any gensym `let $nymph$temp$N` temps
			// scoped to it, same as before.
			HirStmt::Expr(
				e @ (HirExpr::Block { .. }
				| HirExpr::If { .. }
				| HirExpr::For { .. }
				| HirExpr::Match { .. }),
			) => self.block_stmt(e),
			HirStmt::Expr(e) => {
				let expr = self.emit_expr(e);
				Statement::new_expression_statement(SPAN, expr, &self.ast)
			}
			// A return under a generated expression IIFE uses the callable's private
			// completion token so it can cross that synthetic function boundary.
			// Ordinary statement-position returns remain direct JS returns.
			HirStmt::Return { value, target } => {
				let block_token = match target {
					nymph_hir::hir::HirReturnTarget::Callable => None,
					nymph_hir::hir::HirReturnTarget::Block(target) => self
						.block_completion_tokens
						.borrow()
						.iter()
						.rev()
						.find_map(|(candidate, token)| (candidate == target).then_some(*token)),
				};
				if block_token.is_some() || self.in_iife_subexpr.get() {
					let token = {
						let mut tokens = self.return_completion_tokens.borrow_mut();
						let (token, used) = if let Some(block_token) = block_token {
							tokens
								.iter_mut()
								.rev()
								.find(|(token, _)| *token == block_token)
						} else {
							let block_tokens = self.block_completion_tokens.borrow();
							tokens.iter_mut().rev().find(|(token, _)| {
								!block_tokens
									.iter()
									.any(|(_, block_token)| block_token == token)
							})
						}
						.expect("completion return has an active target");
						*used = true;
						*token
					};
					self.completion_transfer(
						token,
						None,
						Some(value.as_ref().map_or_else(
							|| Expression::new_identifier(SPAN, "undefined", &self.ast),
							|value| self.emit_expr(value),
						)),
					)
				} else {
					let value_expr = value.as_ref().map(|value| self.emit_expr(value));
					Statement::new_return_statement(SPAN, value_expr, &self.ast)
				}
			}
		}
	}

	/// Emit an expression as a `JsValue`: leading statements plus a final expression.
	///
	/// For `Block { stmts, tail }`, each statement is emitted in order and the tail
	/// (or `undefined` if absent) becomes the final expression. Any other expression
	/// has no leading statements.
	fn emit_value(&self, expr: &HirExpr) -> JsValue<'a> {
		match expr {
			HirExpr::Block { stmts, tail } => {
				let mut js_stmts = ArenaVec::new_in(&self.ast);
				for stmt in stmts {
					js_stmts.push(self.emit_stmt(stmt));
				}
				let tail_expr = if let Some(tail) = tail {
					let tail = self.emit_value(tail);
					js_stmts.extend(tail.stmts);
					tail.expr
				} else {
					Expression::new_identifier(SPAN, "undefined", &self.ast)
				};
				JsValue {
					stmts: js_stmts,
					expr: tail_expr,
				}
			}
			HirExpr::If {
				cond,
				then,
				otherwise,
			} => {
				// let <tmp>; if (cond) { <tmp> = then } else { <tmp> = else }; → <tmp>
				let tmp = self.ast.allocator.alloc_str(&self.gensym());
				let decl = self.let_uninit(tmp);
				let cond_expr = self.emit_cond(cond);
				let then_stmt = self.assign_block(tmp, Some(then));
				let else_stmt = self.assign_block(tmp, otherwise.as_deref());
				let if_stmt =
					Statement::new_if_statement(SPAN, cond_expr, then_stmt, Some(else_stmt), &self.ast);
				let mut stmts = ArenaVec::new_in(&self.ast);
				stmts.push(decl);
				stmts.push(if_stmt);
				JsValue {
					stmts,
					expr: Expression::new_identifier(SPAN, tmp, &self.ast),
				}
			}
			HirExpr::For { .. } => unreachable!("for expressions are emitted through activation plans"),
			HirExpr::Match { scrutinee, arms } => {
				// const <s> = <scrutinee>; let <r>;
				// <m>: {
				//   if (<test0>) { <binds0>; if (<guard0>) { <r> = <body0>; break <m>; } }
				//   …
				//   { <bindsLast>; <r> = <bodyLast>; }   // last arm: unguarded ⇒ testless tail
				// }  → <r>
				// A labeled block (not an if/else-if chain) is required so a matched-but-
				// guard-failed arm falls through to the next arm. `s`/`r`/`m` are gensym
				// temps (`_tN`), not literally `_s`/`_r`/`_m`.
				let s = self.ast.allocator.alloc_str(&self.gensym());
				let r = self.ast.allocator.alloc_str(&self.gensym());
				let label = self.ast.allocator.alloc_str(&self.gensym());
				let mut stmts = ArenaVec::new_in(&self.ast);
				let scrutinee_expr = self.emit_expr(scrutinee);
				stmts.push(self.const_decl(s, scrutinee_expr));
				stmts.push(self.let_uninit(r));
				let subj = Subject::Temp(s.to_string());

				let mut body = ArenaVec::new_in(&self.ast);
				for (i, arm) in arms.iter().enumerate() {
					let is_last = i + 1 == arms.len();
					let (test, binds, decisions) = self.compile_pat(&arm.pat, &subj);
					// The pattern `test` is a compiler-INTERNAL raw JS boolean built by
					// `compile_pat` and stays raw. The `guard`, by contrast, is the lone
					// user-`boolean` slot inside `match`, so it reads `.v` like any other
					// user condition (uniform value boxing, ADR-0002).
					let guard = arm.guard.as_ref().map(|g| self.emit_cond(g));
					// An unguarded last arm is the guaranteed fallback (exhaustiveness) → no
					// test, no break. Any other arm commits then breaks, guarded by its test.
					if is_last && arm.guard.is_none() {
						let selection = (!decisions.is_empty()).then_some(test).flatten();
						body.push(self.match_arm(
							r,
							&binds,
							&decisions,
							&arm.body,
							MatchArmControl {
								guard: None,
								test: None,
								selection,
								label: None,
							},
						));
					} else {
						body.push(self.match_arm(
							r,
							&binds,
							&decisions,
							&arm.body,
							MatchArmControl {
								guard,
								test,
								selection: None,
								label: Some(label),
							},
						));
					}
				}
				let block = Statement::new_block_statement(SPAN, body, &self.ast);
				stmts.push(Statement::new_labeled_statement(
					SPAN,
					LabelIdentifier::new(SPAN, label, &self.ast),
					block,
					&self.ast,
				));
				JsValue {
					stmts,
					expr: Expression::new_identifier(SPAN, r, &self.ast),
				}
			}
			other => JsValue {
				stmts: ArenaVec::new_in(&self.ast),
				expr: self.emit_expr(other),
			},
		}
	}

	/// Re-emit a subject reference (scrutinee temp or a field path) as a fresh
	/// expression. Needed because tests and each binding require their own copy.
	fn emit_subject(&self, s: &Subject) -> Expression<'a> {
		match s {
			Subject::Temp(name) => {
				Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast)
			}
			Subject::Field(base, field) => {
				let object = self.emit_subject(base);
				Expression::new_static_member_expression(
					SPAN,
					object,
					IdentifierName::new(SPAN, self.ast.allocator.alloc_str(field), &self.ast),
					false,
					&self.ast,
				)
			}
			Subject::Index(base, index) => {
				let object = Expression::new_static_member_expression(
					SPAN,
					self.emit_subject(base),
					IdentifierName::new(SPAN, "v", &self.ast),
					false,
					&self.ast,
				);
				let idx = Expression::new_numeric_literal(
					SPAN,
					*index as f64,
					None,
					NumberBase::Decimal,
					&self.ast,
				);
				Expression::ComputedMemberExpression(ComputedMemberExpression::boxed(
					SPAN, object, idx, false, &self.ast,
				))
			}
			Subject::IndexFromEnd(base, offset) => {
				// <base>[<base>.length - <offset>]
				let arr = Expression::new_static_member_expression(
					SPAN,
					self.emit_subject(base),
					IdentifierName::new(SPAN, "v", &self.ast),
					false,
					&self.ast,
				);
				let len = Expression::new_static_member_expression(
					SPAN,
					Expression::new_static_member_expression(
						SPAN,
						self.emit_subject(base),
						IdentifierName::new(SPAN, "v", &self.ast),
						false,
						&self.ast,
					),
					IdentifierName::new(SPAN, "length", &self.ast),
					false,
					&self.ast,
				);
				let off = Expression::new_numeric_literal(
					SPAN,
					*offset as f64,
					None,
					NumberBase::Decimal,
					&self.ast,
				);
				let index =
					Expression::new_binary_expression(SPAN, len, BinaryOperator::Subtraction, off, &self.ast);
				Expression::ComputedMemberExpression(ComputedMemberExpression::boxed(
					SPAN, arr, index, false, &self.ast,
				))
			}
			Subject::MapGet(base, key) => {
				let map = self.emit_subject(base);
				self.member_call(map, "get", vec![self.emit_boxed_lit(key)])
			}
			Subject::Slice(base, start, end_from_end, kind) => {
				let arr = Expression::new_static_member_expression(
					SPAN,
					self.emit_subject(base),
					IdentifierName::new(SPAN, "v", &self.ast),
					false,
					&self.ast,
				);
				let start_lit = Expression::new_numeric_literal(
					SPAN,
					*start as f64,
					None,
					NumberBase::Decimal,
					&self.ast,
				);
				let slice = if *end_from_end == 0 {
					self.member_call(arr, "slice", vec![start_lit])
				} else {
					let len = Expression::new_static_member_expression(
						SPAN,
						Expression::new_static_member_expression(
							SPAN,
							self.emit_subject(base),
							IdentifierName::new(SPAN, "v", &self.ast),
							false,
							&self.ast,
						),
						IdentifierName::new(SPAN, "length", &self.ast),
						false,
						&self.ast,
					);
					let end_lit = Expression::new_numeric_literal(
						SPAN,
						*end_from_end as f64,
						None,
						NumberBase::Decimal,
						&self.ast,
					);
					let end = Expression::new_binary_expression(
						SPAN,
						len,
						BinaryOperator::Subtraction,
						end_lit,
						&self.ast,
					);
					self.member_call(arr, "slice", vec![start_lit, end])
				};
				self.new_box(
					match kind {
						HirArrayKind::Tuple => "NTuple",
						HirArrayKind::List => "NList",
						HirArrayKind::Raw => unreachable!("patterns never bind a raw-array rest"),
					},
					slice,
				)
			}
			Subject::MapRest(base, keys) => {
				let map_expr = self.emit_subject(base);
				keys.iter().fold(map_expr, |map, key| {
					self.member_call(map, "without", vec![self.emit_boxed_lit(key)])
				})
			}
			Subject::PatternSelect {
				decision,
				left,
				right,
			} => Expression::new_conditional_expression(
				SPAN,
				Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(decision), &self.ast),
				self.emit_subject(left),
				self.emit_subject(right),
				&self.ast,
			),
		}
	}

	/// A scalar pattern literal as a JS expression (for `=== <lit>` tests).
	fn emit_lit(&self, lit: &HirLit) -> Expression<'a> {
		match lit {
			HirLit::Int(v) => self.bigint_literal(v),
			HirLit::UInt(v) => self.bigint_literal(v),
			HirLit::Num(v, _) => {
				Expression::new_numeric_literal(SPAN, *v, None, NumberBase::Decimal, &self.ast)
			}
			HirLit::Bool(b) => Expression::new_boolean_literal(SPAN, *b, &self.ast),
			HirLit::Char(c) => {
				let s = self.ast.allocator.alloc_str(&c.to_string());
				Expression::new_string_literal(SPAN, s, None, &self.ast)
			}
			HirLit::Str(s) => {
				let s = self.ast.allocator.alloc_str(s);
				Expression::new_string_literal(SPAN, s, None, &self.ast)
			}
		}
	}

	fn emit_boxed_lit(&self, lit: &HirLit) -> Expression<'a> {
		let value = self.emit_lit(lit);
		let class = match lit {
			HirLit::Int(_) => "NInt",
			HirLit::UInt(_) => "NUint",
			HirLit::Num(_, kind) => box_rt::num_box_class(*kind),
			HirLit::Bool(_) => "NBool",
			HirLit::Char(_) => "NChar",
			HirLit::Str(_) => "NString",
		};
		self.new_box(class, value)
	}

	/// `<obj>[TAG]` (optional-chained when `optional`), reading the variant tag.
	fn tag_read(&self, obj: Expression<'a>, optional: bool) -> Expression<'a> {
		Expression::ComputedMemberExpression(ComputedMemberExpression::boxed(
			SPAN,
			obj,
			self.global_symbol("nymph.tag"),
			optional,
			&self.ast,
		))
	}

	fn global_symbol(&self, name: &str) -> Expression<'a> {
		let symbol = Expression::new_identifier(SPAN, "Symbol", &self.ast);
		let member = Expression::new_static_member_expression(
			SPAN,
			symbol,
			IdentifierName::new(SPAN, "for", &self.ast),
			false,
			&self.ast,
		);
		let value =
			Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(name), None, &self.ast);
		Expression::new_call_expression(
			SPAN,
			member,
			oxc::ast::NONE,
			ArenaVec::from_value_in(Argument::from(value), &self.ast),
			false,
			&self.ast,
		)
	}

	/// Compile a pattern against a subject into a boolean test (`None` ⇒ always true)
	/// and a sequence of `(name, subject)` bindings.
	fn compile_pat(
		&self,
		pat: &HirPat,
		subj: &Subject,
	) -> (Option<Expression<'a>>, Vec<(String, Subject)>, Vec<String>) {
		match pat {
			HirPat::Wildcard => (None, Vec::new(), Vec::new()),
			HirPat::Binding { name, sub } => {
				let mut binds = vec![(name.to_string(), subj.clone())];
				let (test, decisions) = match sub {
					None => (None, Vec::new()),
					Some(sub) => {
						let (t, mut b, decisions) = self.compile_pat(sub, subj);
						binds.append(&mut b);
						(t, decisions)
					}
				};
				(test, binds, decisions)
			}
			HirPat::Lit(lit) => {
				let subject = self.unwrap_v(self.emit_subject(subj));
				let value = self.emit_lit(lit);
				let test = Expression::new_binary_expression(
					SPAN,
					subject,
					BinaryOperator::StrictEquality,
					value,
					&self.ast,
				);
				(Some(test), Vec::new(), Vec::new())
			}
			HirPat::Variant {
				enum_name,
				variant,
				fields,
			} => {
				// <subject>?.[TAG] === <enum>.<variant>[TAG]
				let subject_tag = self.tag_read(self.emit_subject(subj), true);
				let variant_tag = self.global_symbol(&format!("{enum_name}.{variant}"));
				let mut test = Expression::new_binary_expression(
					SPAN,
					subject_tag,
					BinaryOperator::StrictEquality,
					variant_tag,
					&self.ast,
				);
				let mut binds = Vec::new();
				let mut decisions = Vec::new();
				for (field, sub) in fields {
					let field_subj = Subject::Field(Box::new(subj.clone()), field.to_string());
					let (t, mut b, mut d) = self.compile_pat(sub, &field_subj);
					binds.append(&mut b);
					decisions.append(&mut d);
					if let Some(t) = t {
						test =
							Expression::new_logical_expression(SPAN, test, LogicalOperator::And, t, &self.ast);
					}
				}
				(Some(test), binds, decisions)
			}
			// A struct pattern is irrefutable at its own level (nominal type guarantees
			// the shape); a field sub-pattern may still contribute a test.
			HirPat::Struct { fields } => {
				let mut test: Option<Expression<'a>> = None;
				let mut binds = Vec::new();
				let mut decisions = Vec::new();
				for (field, sub) in fields {
					let field_subj = Subject::Field(Box::new(subj.clone()), field.to_string());
					let (t, mut b, mut d) = self.compile_pat(sub, &field_subj);
					binds.append(&mut b);
					decisions.append(&mut d);
					test = self.and_test(test, t);
				}
				(test, binds, decisions)
			}
			// A tuple pattern binds by index; irrefutable at its own level.
			HirPat::Tuple(elems) => {
				let mut test: Option<Expression<'a>> = None;
				let mut binds = Vec::new();
				let mut decisions = Vec::new();
				for (i, sub) in elems.iter().enumerate() {
					let elem_subj = Subject::Index(Box::new(subj.clone()), i);
					let (t, mut b, mut d) = self.compile_pat(sub, &elem_subj);
					binds.append(&mut b);
					decisions.append(&mut d);
					test = self.and_test(test, t);
				}
				(test, binds, decisions)
			}
			// A list pattern: a length test (exact or `>=`), element bindings by index
			// (prefix from the front, suffix from the end), and an optional rest slice.
			HirPat::List {
				kind,
				prefix,
				rest,
				suffix,
			} => {
				let min_len = prefix.len() + suffix.len();
				// A rest capturing everything (`#[...rest]`, no fixed elements) matches any
				// list, so it needs no length test. Otherwise: exact `===` (no rest) or
				// `>= min_len` (with rest).
				let mut test = if rest.is_some() && min_len == 0 {
					None
				} else {
					let length = Expression::new_static_member_expression(
						SPAN,
						Expression::new_static_member_expression(
							SPAN,
							self.emit_subject(subj),
							IdentifierName::new(SPAN, "v", &self.ast),
							false,
							&self.ast,
						),
						IdentifierName::new(SPAN, "length", &self.ast),
						false,
						&self.ast,
					);
					let n = Expression::new_numeric_literal(
						SPAN,
						min_len as f64,
						None,
						NumberBase::Decimal,
						&self.ast,
					);
					let length_op = if rest.is_none() {
						BinaryOperator::StrictEquality
					} else {
						BinaryOperator::GreaterEqualThan
					};
					Some(Expression::new_binary_expression(
						SPAN, length, length_op, n, &self.ast,
					))
				};
				let mut binds = Vec::new();
				let mut decisions = Vec::new();
				for (i, sub) in prefix.iter().enumerate() {
					let elem = Subject::Index(Box::new(subj.clone()), i);
					let (t, mut b, mut d) = self.compile_pat(sub, &elem);
					binds.append(&mut b);
					decisions.append(&mut d);
					test = self.and_test(test, t);
				}
				let suf_len = suffix.len();
				for (j, sub) in suffix.iter().enumerate() {
					// The j-th suffix element is `suf_len - j` from the end.
					let elem = Subject::IndexFromEnd(Box::new(subj.clone()), suf_len - j);
					let (t, mut b, mut d) = self.compile_pat(sub, &elem);
					binds.append(&mut b);
					decisions.append(&mut d);
					test = self.and_test(test, t);
				}
				if let Some(Some(name)) = rest {
					let slice = Subject::Slice(Box::new(subj.clone()), prefix.len(), suffix.len(), *kind);
					binds.push((name.to_string(), slice));
				}
				(test, binds, decisions)
			}
			// A map pattern: for each `key: vpat`, test `_s.has(key)` and match `vpat`
			// against `_s.get(key)`; an optional `...rest` binds the rest-of-map.
			HirPat::Map { entries, rest } => {
				let mut test: Option<Expression<'a>> = None;
				let mut binds = Vec::new();
				let mut decisions = Vec::new();
				for (key, vpat) in entries {
					let has = self.member_call(
						self.emit_subject(subj),
						"has",
						vec![self.emit_boxed_lit(key)],
					);
					test = self.and_test(test, Some(has));
					let val = Subject::MapGet(Box::new(subj.clone()), key.clone());
					let (t, mut b, mut d) = self.compile_pat(vpat, &val);
					binds.append(&mut b);
					decisions.append(&mut d);
					test = self.and_test(test, t);
				}
				if let Some(Some(name)) = rest {
					let keys = entries.iter().map(|(k, _)| k.clone()).collect();
					let rest_subj = Subject::MapRest(Box::new(subj.clone()), keys);
					binds.push((name.to_string(), rest_subj));
				}
				(test, binds, decisions)
			}
			// A range pattern: bound comparisons against the subject.
			HirPat::Range(range) => (
				Some(self.compile_range(range, subj)),
				Vec::new(),
				Vec::new(),
			),
			// A union matches if either side matches. Sema/lowering guarantee both
			// alternatives bind the same emitted names; select each value from the
			// side whose test matched.
			HirPat::Or(a, b) => {
				let (ta, ba, mut decisions) = self.compile_pat(a, subj);
				let (tb, bb, mut right_decisions) = self.compile_pat(b, subj);
				decisions.append(&mut right_decisions);
				let mut right_by_name: std::collections::HashMap<_, _> = bb.into_iter().collect();
				if ba.is_empty() {
					debug_assert!(right_by_name.is_empty());
					let test = match (ta, tb) {
						(Some(a), Some(b)) => Some(Expression::new_logical_expression(
							SPAN,
							a,
							LogicalOperator::Or,
							b,
							&self.ast,
						)),
						(None, _) => None,
						(Some(a), None) => Some(Expression::new_logical_expression(
							SPAN,
							a,
							LogicalOperator::Or,
							Expression::new_boolean_literal(SPAN, true, &self.ast),
							&self.ast,
						)),
					};
					return (test, Vec::new(), decisions);
				}

				let Some(left_test) = ta else {
					// The left side is irrefutable, so source-order short-circuiting means
					// the right extraction plan is unreachable.
					return (None, ba, decisions);
				};
				let decision = self.gensym();
				let decision_name = self.ast.allocator.alloc_str(&decision);
				let assigned_left = Expression::new_assignment_expression(
					SPAN,
					AssignmentOperator::Assign,
					self.assign_target(decision_name),
					left_test,
					&self.ast,
				);
				let right_test =
					tb.unwrap_or_else(|| Expression::new_boolean_literal(SPAN, true, &self.ast));
				let test = Some(Expression::new_logical_expression(
					SPAN,
					assigned_left,
					LogicalOperator::Or,
					right_test,
					&self.ast,
				));
				let binds = ba
					.into_iter()
					.map(|(name, left)| {
						let right = right_by_name
							.remove(&name)
							.expect("union alternatives must bind the same names");
						(
							name,
							Subject::PatternSelect {
								decision: decision.clone(),
								left: Box::new(left),
								right: Box::new(right),
							},
						)
					})
					.collect();
				debug_assert!(right_by_name.is_empty());
				decisions.push(decision);
				(test, binds, decisions)
			}
		}
	}

	/// Emit a range pattern's bound test against the subject.
	fn compile_range(&self, range: &HirRange, subj: &Subject) -> Expression<'a> {
		// `<lit> <= <subj>`
		let ge = |me: &Self, lit: &HirLit| {
			Expression::new_binary_expression(
				SPAN,
				me.emit_lit(lit),
				BinaryOperator::LessEqualThan,
				me.unwrap_v(me.emit_subject(subj)),
				&me.ast,
			)
		};
		// `<subj> <op> <lit>`
		let lt = |me: &Self, lit: &HirLit, op: BinaryOperator| {
			Expression::new_binary_expression(
				SPAN,
				me.unwrap_v(me.emit_subject(subj)),
				op,
				me.emit_lit(lit),
				&me.ast,
			)
		};
		match range {
			HirRange::From(min) => ge(self, min),
			HirRange::To(max) => lt(self, max, BinaryOperator::LessThan),
			HirRange::ToInclusive(max) => lt(self, max, BinaryOperator::LessEqualThan),
			HirRange::Exclusive { min, max } => {
				let lo = ge(self, min);
				let hi = lt(self, max, BinaryOperator::LessThan);
				Expression::new_logical_expression(SPAN, lo, LogicalOperator::And, hi, &self.ast)
			}
			HirRange::Inclusive { min, max } => {
				let lo = ge(self, min);
				let hi = lt(self, max, BinaryOperator::LessEqualThan);
				Expression::new_logical_expression(SPAN, lo, LogicalOperator::And, hi, &self.ast)
			}
		}
	}

	/// Combine an accumulated test with an optional new one via `&&` (either may be
	/// `None`, meaning "always true").
	fn and_test(
		&self,
		acc: Option<Expression<'a>>,
		next: Option<Expression<'a>>,
	) -> Option<Expression<'a>> {
		match (acc, next) {
			(None, t) | (t, None) => t,
			(Some(a), Some(b)) => Some(Expression::new_logical_expression(
				SPAN,
				a,
				LogicalOperator::And,
				b,
				&self.ast,
			)),
		}
	}

	/// Emit one match arm as a statement inside the labeled block:
	/// `if (<test>) { const <binds>; if (<guard>) { <result> = <body>; break <label>; } }`.
	/// `test`/`guard`/`label` are each optional: no `test` ⇒ the block runs
	/// unconditionally; no `guard` ⇒ the commit is unconditional; no `label` ⇒ the tail
	/// arm (no `break`). Bindings precede the guard so the guard can read them.
	fn match_arm(
		&self,
		result: &'a str,
		binds: &[(String, Subject)],
		decisions: &[String],
		body: &HirExpr,
		control: MatchArmControl<'a>,
	) -> Statement<'a> {
		// commit: `<result> = <body>;` then `break <label>;` (unless this is the tail arm).
		let mut commit = ArenaVec::new_in(&self.ast);
		let val = self.emit_value(body);
		commit.extend(val.stmts);
		let assign = Expression::new_assignment_expression(
			SPAN,
			AssignmentOperator::Assign,
			self.assign_target(result),
			val.expr,
			&self.ast,
		);
		commit.push(Statement::new_expression_statement(SPAN, assign, &self.ast));
		if let Some(label) = control.label {
			commit.push(Statement::new_break_statement(
				SPAN,
				Some(LabelIdentifier::new(SPAN, label, &self.ast)),
				&self.ast,
			));
		}
		let committed = match control.guard {
			Some(guard) => {
				let commit_block = Statement::new_block_statement(SPAN, commit, &self.ast);
				Statement::new_if_statement(SPAN, guard, commit_block, None, &self.ast)
			}
			None => Statement::new_block_statement(SPAN, commit, &self.ast),
		};
		// block: `{ const <binds>; <committed> }`
		let mut block = ArenaVec::new_in(&self.ast);
		for (name, subj) in binds {
			let init = self.emit_subject(subj);
			let activation_binding = self.activation_pattern_bindings.borrow().contains(name);
			if activation_binding {
				let location = self.activation_environment.borrow()[name].clone();
				block.push(self.activation_assignment(self.activation_slot_target(&location), init));
			} else {
				block.push(self.const_decl(name, init));
			}
		}
		block.push(committed);
		let block = Statement::new_block_statement(SPAN, block, &self.ast);
		let arm = match control.test {
			Some(test) => Statement::new_if_statement(SPAN, test, block, None, &self.ast),
			None => block,
		};
		if decisions.is_empty() {
			return arm;
		}
		let mut scoped = ArenaVec::new_in(&self.ast);
		for decision in decisions {
			scoped.push(self.let_uninit(self.ast.allocator.alloc_str(decision)));
		}
		if let Some(selection) = control.selection {
			scoped.push(Statement::new_expression_statement(
				SPAN, selection, &self.ast,
			));
		}
		scoped.push(arm);
		Statement::new_block_statement(SPAN, scoped, &self.ast)
	}

	fn emit_binary(
		&self,
		op: BinOp,
		result: BuiltinResult,
		mode: OperationMode,
		left: Expression<'a>,
		right: Expression<'a>,
	) -> Expression<'a> {
		match op {
			// `&&`/`||` are lowered in `emit_expr`'s `Binary` arm to an operand-reuse
			// ternary (`emit_logical`) — they never reach here, since the raw payload
			// (not the always-truthy box) must drive short-circuiting.
			BinOp::And | BinOp::Or => {
				unreachable!("logical `&&`/`||` are lowered in emit_expr, not emit_binary")
			}
			_ => {
				let (left, right) = if matches!(result, BuiltinResult::Raw) {
					(left, right)
				} else {
					(self.unwrap_v(left), self.unwrap_v(right))
				};
				if op == BinOp::Div && result == BuiltinResult::Float && mode == OperationMode::Checked {
					let raw = self.runtime_call("nymphCheckedDivide", vec![left, right]);
					return self.box_builtin_result(result, raw);
				}
				let (left, right) = if result == BuiltinResult::Float {
					(
						self.runtime_call("nymphIntegerToFloat", vec![left]),
						self.runtime_call("nymphIntegerToFloat", vec![right]),
					)
				} else {
					(left, right)
				};
				if op == BinOp::Pow
					&& matches!(result, BuiltinResult::Int | BuiltinResult::UInt)
					&& mode == OperationMode::Checked
				{
					let raw = self.runtime_call("nymphCheckedPower", vec![left, right]);
					return self.box_builtin_result(result, raw);
				}
				if matches!(op, BinOp::Shl | BinOp::Shr)
					&& matches!(result, BuiltinResult::Int | BuiltinResult::UInt)
					&& mode == OperationMode::Checked
				{
					let raw = self.runtime_call(
						"nymphCheckedShift",
						vec![
							left,
							right,
							Expression::new_boolean_literal(SPAN, op == BinOp::Shl, &self.ast),
						],
					);
					return self.box_builtin_result(result, raw);
				}
				let raw = Expression::BinaryExpression(BinaryExpression::boxed(
					SPAN,
					left,
					match op {
						BinOp::Add => BinaryOperator::Addition,
						BinOp::Sub => BinaryOperator::Subtraction,
						BinOp::Mul => BinaryOperator::Multiplication,
						BinOp::Div => BinaryOperator::Division,
						BinOp::Rem => BinaryOperator::Remainder,
						BinOp::Pow => BinaryOperator::Exponential,
						BinOp::Eq => BinaryOperator::StrictEquality,
						BinOp::Ne => BinaryOperator::StrictInequality,
						BinOp::Lt => BinaryOperator::LessThan,
						BinOp::Le => BinaryOperator::LessEqualThan,
						BinOp::Gt => BinaryOperator::GreaterThan,
						BinOp::Ge => BinaryOperator::GreaterEqualThan,
						BinOp::BitAnd => BinaryOperator::BitwiseAnd,
						BinOp::BitOr => BinaryOperator::BitwiseOR,
						BinOp::BitXor => BinaryOperator::BitwiseXOR,
						BinOp::Shl => BinaryOperator::ShiftLeft,
						BinOp::Shr => BinaryOperator::ShiftRight,
						BinOp::And | BinOp::Or => unreachable!("handled above"),
					},
					right,
					&self.ast,
				));
				self.box_builtin_result_with_mode(result, raw, mode)
			}
		}
	}

	fn box_builtin_result_with_mode(
		&self,
		result: BuiltinResult,
		raw: Expression<'a>,
		mode: OperationMode,
	) -> Expression<'a> {
		if mode == OperationMode::Direct {
			match result {
				BuiltinResult::Int => return self.direct_integer_box("NInt", raw),
				BuiltinResult::UInt => return self.direct_integer_box("NUint", raw),
				_ => {}
			}
		}
		self.box_builtin_result(result, raw)
	}

	fn box_builtin_result(&self, result: BuiltinResult, raw: Expression<'a>) -> Expression<'a> {
		let class = match result {
			BuiltinResult::Int => "NInt",
			BuiltinResult::UInt => "NUint",
			BuiltinResult::Float => "NFloat",
			BuiltinResult::Char => "NChar",
			BuiltinResult::String => "NString",
			BuiltinResult::Boolean => "NBool",
			BuiltinResult::Raw => return raw,
		};
		self.new_box(class, raw)
	}
}
