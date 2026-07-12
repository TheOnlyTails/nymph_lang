// oxc 0.138 deprecates most `AstBuilder` node-construction methods in favor of a
// "new AstBuilder interface" (oxc-project/oxc#23043) that is still landing upstream
// and is not yet present in this crate version. The deprecated methods below are the
// only usable construction path in 0.138 and are what the reference (oxc 0.123)
// transpiler also relies on; re-evaluate this `allow` when upgrading oxc.
#![allow(deprecated)]

use oxc::{
	allocator::{Allocator, Vec as OxcVec},
	ast::{AstBuilder, ast::*},
	codegen::Codegen,
	span::SPAN,
};

use nymph_hir::hir::{
	BinOp, HirClass, HirEnum, HirExpr, HirFunc, HirLit, HirModule, HirPat, HirStmt, UnOp,
};

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
}

/// Intermediate representation for expression-valued code.
///
/// In Nymph, blocks (and eventually `if`/`while` in value position) are
/// expressions. When emitting to JS we may need to wrap them in an IIFE.
/// `JsValue` keeps the leading statements separate from the final expression
/// so the common case (no statements) can emit the expression directly.
struct JsValue<'a> {
	stmts: OxcVec<'a, Statement<'a>>,
	expr: Expression<'a>,
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
		body_stmts.push(ast.statement_return(SPAN, Some(self.expr)));

		let body = ast.function_body(SPAN, ast.vec(), body_stmts);
		let params = ast.formal_parameters(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			ast.vec(),
			oxc::ast::NONE,
		);
		let arrow = ast.expression_arrow_function(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			body,
		);

		ast.expression_call(SPAN, arrow, oxc::ast::NONE, ast.vec(), false)
	}
}

pub struct Emitter<'a> {
	ast: AstBuilder<'a>,
	#[allow(dead_code)]
	alloc: &'a Allocator,
	/// Counter for fresh temporary names (result temporaries for value-position
	/// control flow). `Cell` keeps the emit methods `&self`.
	gensym: std::cell::Cell<u32>,
}

impl Default for Emitter<'_> {
	fn default() -> Self {
		Self::new()
	}
}

impl<'a> Emitter<'a> {
	pub fn new() -> Emitter<'static> {
		// Leak an allocator for the lifetime of the emit call; the returned String
		// outlives it. (A slice-1 simplification; a later slice can thread an
		// externally-owned Allocator if allocation pressure matters.)
		let alloc: &'static Allocator = Box::leak(Box::new(Allocator::default()));
		Emitter {
			ast: AstBuilder::new(alloc),
			alloc,
			gensym: std::cell::Cell::new(0),
		}
	}

	/// A fresh temporary variable name (`_t0`, `_t1`, …).
	fn gensym(&self) -> String {
		let n = self.gensym.get();
		self.gensym.set(n + 1);
		format!("_t{n}")
	}

	pub fn emit_module(&self, module: &HirModule) -> String {
		let mut stmts = self.ast.vec();
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
		}
		for func in &module.funcs {
			stmts.push(self.emit_func(func));
		}
		let program = self.ast.program(
			SPAN,
			SourceType::mjs(),
			"",
			self.ast.vec(),
			None,
			self.ast.vec(),
			stmts,
		);
		Codegen::new().build(&program).code
	}

	fn emit_func(&self, func: &HirFunc) -> Statement<'a> {
		// function <name>(<params>) { return <body>; }
		//
		// When the body is itself a `Block`, emit its statements directly into the
		// function body (followed by `return <tail>;`) instead of wrapping them in a
		// needless IIFE via `emit_expr`/`into_expression`.
		let mut body_stmts = self.ast.vec();
		match &func.body {
			HirExpr::Block { .. } => {
				let value = self.emit_value(&func.body);
				body_stmts.extend(value.stmts);
				body_stmts.push(self.ast.statement_return(SPAN, Some(value.expr)));
			}
			other => {
				let body_expr = self.emit_expr(other);
				body_stmts.push(self.ast.statement_return(SPAN, Some(body_expr)));
			}
		}
		let mut js_params = self.ast.vec();
		for param in &func.params {
			let binding_pattern = self
				.ast
				.binding_pattern_binding_identifier(SPAN, self.ast.allocator.alloc_str(param));
			js_params.push(self.ast.plain_formal_parameter(SPAN, binding_pattern));
		}
		let params = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			js_params,
			oxc::ast::NONE,
		);
		let fn_body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);
		let function = self.ast.alloc_function(
			SPAN,
			FunctionType::FunctionDeclaration,
			Some(
				self
					.ast
					.binding_identifier(SPAN, self.ast.allocator.alloc_str(&func.name)),
			),
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			Some(fn_body),
		);
		Statement::FunctionDeclaration(function)
	}

	/// Emit a struct as `class <Name> { constructor(fields) { Object.assign(this, fields); } }`.
	///
	/// The object-argument constructor lets construction pass labeled fields as a
	/// plain object (`new Point({ x, y })`) without depending on field order.
	/// `Object.assign` copies each property onto the instance; field defaults and
	/// validation are deferred to a later slice.
	fn emit_class(&self, class: &HirClass) -> Statement<'a> {
		// Object.assign(this, fields)
		let object_assign = Expression::from(self.ast.member_expression_static(
			SPAN,
			self.ast.expression_identifier(SPAN, "Object"),
			self.ast.identifier_name(SPAN, "assign"),
			false,
		));
		let mut call_args = self.ast.vec();
		call_args.push(Argument::from(self.ast.expression_this(SPAN)));
		call_args.push(Argument::from(
			self.ast.expression_identifier(SPAN, "fields"),
		));
		let assign_call =
			self
				.ast
				.expression_call(SPAN, object_assign, oxc::ast::NONE, call_args, false);
		let mut ctor_stmts = self.ast.vec();
		ctor_stmts.push(self.ast.statement_expression(SPAN, assign_call));

		// constructor(fields) { … }
		let mut ctor_params = self.ast.vec();
		let fields_pat = self.ast.binding_pattern_binding_identifier(SPAN, "fields");
		ctor_params.push(self.ast.plain_formal_parameter(SPAN, fields_pat));
		let params = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			ctor_params,
			oxc::ast::NONE,
		);
		let ctor_body = self.ast.function_body(SPAN, self.ast.vec(), ctor_stmts);
		let ctor_fn = self.ast.alloc_function(
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
		);
		let ctor = self.ast.class_element_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			self.ast.vec(),
			self.ast.property_key_static_identifier(SPAN, "constructor"),
			ctor_fn,
			MethodDefinitionKind::Constructor,
			false,
			false,
			false,
			false,
			None,
		);

		let mut elements = self.ast.vec();
		elements.push(ctor);
		let body = self.ast.class_body(SPAN, elements);
		let name = self
			.ast
			.binding_identifier(SPAN, self.ast.allocator.alloc_str(&class.name));
		let class = self.ast.alloc_class(
			SPAN,
			ClassType::ClassDeclaration,
			self.ast.vec(),
			Some(name),
			oxc::ast::NONE,
			None,
			oxc::ast::NONE,
			self.ast.vec(),
			body,
			false,
			false,
		);
		Statement::ClassDeclaration(class)
	}

	// ── Enum Symbol-tag ABI ────────────────────────────────────────────────────

	/// `const TAG = Symbol.for("nymph.tag");` — the shared discriminant key, the
	/// same symbol in every module via the global registry.
	fn emit_tag_const(&self) -> Statement<'a> {
		let symbol_for = Expression::from(self.ast.member_expression_static(
			SPAN,
			self.ast.expression_identifier(SPAN, "Symbol"),
			self.ast.identifier_name(SPAN, "for"),
			false,
		));
		let mut args = self.ast.vec();
		args.push(Argument::from(self.ast.expression_string_literal(
			SPAN,
			self.ast.allocator.alloc_str("nymph.tag"),
			None,
		)));
		let init = self
			.ast
			.expression_call(SPAN, symbol_for, oxc::ast::NONE, args, false);
		self.const_decl("TAG", init)
	}

	/// Emit an enum as `const <E> = (() => { const t0 = Symbol("E.V0"); … return
	/// { V0: <factory|singleton>, … }; })();`. The IIFE scopes each variant's unique
	/// symbol; field variants become object-arg factories, nullary variants frozen
	/// singletons — each carrying `[TAG]` so a matcher can compare identity.
	fn emit_enum(&self, hir_enum: &HirEnum) -> Statement<'a> {
		let mut stmts = self.ast.vec();
		let mut props = self.ast.vec();
		for (i, variant) in hir_enum.variants.iter().enumerate() {
			let t_name = format!("t{i}");
			// const t<i> = Symbol("<E>.<V>");
			let label = format!("{}.{}", hir_enum.name, variant.name);
			let mut sym_args = self.ast.vec();
			sym_args.push(Argument::from(self.ast.expression_string_literal(
				SPAN,
				self.ast.allocator.alloc_str(&label),
				None,
			)));
			let sym_call = self.ast.expression_call(
				SPAN,
				self.ast.expression_identifier(SPAN, "Symbol"),
				oxc::ast::NONE,
				sym_args,
				false,
			);
			stmts.push(self.const_decl(&t_name, sym_call));

			// The `{ [TAG]: t<i> }` object both variant shapes carry.
			let mut tag_props = self.ast.vec();
			tag_props.push(self.tag_prop(&t_name));
			let tag_obj = self.ast.expression_object(SPAN, tag_props);
			// The variant's value: a factory (fields) or a frozen singleton (nullary).
			let value = if variant.fields.is_empty() {
				self.member_call(
					self.ast.expression_identifier(SPAN, "Object"),
					"freeze",
					vec![tag_obj],
				)
			} else {
				let factory = self.variant_factory(&t_name);
				self.member_call(
					self.ast.expression_identifier(SPAN, "Object"),
					"assign",
					vec![factory, tag_obj],
				)
			};

			let key = self
				.ast
				.property_key_static_identifier(SPAN, self.ast.allocator.alloc_str(&variant.name));
			props.push(ObjectPropertyKind::ObjectProperty(
				self
					.ast
					.alloc_object_property(SPAN, PropertyKind::Init, key, value, false, false, false),
			));
		}
		let return_obj = self.ast.expression_object(SPAN, props);
		let iife = JsValue {
			stmts,
			expr: return_obj,
		}
		.into_expression(self.ast);
		self.const_decl(hir_enum.name.as_str(), iife)
	}

	/// `(fields) => { return { [TAG]: <t_name>, ...fields }; }` — a field variant's
	/// object-argument factory.
	fn variant_factory(&self, t_name: &str) -> Expression<'a> {
		let mut obj_props = self.ast.vec();
		obj_props.push(self.tag_prop(t_name));
		obj_props.push(
			self
				.ast
				.object_property_kind_spread_property(SPAN, self.ast.expression_identifier(SPAN, "fields")),
		);
		let obj = self.ast.expression_object(SPAN, obj_props);
		let mut body_stmts = self.ast.vec();
		body_stmts.push(self.ast.statement_return(SPAN, Some(obj)));
		let body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);
		let mut params = self.ast.vec();
		params.push(self.ast.plain_formal_parameter(
			SPAN,
			self.ast.binding_pattern_binding_identifier(SPAN, "fields"),
		));
		let formal = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			params,
			oxc::ast::NONE,
		);
		self.ast.expression_arrow_function(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			formal,
			oxc::ast::NONE,
			body,
		)
	}

	/// A computed `[TAG]: <t_name>` object property.
	fn tag_prop(&self, t_name: &str) -> ObjectPropertyKind<'a> {
		let key = PropertyKey::from(self.ast.expression_identifier(SPAN, "TAG"));
		let value = self
			.ast
			.expression_identifier(SPAN, self.ast.allocator.alloc_str(t_name));
		ObjectPropertyKind::ObjectProperty(self.ast.alloc_object_property(
			SPAN,
			PropertyKind::Init,
			key,
			value,
			false,
			false,
			true,
		))
	}

	/// `const <name> = <init>;`
	fn const_decl(&self, name: &str, init: Expression<'a>) -> Statement<'a> {
		let pat = self
			.ast
			.binding_pattern_binding_identifier(SPAN, self.ast.allocator.alloc_str(name));
		let declarator = self.ast.variable_declarator(
			SPAN,
			VariableDeclarationKind::Const,
			pat,
			oxc::ast::NONE,
			Some(init),
			false,
		);
		let decl = self.ast.variable_declaration(
			SPAN,
			VariableDeclarationKind::Const,
			self.ast.vec1(declarator),
			false,
		);
		Statement::from(Declaration::VariableDeclaration(self.ast.alloc(decl)))
	}

	/// `<enum>.<variant>` — a member access on the enum's ABI object (a factory or
	/// a frozen singleton).
	fn variant_member(&self, enum_name: &str, variant: &str) -> Expression<'a> {
		Expression::from(
			self.ast.member_expression_static(
				SPAN,
				self
					.ast
					.expression_identifier(SPAN, self.ast.allocator.alloc_str(enum_name)),
				self
					.ast
					.identifier_name(SPAN, self.ast.allocator.alloc_str(variant)),
				false,
			),
		)
	}

	/// `<callee>(<arg>)` — a single-argument call.
	fn call1(&self, callee: Expression<'a>, arg: Expression<'a>) -> Expression<'a> {
		let mut args = self.ast.vec();
		args.push(Argument::from(arg));
		self
			.ast
			.expression_call(SPAN, callee, oxc::ast::NONE, args, false)
	}

	/// `object.method(...args)`.
	fn member_call(
		&self,
		object: Expression<'a>,
		method: &str,
		args: Vec<Expression<'a>>,
	) -> Expression<'a> {
		let callee = Expression::from(
			self.ast.member_expression_static(
				SPAN,
				object,
				self
					.ast
					.identifier_name(SPAN, self.ast.allocator.alloc_str(method)),
				false,
			),
		);
		let mut js_args = self.ast.vec();
		for a in args {
			js_args.push(Argument::from(a));
		}
		self
			.ast
			.expression_call(SPAN, callee, oxc::ast::NONE, js_args, false)
	}

	fn emit_expr(&self, expr: &HirExpr) -> Expression<'a> {
		match expr {
			HirExpr::Num(value) => {
				self
					.ast
					.expression_numeric_literal(SPAN, *value, None, NumberBase::Decimal)
			}
			HirExpr::Str(s) => {
				self
					.ast
					.expression_string_literal(SPAN, self.ast.allocator.alloc_str(s), None)
			}
			HirExpr::Bool(b) => self.ast.expression_boolean_literal(SPAN, *b),
			HirExpr::Char(c) => {
				// A Nymph char is a single-character JS string.
				let s = self.ast.allocator.alloc_str(&c.to_string());
				self.ast.expression_string_literal(SPAN, s, None)
			}
			HirExpr::Local(name) => self
				.ast
				.expression_identifier(SPAN, self.ast.allocator.alloc_str(name)),
			HirExpr::Binary { op, lhs, rhs } => {
				let left = self.emit_expr(lhs);
				let right = self.emit_expr(rhs);
				self.emit_binary(*op, left, right)
			}
			HirExpr::Unary { op, operand } => {
				let inner = self.emit_expr(operand);
				let operator = match op {
					UnOp::Neg => UnaryOperator::UnaryNegation,
					UnOp::Not => UnaryOperator::LogicalNot,
				};
				self.ast.expression_unary(SPAN, operator, inner)
			}
			HirExpr::Call { callee, args } => {
				let callee = self.emit_expr(callee);
				let mut arguments = self.ast.vec();
				for arg in args {
					arguments.push(Argument::from(self.emit_expr(arg)));
				}
				self
					.ast
					.expression_call(SPAN, callee, oxc::ast::NONE, arguments, false)
			}
			// A tuple/list literal → a JS array `[a, b, …]`.
			HirExpr::Array(items) => {
				let mut elems = self.ast.vec();
				for item in items {
					elems.push(ArrayExpressionElement::from(self.emit_expr(item)));
				}
				self.ast.expression_array(SPAN, elems)
			}
			// A map literal → `new Map([[k, v], …])`.
			HirExpr::MapLit(pairs) => {
				let mut entries = self.ast.vec();
				for (k, v) in pairs {
					let mut pair = self.ast.vec();
					pair.push(ArrayExpressionElement::from(self.emit_expr(k)));
					pair.push(ArrayExpressionElement::from(self.emit_expr(v)));
					let arr = self.ast.expression_array(SPAN, pair);
					entries.push(ArrayExpressionElement::from(arr));
				}
				let outer = self.ast.expression_array(SPAN, entries);
				let callee = self.ast.expression_identifier(SPAN, "Map");
				let mut args = self.ast.vec();
				args.push(Argument::from(outer));
				self.ast.expression_new(SPAN, callee, oxc::ast::NONE, args)
			}
			// A list/tuple subscript → a computed member `recv[index]`.
			HirExpr::Index { recv, index } => {
				let object = self.emit_expr(recv);
				let property = self.emit_expr(index);
				Expression::ComputedMemberExpression(
					self
						.ast
						.alloc_computed_member_expression(SPAN, object, property, false),
				)
			}
			// Struct construction → `new <class>({ field: value, … })`.
			HirExpr::New { class, fields } => {
				let mut props = self.ast.vec();
				for (name, value) in fields {
					let key = self
						.ast
						.property_key_static_identifier(SPAN, self.ast.allocator.alloc_str(name));
					let val = self.emit_expr(value);
					props.push(ObjectPropertyKind::ObjectProperty(
						self
							.ast
							.alloc_object_property(SPAN, PropertyKind::Init, key, val, false, false, false),
					));
				}
				let obj = self.ast.expression_object(SPAN, props);
				let callee = self
					.ast
					.expression_identifier(SPAN, self.ast.allocator.alloc_str(class));
				let mut args = self.ast.vec();
				args.push(Argument::from(obj));
				self.ast.expression_new(SPAN, callee, oxc::ast::NONE, args)
			}
			// Field access → `recv.name`.
			HirExpr::Field { recv, name } => {
				let object = self.emit_expr(recv);
				Expression::from(
					self.ast.member_expression_static(
						SPAN,
						object,
						self
							.ast
							.identifier_name(SPAN, self.ast.allocator.alloc_str(name)),
						false,
					),
				)
			}
			// Variant construction → `<enum>.<variant>({ field: value, … })`.
			HirExpr::VariantNew {
				enum_name,
				variant,
				fields,
			} => {
				let mut props = self.ast.vec();
				for (name, value) in fields {
					let key = self
						.ast
						.property_key_static_identifier(SPAN, self.ast.allocator.alloc_str(name));
					let val = self.emit_expr(value);
					props.push(ObjectPropertyKind::ObjectProperty(
						self
							.ast
							.alloc_object_property(SPAN, PropertyKind::Init, key, val, false, false, false),
					));
				}
				let obj = self.ast.expression_object(SPAN, props);
				let callee = self.variant_member(enum_name, variant);
				self.call1(callee, obj)
			}
			// Nullary variant reference → `<enum>.<variant>` (the frozen singleton).
			HirExpr::VariantRef { enum_name, variant } => self.variant_member(enum_name, variant),
			// A map lookup → `recv.get(key)`.
			HirExpr::MapGet { recv, key } => {
				let object = self.emit_expr(recv);
				let member = Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
					SPAN,
					object,
					self.ast.identifier_name(SPAN, "get"),
					false,
				));
				let mut args = self.ast.vec();
				args.push(Argument::from(self.emit_expr(key)));
				self
					.ast
					.expression_call(SPAN, member, oxc::ast::NONE, args, false)
			}
			HirExpr::Assign { target, value } => {
				let value_expr = self.emit_expr(value);
				let name = match target.as_ref() {
					HirExpr::Local(n) => self.ast.allocator.alloc_str(n),
					_ => unreachable!("slice-1 assignment targets are identifiers"),
				};
				self.ast.expression_assignment(
					SPAN,
					AssignmentOperator::Assign,
					self.assign_target(name),
					value_expr,
				)
			}
			// Control-flow expressions in value position collapse to an expression
			// (an IIFE when they carry leading statements).
			HirExpr::Block { .. }
			| HirExpr::If { .. }
			| HirExpr::While { .. }
			| HirExpr::Match { .. } => self.emit_value(expr).into_expression(self.ast),
		}
	}

	/// A simple-identifier assignment target for `<name> = …`.
	fn assign_target(&self, name: &'a str) -> AssignmentTarget<'a> {
		AssignmentTarget::AssignmentTargetIdentifier(self.ast.alloc_identifier_reference(SPAN, name))
	}

	/// `let <name>;` — an uninitialised binding for a control-flow result temporary.
	fn let_uninit(&self, name: &'a str) -> Statement<'a> {
		let pat = self.ast.binding_pattern_binding_identifier(SPAN, name);
		let declarator = self.ast.variable_declarator(
			SPAN,
			VariableDeclarationKind::Let,
			pat,
			oxc::ast::NONE,
			None,
			false,
		);
		let decl = self.ast.variable_declaration(
			SPAN,
			VariableDeclarationKind::Let,
			self.ast.vec1(declarator),
			false,
		);
		Statement::from(Declaration::VariableDeclaration(self.ast.alloc(decl)))
	}

	/// `{ <branch stmts>; <name> = <branch value>; }` — a block that assigns an
	/// (optional) branch's value to `name` (or `undefined` when the branch is absent).
	fn assign_block(&self, name: &'a str, branch: Option<&HirExpr>) -> Statement<'a> {
		let val = match branch {
			Some(b) => self.emit_value(b),
			None => JsValue {
				stmts: self.ast.vec(),
				expr: self.ast.expression_identifier(SPAN, "undefined"),
			},
		};
		let mut stmts = val.stmts;
		let assign = self.ast.expression_assignment(
			SPAN,
			AssignmentOperator::Assign,
			self.assign_target(name),
			val.expr,
		);
		stmts.push(self.ast.statement_expression(SPAN, assign));
		self.ast.statement_block(SPAN, stmts)
	}

	/// A HIR expression emitted as a JS block statement, evaluating its value for
	/// effect (used for a `while` body, whose value is discarded).
	fn block_stmt(&self, expr: &HirExpr) -> Statement<'a> {
		let val = self.emit_value(expr);
		let mut stmts = val.stmts;
		stmts.push(self.ast.statement_expression(SPAN, val.expr));
		self.ast.statement_block(SPAN, stmts)
	}

	/// Emit a single HIR statement as a JS statement.
	fn emit_stmt(&self, stmt: &HirStmt) -> Statement<'a> {
		match stmt {
			HirStmt::Let {
				name,
				mutable,
				value,
			} => {
				let kind = if *mutable {
					VariableDeclarationKind::Let
				} else {
					VariableDeclarationKind::Const
				};
				let init = self.emit_expr(value);
				let pat = self
					.ast
					.binding_pattern_binding_identifier(SPAN, self.ast.allocator.alloc_str(name));
				let declarator =
					self
						.ast
						.variable_declarator(SPAN, kind, pat, oxc::ast::NONE, Some(init), false);
				let decl = self
					.ast
					.variable_declaration(SPAN, kind, self.ast.vec1(declarator), false);
				Statement::from(Declaration::VariableDeclaration(self.ast.alloc(decl)))
			}
			HirStmt::Expr(e) => {
				let expr = self.emit_expr(e);
				self.ast.statement_expression(SPAN, expr)
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
				let mut js_stmts = self.ast.vec();
				for stmt in stmts {
					js_stmts.push(self.emit_stmt(stmt));
				}
				let tail_expr = match tail {
					Some(tail) => self.emit_expr(tail),
					None => self.ast.expression_identifier(SPAN, "undefined"),
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
				let cond_expr = self.emit_expr(cond);
				let then_stmt = self.assign_block(tmp, Some(then));
				let else_stmt = self.assign_block(tmp, otherwise.as_deref());
				let if_stmt = self
					.ast
					.statement_if(SPAN, cond_expr, then_stmt, Some(else_stmt));
				let mut stmts = self.ast.vec();
				stmts.push(decl);
				stmts.push(if_stmt);
				JsValue {
					stmts,
					expr: self.ast.expression_identifier(SPAN, tmp),
				}
			}
			HirExpr::While { cond, body } => {
				// A `while` is a statement; its value is `undefined`.
				let cond_expr = self.emit_expr(cond);
				let body_stmt = self.block_stmt(body);
				let while_stmt = self.ast.statement_while(SPAN, cond_expr, body_stmt);
				let mut stmts = self.ast.vec();
				stmts.push(while_stmt);
				JsValue {
					stmts,
					expr: self.ast.expression_identifier(SPAN, "undefined"),
				}
			}
			HirExpr::Match { scrutinee, arms } => {
				// const <s> = <scrutinee>; let <r>;
				// if (<test0>) { <binds0>; <r> = <body0> } else if … else { <bindsN>; <r> = <bodyN> }
				// → <r>    (totality lets the last arm skip its test)
				// `s`/`r` are fresh gensym temps (`_tN`), not literally `_s`/`_r`.
				let s = self.ast.allocator.alloc_str(&self.gensym());
				let r = self.ast.allocator.alloc_str(&self.gensym());
				let mut stmts = self.ast.vec();
				let scrutinee_expr = self.emit_expr(scrutinee);
				stmts.push(self.const_decl(s, scrutinee_expr));
				stmts.push(self.let_uninit(r));
				let subj = Subject::Temp(s.to_string());
				// Build the chain back-to-front so each `else` is the previously-built arm.
				let mut chain: Option<Statement<'a>> = None;
				for (i, arm) in arms.iter().enumerate().rev() {
					let (test, binds) = self.compile_pat(&arm.pat, &subj);
					let block = self.arm_block(r, &binds, &arm.body);
					let is_last = i + 1 == arms.len();
					chain = Some(if is_last && chain.is_none() {
						block
					} else {
						let cond = test.unwrap_or_else(|| self.ast.expression_boolean_literal(SPAN, true));
						self.ast.statement_if(SPAN, cond, block, chain.take())
					});
				}
				if let Some(chain) = chain {
					stmts.push(chain);
				}
				JsValue {
					stmts,
					expr: self.ast.expression_identifier(SPAN, r),
				}
			}
			other => JsValue {
				stmts: self.ast.vec(),
				expr: self.emit_expr(other),
			},
		}
	}

	/// Re-emit a subject reference (scrutinee temp or a field path) as a fresh
	/// expression. Needed because tests and each binding require their own copy.
	fn emit_subject(&self, s: &Subject) -> Expression<'a> {
		match s {
			Subject::Temp(name) => self
				.ast
				.expression_identifier(SPAN, self.ast.allocator.alloc_str(name)),
			Subject::Field(base, field) => {
				let object = self.emit_subject(base);
				Expression::from(
					self.ast.member_expression_static(
						SPAN,
						object,
						self
							.ast
							.identifier_name(SPAN, self.ast.allocator.alloc_str(field)),
						false,
					),
				)
			}
		}
	}

	/// A scalar pattern literal as a JS expression (for `=== <lit>` tests).
	fn emit_lit(&self, lit: &HirLit) -> Expression<'a> {
		match lit {
			HirLit::Num(v) => self
				.ast
				.expression_numeric_literal(SPAN, *v, None, NumberBase::Decimal),
			HirLit::Bool(b) => self.ast.expression_boolean_literal(SPAN, *b),
			HirLit::Char(c) => {
				let s = self.ast.allocator.alloc_str(&c.to_string());
				self.ast.expression_string_literal(SPAN, s, None)
			}
		}
	}

	/// `<obj>[TAG]` (optional-chained when `optional`), reading the variant tag.
	fn tag_read(&self, obj: Expression<'a>, optional: bool) -> Expression<'a> {
		Expression::ComputedMemberExpression(self.ast.alloc_computed_member_expression(
			SPAN,
			obj,
			self.ast.expression_identifier(SPAN, "TAG"),
			optional,
		))
	}

	/// Compile a pattern against a subject into a boolean test (`None` ⇒ always true)
	/// and a sequence of `(name, subject)` bindings.
	fn compile_pat(
		&self,
		pat: &HirPat,
		subj: &Subject,
	) -> (Option<Expression<'a>>, Vec<(String, Subject)>) {
		match pat {
			HirPat::Wildcard => (None, Vec::new()),
			HirPat::Binding { name, sub } => {
				let mut binds = vec![(name.to_string(), subj.clone())];
				let test = match sub {
					None => None,
					Some(sub) => {
						let (t, mut b) = self.compile_pat(sub, subj);
						binds.append(&mut b);
						t
					}
				};
				(test, binds)
			}
			HirPat::Lit(lit) => {
				let subject = self.emit_subject(subj);
				let value = self.emit_lit(lit);
				let test = self
					.ast
					.expression_binary(SPAN, subject, BinaryOperator::StrictEquality, value);
				(Some(test), Vec::new())
			}
			HirPat::Variant {
				enum_name,
				variant,
				fields,
			} => {
				// <subject>?.[TAG] === <enum>.<variant>[TAG]
				let subject_tag = self.tag_read(self.emit_subject(subj), true);
				let variant_tag = self.tag_read(self.variant_member(enum_name, variant), false);
				let mut test = self.ast.expression_binary(
					SPAN,
					subject_tag,
					BinaryOperator::StrictEquality,
					variant_tag,
				);
				let mut binds = Vec::new();
				for (field, sub) in fields {
					let field_subj = Subject::Field(Box::new(subj.clone()), field.to_string());
					let (t, mut b) = self.compile_pat(sub, &field_subj);
					binds.append(&mut b);
					if let Some(t) = t {
						test = self
							.ast
							.expression_logical(SPAN, test, LogicalOperator::And, t);
					}
				}
				(Some(test), binds)
			}
		}
	}

	/// `{ const <name> = <subject>; …; <result> = <body>; }` — an arm's body block.
	fn arm_block(
		&self,
		result: &'a str,
		binds: &[(String, Subject)],
		body: &HirExpr,
	) -> Statement<'a> {
		let mut stmts = self.ast.vec();
		for (name, subj) in binds {
			let init = self.emit_subject(subj);
			stmts.push(self.const_decl(name, init));
		}
		let val = self.emit_value(body);
		stmts.extend(val.stmts);
		let assign = self.ast.expression_assignment(
			SPAN,
			AssignmentOperator::Assign,
			self.assign_target(result),
			val.expr,
		);
		stmts.push(self.ast.statement_expression(SPAN, assign));
		self.ast.statement_block(SPAN, stmts)
	}

	fn emit_binary(&self, op: BinOp, left: Expression<'a>, right: Expression<'a>) -> Expression<'a> {
		// Logical operators are a distinct oxc node from binary operators.
		if let BinOp::And | BinOp::Or = op {
			let operator = if op == BinOp::And {
				LogicalOperator::And
			} else {
				LogicalOperator::Or
			};
			return self.ast.expression_logical(SPAN, left, operator, right);
		}
		let operator = match op {
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
		};
		self.ast.expression_binary(SPAN, left, operator, right)
	}
}
