use std::path::Path;

use oxc_allocator::{Allocator, Vec as OxcVec};
use oxc_ast::{AstBuilder, NONE, ast::*};
use oxc_span::SPAN;

use crate::{
	ast::{
		self, Spanned,
		declaration::{
			Declaration, FuncDeclaration, ImplMember, LetDeclaration, Module, StructInnerMember,
			Visibility,
		},
		expr::{
			Expr, ListItem, MapEntry, MatchArm, Pattern, Statement as NymphStatement, StringEscape,
			StringPart,
		},
		ops::{AssignOperator, BinaryOperator, PatternOperator},
	},
	transpiler::{
		external::{external_export_name, find_external_module},
		operators::{assign_op_to_binary, binary_op_method, postfix_op_method, prefix_op_method},
	},
	types::{Context, Type},
};

/// Intermediate representation for expression-valued code.
///
/// In Nymph, blocks / if / match are all expressions. When emitting to JS
/// we may need to wrap them in an IIFE.  `JsValue` keeps the leading
/// statements separate from the final expression so that we can optimise
/// the common case (no statements ⟹ emit expression directly).
pub struct JsValue<'a> {
	pub stmts: OxcVec<'a, Statement<'a>>,
	pub expr: Expression<'a>,
}

impl<'a> JsValue<'a> {
	/// Collapse into a single JS expression.
	/// If there are leading statements, wrap in an IIFE:
	/// `(() => { ...stmts; return expr; })()`
	pub fn into_expression(self, ast: AstBuilder<'a>, alloc: &'a Allocator) -> Expression<'a> {
		if self.stmts.is_empty() {
			return self.expr;
		}

		let mut body_stmts = OxcVec::with_capacity_in(self.stmts.len() + 1, alloc);
		for s in self.stmts {
			body_stmts.push(s);
		}
		body_stmts.push(ast.statement_return(SPAN, Some(self.expr)));

		let body = ast.function_body(SPAN, ast.vec(), body_stmts);
		let params = ast.formal_parameters(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			ast.vec(),
			NONE,
		);
		let arrow = ast.expression_arrow_function(SPAN, false, false, NONE, params, NONE, body);

		ast.expression_call(SPAN, arrow, NONE, ast.vec(), false)
	}
}

/// The main code emitter.
pub struct Emitter<'a> {
	pub alloc: &'a Allocator,
	pub ast: AstBuilder<'a>,
	pub ctx: &'a Context,
	gensym: u64,
	/// Path to the current `.nym` source file (for resolving externals).
	source_path: Option<&'a Path>,
}

impl<'a> Emitter<'a> {
	pub fn new(alloc: &'a Allocator, ctx: &'a Context, source_path: Option<&'a Path>) -> Self {
		let ast = AstBuilder::new(alloc);
		Self {
			alloc,
			ast,
			ctx,
			gensym: 0,
			source_path,
		}
	}

	/// Generate a unique temporary variable name.
	fn gensym(&mut self, prefix: &str) -> String {
		let id = self.gensym;
		self.gensym += 1;
		format!("__{prefix}${id}")
	}

	// ───────────────────── helpers ─────────────────────

	/// Allocate a string in the arena, producing a `&'a str`.
	fn arena_str(&self, s: &str) -> &'a str {
		self.ast.allocator.alloc_str(s)
	}

	fn ident_ref(&self, name: &str) -> Expression<'a> {
		self.ast.expression_identifier(SPAN, self.arena_str(name))
	}

	fn string_lit(&self, value: &str) -> Expression<'a> {
		self
			.ast
			.expression_string_literal(SPAN, self.arena_str(value), None)
	}

	fn number_lit(&self, value: f64) -> Expression<'a> {
		self
			.ast
			.expression_numeric_literal(SPAN, value, None, NumberBase::Decimal)
	}

	fn bool_lit(&self, value: bool) -> Expression<'a> {
		self.ast.expression_boolean_literal(SPAN, value)
	}

	fn undefined(&self) -> Expression<'a> {
		self.ident_ref("undefined")
	}

	fn member(&self, object: Expression<'a>, prop: &str) -> Expression<'a> {
		Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
			SPAN,
			object,
			self.ast.identifier_name(SPAN, self.arena_str(prop)),
			false,
		))
	}

	fn method_call(
		&self,
		object: Expression<'a>,
		method: &str,
		args: OxcVec<'a, Argument<'a>>,
	) -> Expression<'a> {
		let callee = self.member(object, method);
		self.ast.expression_call(SPAN, callee, NONE, args, false)
	}

	fn call(&self, callee: Expression<'a>, args: OxcVec<'a, Argument<'a>>) -> Expression<'a> {
		self.ast.expression_call(SPAN, callee, NONE, args, false)
	}

	fn binding_pattern(&self, name: &str) -> BindingPattern<'a> {
		self
			.ast
			.binding_pattern_binding_identifier(SPAN, self.arena_str(name))
	}

	fn assign_target_static_member(
		&self,
		object: Expression<'a>,
		prop: &str,
	) -> AssignmentTarget<'a> {
		AssignmentTarget::StaticMemberExpression(self.ast.alloc_static_member_expression(
			SPAN,
			object,
			self.ast.identifier_name(SPAN, self.arena_str(prop)),
			false,
		))
	}

	fn assign_target_computed_member(
		&self,
		object: Expression<'a>,
		index: Expression<'a>,
	) -> AssignmentTarget<'a> {
		AssignmentTarget::ComputedMemberExpression(
			self
				.ast
				.alloc_computed_member_expression(SPAN, object, index, false),
		)
	}

	fn var_decl_stmt(
		&self,
		kind: VariableDeclarationKind,
		name: &str,
		init: Expression<'a>,
	) -> Statement<'a> {
		let declarator = self.ast.variable_declarator(
			SPAN,
			kind,
			self.binding_pattern(name),
			NONE,
			Some(init),
			false,
		);
		let decl = self
			.ast
			.variable_declaration(SPAN, kind, self.ast.vec1(declarator), false);
		Statement::from(oxc_ast::ast::Declaration::VariableDeclaration(
			self.ast.alloc(decl),
		))
	}

	// ───────────────── module-level emit ─────────────────

	/// Emit an entire Nymph module as a list of JS statements.
	pub fn emit_module(&mut self, module: &'a Module) -> OxcVec<'a, Statement<'a>> {
		let mut stmts = self.ast.vec();
		for decl in &module.members {
			let emitted = self.emit_declaration(decl, None);
			stmts.extend(emitted);
		}
		stmts
	}

	/// Emit a top-level declaration. Returns zero or more JS statements.
	///
	/// `outer_name` is set when inside a struct/enum body, to qualify external names.
	pub fn emit_declaration(
		&mut self,
		decl: &'a Declaration,
		outer_name: Option<&str>,
	) -> Vec<Statement<'a>> {
		match decl {
			Declaration::Import { .. } => {
				// TODO: emit ES6 import statements
				vec![]
			}

			Declaration::Let {
				visibility,
				meta,
				value,
			} => {
				let js_val = self.emit_expr(value);
				let expr = js_val.into_expression(self.ast, self.alloc);
				let is_export = matches!(visibility, Some(Visibility::Public));
				let kind = if meta.mutable {
					VariableDeclarationKind::Let
				} else {
					VariableDeclarationKind::Const
				};
				let name = self.pattern_to_binding_name(&meta.name.0);
				let stmt = self.var_decl_stmt(kind, &name, expr);
				if is_export {
					vec![self.export_stmt(stmt)]
				} else {
					vec![stmt]
				}
			}

			Declaration::ExternalLet(visibility, meta) => {
				self.emit_external_let(meta, *visibility, outer_name)
			}

			Declaration::Func {
				visibility,
				meta,
				body,
			} => {
				let func_stmt = self.emit_func(meta, body, matches!(visibility, Some(Visibility::Public)));
				vec![func_stmt]
			}

			Declaration::ExternalFunc(visibility, meta) => {
				self.emit_external_func(meta, *visibility, outer_name)
			}

			Declaration::TypeAlias { .. } => {
				// Type aliases are erased at runtime.
				vec![]
			}

			Declaration::Struct {
				visibility,
				name,
				fields,
				members,
				generics: _,
			} => self.emit_struct(
				name,
				fields,
				members,
				matches!(visibility, Some(Visibility::Public)),
			),

			Declaration::Enum {
				visibility,
				name,
				variants,
				members,
				generics: _,
			} => self.emit_enum(
				name,
				variants,
				members,
				matches!(visibility, Some(Visibility::Public)),
			),

			Declaration::Interface { .. } => {
				// Interfaces are erased at runtime.
				vec![]
			}

			Declaration::Namespace {
				visibility,
				name,
				members,
			} => self.emit_namespace(
				name,
				members,
				matches!(visibility, Some(Visibility::Public)),
			),

			Declaration::Impl { type_, members, .. } => self.emit_impl(type_, members),

			Declaration::ImplFor { type_, members, .. } => self.emit_impl(type_, members),
		}
	}

	// ───────────────── struct emit ─────────────────

	fn emit_struct(
		&mut self,
		name: &'a ast::Ident,
		fields: &[Spanned<crate::ast::declaration::StructField>],
		inner_members: &'a [Spanned<StructInnerMember>],
		export: bool,
	) -> Vec<Statement<'a>> {
		let class_name = name.0.as_str();

		// Constructor parameters & assignment
		let mut constructor_params = self.ast.vec();
		let mut constructor_body_stmts = self.ast.vec();

		for field in fields {
			let fname = field.0.name.0.as_str();
			let param = self
				.ast
				.plain_formal_parameter(SPAN, self.binding_pattern(fname));
			constructor_params.push(param);

			// this.fieldName = fieldName;
			let assign = self.ast.expression_assignment(
				SPAN,
				AssignmentOperator::Assign,
				self.assign_target_static_member(self.ast.expression_this(SPAN), fname),
				self.ident_ref(fname),
			);
			constructor_body_stmts.push(self.ast.statement_expression(SPAN, assign));
		}

		let constructor_params = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			constructor_params,
			NONE,
		);
		let constructor_body = self
			.ast
			.function_body(SPAN, self.ast.vec(), constructor_body_stmts);

		let constructor = self.ast.class_element_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			self.ast.vec(),
			PropertyKey::StaticIdentifier(
				self
					.ast
					.alloc(self.ast.identifier_name(SPAN, "constructor")),
			),
			self.ast.alloc(self.ast.function(
				SPAN,
				FunctionType::FunctionExpression,
				None::<BindingIdentifier<'a>>,
				false, // generator
				false, // async
				false, // declare
				NONE,
				NONE,
				constructor_params,
				NONE,
				Some(constructor_body),
			)),
			MethodDefinitionKind::Constructor,
			false, // computed
			false, // static
			false, // override
			false, // optional
			None,
		);

		let mut class_body_elements = self.ast.vec();
		class_body_elements.push(constructor);

		// Emit instance members
		for inner in inner_members {
			match &inner.0 {
				StructInnerMember::Member(member) => {
					if let Some(el) = self.emit_impl_member_as_class_element(&member.0, false) {
						class_body_elements.push(el);
					}
				}
				StructInnerMember::Namespace(members) => {
					for m in members {
						if let Some(el) = self.emit_impl_member_as_class_element(&m.0, true) {
							class_body_elements.push(el);
						}
					}
				}
				StructInnerMember::Impl { members, .. } => {
					for m in members {
						if let Some(el) = self.emit_impl_member_as_class_element(&m.0, false) {
							class_body_elements.push(el);
						}
					}
				}
				StructInnerMember::ImplMut(members) => {
					for m in members {
						if let Some(el) = self.emit_impl_member_as_class_element(&m.0, false) {
							class_body_elements.push(el);
						}
					}
				}
			}
		}

		// Actually, let's use declaration_class directly
		self.emit_class_declaration(class_name, fields, inner_members, export)
	}

	fn emit_class_declaration(
		&mut self,
		class_name: &'a str,
		fields: &[Spanned<crate::ast::declaration::StructField>],
		inner_members: &'a [Spanned<StructInnerMember>],
		export: bool,
	) -> Vec<Statement<'a>> {
		let mut constructor_params = self.ast.vec();
		let mut constructor_body_stmts = self.ast.vec();

		for field in fields {
			let fname = field.0.name.0.as_str();
			constructor_params.push(
				self
					.ast
					.plain_formal_parameter(SPAN, self.binding_pattern(fname)),
			);
			let assign = self.ast.expression_assignment(
				SPAN,
				AssignmentOperator::Assign,
				self.assign_target_static_member(self.ast.expression_this(SPAN), fname),
				self.ident_ref(fname),
			);
			constructor_body_stmts.push(self.ast.statement_expression(SPAN, assign));
		}

		let params = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			constructor_params,
			NONE,
		);
		let body = self
			.ast
			.function_body(SPAN, self.ast.vec(), constructor_body_stmts);
		let constructor = self.ast.class_element_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			self.ast.vec(),
			PropertyKey::StaticIdentifier(
				self
					.ast
					.alloc(self.ast.identifier_name(SPAN, "constructor")),
			),
			self.ast.alloc(self.ast.function(
				SPAN,
				FunctionType::FunctionExpression,
				None::<BindingIdentifier<'a>>,
				false,
				false,
				false,
				NONE,
				NONE,
				params,
				NONE,
				Some(body),
			)),
			MethodDefinitionKind::Constructor,
			false,
			false,
			false,
			false,
			None,
		);

		let mut elements = self.ast.vec();
		elements.push(constructor);

		for inner in inner_members {
			match &inner.0 {
				StructInnerMember::Member(m) => {
					if let Some(el) = self.emit_impl_member_as_class_element(&m.0, false) {
						elements.push(el);
					}
				}
				StructInnerMember::Namespace(members) => {
					for m in members {
						if let Some(el) = self.emit_impl_member_as_class_element(&m.0, true) {
							elements.push(el);
						}
					}
				}
				StructInnerMember::Impl { members, .. } | StructInnerMember::ImplMut(members) => {
					for m in members {
						if let Some(el) = self.emit_impl_member_as_class_element(&m.0, false) {
							elements.push(el);
						}
					}
				}
			}
		}

		let class_body = self.ast.class_body(SPAN, elements);
		let class_decl = self.ast.declaration_class(
			SPAN,
			ClassType::ClassDeclaration,
			self.ast.vec(),
			Some(self.ast.binding_identifier(SPAN, class_name)),
			NONE,
			None::<Expression<'a>>,
			NONE,
			self.ast.vec(),
			class_body,
			false,
			false,
		);

		let stmt: Statement<'a> = Statement::from(class_decl);
		if export {
			vec![self.export_stmt(stmt)]
		} else {
			vec![stmt]
		}
	}

	fn emit_impl_member_as_class_element(
		&mut self,
		member: &'a ImplMember,
		is_static: bool,
	) -> Option<ClassElement<'a>> {
		match member {
			ImplMember::Func { meta, body, .. } => {
				let method_name = meta.name.0.as_str();
				let mut params = self.ast.vec();
				for p in &meta.params {
					let pname = self.pattern_to_binding_name(&p.0.name.0);
					params.push(
						self
							.ast
							.plain_formal_parameter(SPAN, self.binding_pattern(&pname)),
					);
				}
				let formal_params =
					self
						.ast
						.formal_parameters(SPAN, FormalParameterKind::FormalParameter, params, NONE);

				let js_val = self.emit_expr(body);
				let ret_expr = js_val.into_expression(self.ast, self.alloc);
				let mut body_stmts = self.ast.vec();
				body_stmts.push(self.ast.statement_return(SPAN, Some(ret_expr)));
				let fn_body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);

				Some(self.ast.class_element_method_definition(
					SPAN,
					MethodDefinitionType::MethodDefinition,
					self.ast.vec(),
					PropertyKey::StaticIdentifier(
						self.ast.alloc(self.ast.identifier_name(SPAN, method_name)),
					),
					self.ast.alloc(self.ast.function(
						SPAN,
						FunctionType::FunctionExpression,
						None::<BindingIdentifier<'a>>,
						false,
						false,
						false,
						NONE,
						NONE,
						formal_params,
						NONE,
						Some(fn_body),
					)),
					MethodDefinitionKind::Method,
					false,
					is_static,
					false,
					false,
					None,
				))
			}
			ImplMember::Let { meta, value, .. } => {
				let js_val = self.emit_expr(value);
				let init = js_val.into_expression(self.ast, self.alloc);
				let prop_name = self.pattern_to_binding_name(&meta.name.0);
				Some(
					self.ast.class_element_property_definition(
						SPAN,
						PropertyDefinitionType::PropertyDefinition,
						self.ast.vec(),
						PropertyKey::StaticIdentifier(
							self
								.ast
								.alloc(self.ast.identifier_name(SPAN, self.arena_str(&prop_name))),
						),
						NONE,
						Some(init),
						false,
						is_static,
						false,
						false,
						false,
						false,
						false,
						None,
					),
				)
			}
			ImplMember::ExternalFunc(..) | ImplMember::ExternalLet(..) => {
				// TODO: emit import from external module
				None
			}
		}
	}

	// ───────────────── enum emit ─────────────────

	fn emit_enum(
		&mut self,
		name: &ast::Ident,
		variants: &[Spanned<crate::ast::declaration::EnumVariant>],
		inner_members: &'a [Spanned<StructInnerMember>],
		export: bool,
	) -> Vec<Statement<'a>> {
		let enum_name = name.0.as_str();
		let mut stmts: Vec<Statement<'a>> = vec![];

		// const EnumName = {};
		let obj = self.ast.expression_object(SPAN, self.ast.vec());
		stmts.push(self.var_decl_stmt(VariableDeclarationKind::Const, enum_name, obj));

		// For each variant, add a factory function:
		// EnumName.VariantName = (field1, field2) => ({ _tag: 'VariantName', field1, field2 });
		for variant in variants {
			let variant_name = variant.0.name.0.as_str();
			let vfields = &variant.0.fields;

			if vfields.is_empty() {
				// Singleton variant: EnumName.Variant = Object.freeze({ _tag: 'Variant' });
				let mut props = self.ast.vec();
				props.push(
					self.ast.object_property_kind_object_property(
						SPAN,
						PropertyKind::Init,
						self
							.ast
							.property_key_static_identifier(SPAN, self.ast.atom("~tag")),
						self.string_lit(variant_name),
						false,
						false,
						false,
					),
				);
				let obj = self.ast.expression_object(SPAN, props);
				let frozen = self.method_call(self.ident_ref("Object"), "freeze", {
					let mut args = self.ast.vec();
					args.push(Argument::from(obj));
					args
				});
				let assign = self.ast.expression_assignment(
					SPAN,
					AssignmentOperator::Assign,
					self.assign_target_static_member(self.ident_ref(enum_name), variant_name),
					frozen,
				);
				stmts.push(self.ast.statement_expression(SPAN, assign));
			} else {
				// Factory: EnumName.Variant = (f1, f2) => ({ _tag: 'Variant', f1, f2 })
				let mut arrow_params = self.ast.vec();
				let mut obj_props = self.ast.vec();

				// _tag property
				obj_props.push(
					self.ast.object_property_kind_object_property(
						SPAN,
						PropertyKind::Init,
						self
							.ast
							.property_key_static_identifier(SPAN, self.ast.atom("~tag")),
						self.string_lit(variant_name),
						false,
						false,
						false,
					),
				);

				for f in vfields {
					let fname = f.0.name.0.as_str();
					arrow_params.push(
						self
							.ast
							.plain_formal_parameter(SPAN, self.binding_pattern(fname)),
					);
					obj_props.push(
						self.ast.object_property_kind_object_property(
							SPAN,
							PropertyKind::Init,
							self
								.ast
								.property_key_static_identifier(SPAN, self.ast.atom(fname)),
							self.ident_ref(fname),
							false,
							true, // shorthand
							false,
						),
					);
				}

				let params = self.ast.formal_parameters(
					SPAN,
					FormalParameterKind::ArrowFormalParameters,
					arrow_params,
					NONE,
				);

				let obj_expr = self.ast.expression_object(SPAN, obj_props);
				let arrow_body = self.ast.function_body(SPAN, self.ast.vec(), {
					let mut s = self.ast.vec();
					s.push(self.ast.statement_return(SPAN, Some(obj_expr)));
					s
				});

				let arrow = self
					.ast
					.expression_arrow_function(SPAN, true, false, NONE, params, NONE, arrow_body);

				let assign = self.ast.expression_assignment(
					SPAN,
					AssignmentOperator::Assign,
					self.assign_target_static_member(self.ident_ref(enum_name), variant_name),
					arrow,
				);
				stmts.push(self.ast.statement_expression(SPAN, assign));
			}
		}

		// Emit inner members as properties on the enum namespace object
		for inner in inner_members {
			match &inner.0 {
				StructInnerMember::Member(m) => {
					self.emit_impl_member_on_object(&m.0, enum_name, &mut stmts);
				}
				StructInnerMember::Namespace(members) => {
					for m in members {
						self.emit_impl_member_on_object(&m.0, enum_name, &mut stmts);
					}
				}
				StructInnerMember::Impl { members, .. } | StructInnerMember::ImplMut(members) => {
					for m in members {
						self.emit_impl_member_on_object(&m.0, enum_name, &mut stmts);
					}
				}
			}
		}

		if export {
			// Wrap the first statement (const declaration) in export
			if let Some(first) = stmts.first_mut() {
				let orig = std::mem::replace(first, self.ast.statement_empty(SPAN));
				*first = self.export_stmt(orig);
			}
		}

		stmts
	}

	fn emit_impl_member_on_object(
		&mut self,
		member: &'a ImplMember,
		obj_name: &str,
		stmts: &mut Vec<Statement<'a>>,
	) {
		match member {
			ImplMember::Func { meta, body, .. } => {
				let method_name = meta.name.0.as_str();
				let mut params = self.ast.vec();
				for p in &meta.params {
					let pname = self.pattern_to_binding_name(&p.0.name.0);
					params.push(
						self
							.ast
							.plain_formal_parameter(SPAN, self.binding_pattern(&pname)),
					);
				}
				let formal_params = self.ast.formal_parameters(
					SPAN,
					FormalParameterKind::ArrowFormalParameters,
					params,
					NONE,
				);

				let js_val = self.emit_expr(body);
				let ret = js_val.into_expression(self.ast, self.alloc);
				let fn_body = self.ast.function_body(SPAN, self.ast.vec(), {
					let mut s = self.ast.vec();
					s.push(self.ast.statement_return(SPAN, Some(ret)));
					s
				});

				let arrow =
					self
						.ast
						.expression_arrow_function(SPAN, true, false, NONE, formal_params, NONE, fn_body);

				let assign = self.ast.expression_assignment(
					SPAN,
					AssignmentOperator::Assign,
					self.assign_target_static_member(self.ident_ref(obj_name), method_name),
					arrow,
				);
				stmts.push(self.ast.statement_expression(SPAN, assign));
			}
			ImplMember::Let { meta, value, .. } => {
				let name = self.pattern_to_binding_name(&meta.name.0);
				let js_val = self.emit_expr(value);
				let init = js_val.into_expression(self.ast, self.alloc);
				let assign = self.ast.expression_assignment(
					SPAN,
					AssignmentOperator::Assign,
					self.assign_target_static_member(self.ident_ref(obj_name), &name),
					init,
				);
				stmts.push(self.ast.statement_expression(SPAN, assign));
			}
			ImplMember::ExternalFunc(..) | ImplMember::ExternalLet(..) => {
				// TODO
			}
		}
	}

	// ───────────────── namespace ─────────────────

	fn emit_namespace(
		&mut self,
		name: &ast::Ident,
		members: &'a [Spanned<ImplMember>],
		export: bool,
	) -> Vec<Statement<'a>> {
		let ns_name = name.0.as_str();
		let mut stmts: Vec<Statement<'a>> = vec![];

		let obj = self.ast.expression_object(SPAN, self.ast.vec());
		stmts.push(self.var_decl_stmt(VariableDeclarationKind::Const, ns_name, obj));

		for m in members {
			self.emit_impl_member_on_object(&m.0, ns_name, &mut stmts);
		}

		if export && let Some(first) = stmts.first_mut() {
			let orig = std::mem::replace(first, self.ast.statement_empty(SPAN));
			*first = self.export_stmt(orig);
		}

		stmts
	}

	// ───────────────── impl blocks ─────────────────

	fn emit_impl(
		&mut self,
		type_: &Spanned<crate::ast::types::Type>,
		members: &'a [Spanned<ImplMember>],
	) -> Vec<Statement<'a>> {
		// Extract the type name for prototype patching
		let type_name = match &type_.0 {
			crate::ast::types::Type::Reference { name, .. } => name.0.as_str(),
			_ => return vec![],
		};

		let mut stmts: Vec<Statement<'a>> = vec![];
		for m in members {
			if let ImplMember::Func { meta, body, .. } = &m.0 {
				let method_name = meta.name.0.as_str();
				let mut params = self.ast.vec();
				for p in &meta.params {
					let pname = self.pattern_to_binding_name(&p.0.name.0);
					params.push(
						self
							.ast
							.plain_formal_parameter(SPAN, self.binding_pattern(&pname)),
					);
				}
				let formal_params = self.ast.formal_parameters(
					SPAN,
					FormalParameterKind::ArrowFormalParameters,
					params,
					NONE,
				);

				let js_val = self.emit_expr(body);
				let ret = js_val.into_expression(self.ast, self.alloc);
				let fn_body = self.ast.function_body(SPAN, self.ast.vec(), {
					let mut s = self.ast.vec();
					s.push(self.ast.statement_return(SPAN, Some(ret)));
					s
				});

				let func_expr = self.ast.expression_function(
					SPAN,
					FunctionType::FunctionExpression,
					None::<BindingIdentifier<'a>>,
					false,
					false,
					false,
					NONE,
					NONE,
					formal_params,
					NONE,
					Some(fn_body),
				);

				// TypeName.prototype.methodName = function(...) { ... };
				let target = self.member(
					self.member(self.ident_ref(type_name), "prototype"),
					method_name,
				);
				let assign = self.ast.expression_assignment(
					SPAN,
					AssignmentOperator::Assign,
					match target {
						Expression::StaticMemberExpression(m) => AssignmentTarget::StaticMemberExpression(m),
						_ => unreachable!(),
					},
					func_expr,
				);
				stmts.push(self.ast.statement_expression(SPAN, assign));
			}
		}
		stmts
	}

	// ───────────────── func emit ─────────────────

	fn emit_func(
		&mut self,
		meta: &'a FuncDeclaration,
		body: &'a Spanned<Expr>,
		export: bool,
	) -> Statement<'a> {
		let func_name = meta.name.0.as_str();
		let mut params = self.ast.vec();
		for p in &meta.params {
			let pname = self.pattern_to_binding_name(&p.0.name.0);
			let param = self
				.ast
				.plain_formal_parameter(SPAN, self.binding_pattern(&pname));
			params.push(param);
		}
		let formal_params =
			self
				.ast
				.formal_parameters(SPAN, FormalParameterKind::FormalParameter, params, NONE);

		let js_val = self.emit_expr(body);
		let ret_expr = js_val.into_expression(self.ast, self.alloc);
		let mut body_stmts = self.ast.vec();
		body_stmts.push(self.ast.statement_return(SPAN, Some(ret_expr)));
		let fn_body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);

		let func_decl = self.ast.declaration_function(
			SPAN,
			FunctionType::FunctionDeclaration,
			Some(self.ast.binding_identifier(SPAN, func_name)),
			false, // generator
			false, // async
			false, // declare
			NONE,
			NONE,
			formal_params,
			NONE,
			Some(fn_body),
		);

		let stmt = Statement::from(func_decl);
		if export { self.export_stmt(stmt) } else { stmt }
	}

	// ───────────────── external declarations ─────────────────

	fn emit_external_let(
		&mut self,
		meta: &LetDeclaration,
		visibility: Option<Visibility>,
		outer_name: Option<&str>,
	) -> Vec<Statement<'a>> {
		let name = self.pattern_to_binding_name(&meta.name.0);
		let export_name = external_export_name(outer_name, &name);

		if let Some(ext_path) = self.source_path.and_then(find_external_module) {
			let rel = format!("./{}", ext_path.file_name().unwrap().to_string_lossy());
			let mut specifiers = self.ast.vec();
			specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
				self.ast.alloc(
					self.ast.import_specifier(
						SPAN,
						self
							.ast
							.module_export_name_identifier_name(SPAN, self.arena_str(&export_name)),
						self.ast.binding_identifier(SPAN, self.arena_str(&name)),
						ImportOrExportKind::Value,
					),
				),
			));
			let import = self.ast.module_declaration_import_declaration(
				SPAN,
				Some(specifiers),
				self.ast.string_literal(SPAN, self.arena_str(&rel), None),
				None,
				NONE,
				ImportOrExportKind::Value,
			);
			let import_stmt = Statement::from(import);
			let is_export = matches!(visibility, Some(Visibility::Public));
			if is_export {
				vec![import_stmt, self.re_export_stmt(&name)]
			} else {
				vec![import_stmt]
			}
		} else {
			vec![]
		}
	}

	fn emit_external_func(
		&mut self,
		meta: &FuncDeclaration,
		visibility: Option<Visibility>,
		outer_name: Option<&str>,
	) -> Vec<Statement<'a>> {
		let name = meta.name.0.as_str();
		let export_name = external_export_name(outer_name, name);

		if let Some(ext_path) = self.source_path.and_then(find_external_module) {
			let rel = format!("./{}", ext_path.file_name().unwrap().to_string_lossy());
			let mut specifiers = self.ast.vec();
			specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
				self.ast.alloc(
					self.ast.import_specifier(
						SPAN,
						self
							.ast
							.module_export_name_identifier_name(SPAN, self.arena_str(&export_name)),
						self.ast.binding_identifier(SPAN, self.arena_str(name)),
						ImportOrExportKind::Value,
					),
				),
			));
			let import = self.ast.module_declaration_import_declaration(
				SPAN,
				Some(specifiers),
				self.ast.string_literal(SPAN, self.arena_str(&rel), None),
				None,
				NONE,
				ImportOrExportKind::Value,
			);
			let import_stmt = Statement::from(import);
			let is_export = matches!(visibility, Some(Visibility::Public));
			if is_export {
				vec![import_stmt, self.re_export_stmt(name)]
			} else {
				vec![import_stmt]
			}
		} else {
			vec![]
		}
	}

	// ───────────────── statement emit ─────────────────

	pub fn emit_statement(&mut self, stmt: &'a NymphStatement) -> Statement<'a> {
		match stmt {
			NymphStatement::Expr(e) => {
				let js = self.emit_expr(e);
				if js.stmts.is_empty() {
					self.ast.statement_expression(SPAN, js.expr)
				} else {
					let expr = js.into_expression(self.ast, self.alloc);
					self.ast.statement_expression(SPAN, expr)
				}
			}
			NymphStatement::Let { meta, value } => {
				let js = self.emit_expr(value);
				let init = js.into_expression(self.ast, self.alloc);
				let kind = if meta.mutable {
					VariableDeclarationKind::Let
				} else {
					VariableDeclarationKind::Const
				};
				let name = self.pattern_to_binding_name(&meta.name.0);
				self.var_decl_stmt(kind, &name, init)
			}
		}
	}

	// ───────────────── expression emit ─────────────────

	pub fn emit_expr(&mut self, expr: &'a Spanned<Expr>) -> JsValue<'a> {
		self.emit_expr_inner(&expr.0)
	}

	fn emit_expr_inner(&mut self, expr: &'a Expr) -> JsValue<'a> {
		match expr {
			Expr::Int(Spanned(n, _)) => JsValue {
				stmts: self.ast.vec(),
				expr: self.number_lit(*n as f64),
			},
			Expr::Float(Spanned(f, _)) => JsValue {
				stmts: self.ast.vec(),
				expr: self.number_lit(f.into_inner()),
			},
			Expr::Char(Spanned(c, _)) => JsValue {
				stmts: self.ast.vec(),
				expr: self.number_lit(*c as u32 as f64),
			},
			Expr::String(parts) => {
				let js_expr = self.emit_string_parts(parts);
				JsValue {
					stmts: self.ast.vec(),
					expr: js_expr,
				}
			}
			Expr::Boolean(Spanned(b, _)) => JsValue {
				stmts: self.ast.vec(),
				expr: self.bool_lit(*b),
			},
			Expr::Identifier(ident) => JsValue {
				stmts: self.ast.vec(),
				expr: self.ident_ref(ident.0.as_str()),
			},
			Expr::List(items) => {
				let mut elems = self.ast.vec();
				for item in items {
					match &item.0 {
						ListItem::Expr(e) => {
							let v = self.emit_expr(e);
							let e = v.into_expression(self.ast, self.alloc);
							elems.push(ArrayExpressionElement::from(e));
						}
						ListItem::Spread(e) => {
							let v = self.emit_expr(e);
							let e = v.into_expression(self.ast, self.alloc);
							elems.push(ArrayExpressionElement::SpreadElement(
								self.ast.alloc(self.ast.spread_element(SPAN, e)),
							));
						}
					}
				}
				JsValue {
					stmts: self.ast.vec(),
					expr: self.ast.expression_array(SPAN, elems),
				}
			}
			Expr::Tuple(items) => {
				let mut elems = self.ast.vec();
				for item in items {
					match &item.0 {
						ListItem::Expr(e) => {
							let v = self.emit_expr(e);
							let e = v.into_expression(self.ast, self.alloc);
							elems.push(ArrayExpressionElement::from(e));
						}
						ListItem::Spread(e) => {
							let v = self.emit_expr(e);
							let e = v.into_expression(self.ast, self.alloc);
							elems.push(ArrayExpressionElement::SpreadElement(
								self.ast.alloc(self.ast.spread_element(SPAN, e)),
							));
						}
					}
				}
				JsValue {
					stmts: self.ast.vec(),
					expr: self.ast.expression_array(SPAN, elems),
				}
			}
			Expr::Map(entries) => {
				let mut pairs = self.ast.vec();
				for entry in entries {
					match &entry.0 {
						MapEntry::Expr(k, v) => {
							let kv = self.emit_expr(k);
							let ke = kv.into_expression(self.ast, self.alloc);
							let vv = self.emit_expr(v);
							let ve = vv.into_expression(self.ast, self.alloc);
							let pair = self.ast.expression_array(SPAN, {
								let mut a = self.ast.vec();
								a.push(ArrayExpressionElement::from(ke));
								a.push(ArrayExpressionElement::from(ve));
								a
							});
							pairs.push(ArrayExpressionElement::from(pair));
						}
						MapEntry::Spread(e) => {
							let v = self.emit_expr(e);
							let e = v.into_expression(self.ast, self.alloc);
							pairs.push(ArrayExpressionElement::SpreadElement(
								self.ast.alloc(self.ast.spread_element(SPAN, e)),
							));
						}
					}
				}
				// new Map([...entries])
				let entries_arr = self.ast.expression_array(SPAN, pairs);
				let mut args = self.ast.vec();
				args.push(Argument::from(entries_arr));
				JsValue {
					stmts: self.ast.vec(),
					expr: self
						.ast
						.expression_new(SPAN, self.ident_ref("Map"), NONE, args),
				}
			}
			Expr::Range(_) => {
				// TODO: emit range helper
				JsValue {
					stmts: self.ast.vec(),
					expr: self.ast.expression_array(SPAN, self.ast.vec()),
				}
			}
			Expr::Call {
				func,
				args,
				generics: _,
			} => {
				let is_constructor = match &func.0 {
					Expr::Identifier(ident) => self
						.ctx
						.lookup_type_ref(&ident.0)
						.is_some_and(|ty| matches!(ty, Type::Function { constructor: true, .. })),
					_ => false,
				};

				let callee_val = self.emit_expr(func);
				let callee = callee_val.into_expression(self.ast, self.alloc);
				let mut js_args = self.ast.vec();
				for arg in args {
					let a = &arg.0;
					let v = self.emit_expr(&a.value);
					let e = v.into_expression(self.ast, self.alloc);
					if a.spread {
						js_args.push(Argument::SpreadElement(
							self.ast.alloc(self.ast.spread_element(SPAN, e)),
						));
					} else {
						js_args.push(Argument::from(e));
					}
				}
				JsValue {
					stmts: self.ast.vec(),
					expr: if is_constructor {
						self.ast.expression_new(SPAN, callee, NONE, js_args)
					} else {
						self.call(callee, js_args)
					},
				}
			}
			Expr::MemberAccess {
				parent,
				member,
				optional,
			} => {
				let obj = self.emit_expr(parent);
				let obj_expr = obj.into_expression(self.ast, self.alloc);
				let prop = member.0.as_str();
				if *optional {
					JsValue {
						stmts: self.ast.vec(),
						expr: Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
							SPAN,
							obj_expr,
							self.ast.identifier_name(SPAN, prop),
							true,
						)),
					}
				} else {
					JsValue {
						stmts: self.ast.vec(),
						expr: self.member(obj_expr, prop),
					}
				}
			}
			Expr::IndexAccess {
				parent,
				index,
				optional: _,
			} => {
				let obj = self.emit_expr(parent);
				let obj_expr = obj.into_expression(self.ast, self.alloc);
				let idx = self.emit_expr(index);
				let idx_expr = idx.into_expression(self.ast, self.alloc);
				JsValue {
					stmts: self.ast.vec(),
					expr: Expression::ComputedMemberExpression(
						self
							.ast
							.alloc_computed_member_expression(SPAN, obj_expr, idx_expr, false),
					),
				}
			}
			Expr::Closure {
				params,
				body,
				generics: _,
				return_type: _,
			} => {
				let mut arrow_params = self.ast.vec();
				for p in params {
					let pname = self.pattern_to_binding_name(&p.0.name.0);
					arrow_params.push(
						self
							.ast
							.plain_formal_parameter(SPAN, self.binding_pattern(&pname)),
					);
				}
				let formal = self.ast.formal_parameters(
					SPAN,
					FormalParameterKind::ArrowFormalParameters,
					arrow_params,
					NONE,
				);
				let js_body = self.emit_expr(body);
				let body_expr = js_body.into_expression(self.ast, self.alloc);
				let fn_body = self.ast.function_body(SPAN, self.ast.vec(), {
					let mut s = self.ast.vec();
					s.push(self.ast.statement_return(SPAN, Some(body_expr)));
					s
				});
				JsValue {
					stmts: self.ast.vec(),
					expr: self
						.ast
						.expression_arrow_function(SPAN, true, false, NONE, formal, NONE, fn_body),
				}
			}
			Expr::PrefixOp { op, value } => {
				let val = self.emit_expr(value);
				let val_expr = val.into_expression(self.ast, self.alloc);
				let method = prefix_op_method(*op);
				JsValue {
					stmts: self.ast.vec(),
					expr: self.method_call(val_expr, method, self.ast.vec()),
				}
			}
			Expr::PostfixOp { op, value } => {
				let val = self.emit_expr(value);
				let val_expr = val.into_expression(self.ast, self.alloc);
				let method = postfix_op_method(*op);
				JsValue {
					stmts: self.ast.vec(),
					expr: self.method_call(val_expr, method, self.ast.vec()),
				}
			}
			Expr::BinaryOp { lhs, op, rhs } => self.emit_binary_op(lhs, *op, rhs),
			Expr::TypeOp { lhs, op: _, rhs: _ } => {
				// `expr as Type` → expr.into()
				let val = self.emit_expr(lhs);
				let val_expr = val.into_expression(self.ast, self.alloc);
				JsValue {
					stmts: self.ast.vec(),
					expr: self.method_call(val_expr, "into", self.ast.vec()),
				}
			}
			Expr::PatternOp { lhs, op, rhs } => self.emit_pattern_op(lhs, *op, &rhs.0),
			Expr::AssignOp { lhs, op, rhs } => self.emit_assign_op(lhs, *op, rhs),
			Expr::Return { value, label: _ } => {
				// Return is always in statement context for the transpiler
				match value {
					Some(v) => {
						let val = self.emit_expr(v);
						let expr = val.into_expression(self.ast, self.alloc);
						JsValue {
							stmts: {
								let mut s = self.ast.vec();
								s.push(self.ast.statement_return(SPAN, Some(expr)));
								s
							},
							expr: self.undefined(),
						}
					}
					None => JsValue {
						stmts: {
							let mut s = self.ast.vec();
							s.push(self.ast.statement_return(SPAN, None));
							s
						},
						expr: self.undefined(),
					},
				}
			}
			Expr::Break { value: _, label } => JsValue {
				stmts: {
					let mut s = self.ast.vec();
					s.push(
						self.ast.statement_break(
							SPAN,
							label
								.as_ref()
								.map(|Spanned(it, _)| self.ast.label_identifier(SPAN, self.arena_str(it))),
						),
					);
					s
				},
				expr: self.undefined(),
			},
			Expr::Continue { label } => JsValue {
				stmts: {
					let mut s = self.ast.vec();
					s.push(
						self.ast.statement_continue(
							SPAN,
							label
								.as_ref()
								.map(|Spanned(it, _)| self.ast.label_identifier(SPAN, self.arena_str(it))),
						),
					);
					s
				},
				expr: self.undefined(),
			},
			Expr::While {
				condition,
				body,
				label: _,
			} => {
				let cond = self.emit_expr(condition);
				let cond_expr = cond.into_expression(self.ast, self.alloc);
				let body_val = self.emit_expr(body);
				let body_expr = body_val.into_expression(self.ast, self.alloc);
				let body_stmt = self.ast.statement_expression(SPAN, body_expr);
				JsValue {
					stmts: {
						let mut s = self.ast.vec();
						s.push(self.ast.statement_while(SPAN, cond_expr, body_stmt));
						s
					},
					expr: self.undefined(),
				}
			}
			Expr::For {
				variable,
				iterable,
				body,
				label,
			} => {
				let var_name = self.pattern_to_binding_name(&variable.0);
				let iter_val = self.emit_expr(iterable);
				let iter_expr = iter_val.into_expression(self.ast, self.alloc);
				let body_val = self.emit_expr(body);
				let body_expr = body_val.into_expression(self.ast, self.alloc);
				let body_stmt = self.ast.statement_expression(SPAN, body_expr);

				let left = ForStatementLeft::VariableDeclaration(self.ast.alloc_variable_declaration(
					SPAN,
					VariableDeclarationKind::Const,
					self.ast.vec1(self.ast.variable_declarator(
						SPAN,
						VariableDeclarationKind::Const,
						self.binding_pattern(&var_name),
						NONE,
						None,
						false,
					)),
					false,
				));

				let for_stmt = self
					.ast
					.statement_for_of(SPAN, false, left, iter_expr, body_stmt);

				JsValue {
					stmts: {
						let mut s = self.ast.vec();
						s.push(if let Some(label) = label {
							self.ast.statement_labeled(
								SPAN,
								self
									.ast
									.label_identifier(SPAN, self.ast.atom(label.0.as_str())),
								for_stmt,
							)
						} else {
							for_stmt
						});
						s
					},
					expr: self.undefined(),
				}
			}
			Expr::If {
				condition,
				then,
				otherwise,
			} => self.emit_if(condition, then, otherwise.as_deref()),
			Expr::Match { value, arms } => self.emit_match(value, arms),
			Expr::This => JsValue {
				stmts: self.ast.vec(),
				expr: self.ast.expression_this(SPAN),
			},
			Expr::Placeholder => JsValue {
				stmts: self.ast.vec(),
				expr: self.undefined(),
			},
			Expr::Block { body, label: _ } => self.emit_block(body),
			Expr::Grouped(inner) => self.emit_expr(inner),
		}
	}

	// ───────────────── binary ops ─────────────────

	fn emit_binary_op(
		&mut self,
		lhs: &'a Spanned<Expr>,
		op: BinaryOperator,
		rhs: &'a Spanned<Expr>,
	) -> JsValue<'a> {
		match op {
			// Pipe: a |> f → f(a)
			BinaryOperator::Pipe => {
				let arg_val = self.emit_expr(lhs);
				let arg = arg_val.into_expression(self.ast, self.alloc);
				let func_val = self.emit_expr(rhs);
				let func = func_val.into_expression(self.ast, self.alloc);
				let mut args = self.ast.vec();
				args.push(Argument::from(arg));
				JsValue {
					stmts: self.ast.vec(),
					expr: self.call(func, args),
				}
			}
			// Unwrap: a ?? b → a.unwrap_or(() => b)
			BinaryOperator::Unwrap => {
				let lhs_val = self.emit_expr(lhs);
				let lhs_expr = lhs_val.into_expression(self.ast, self.alloc);
				let rhs_val = self.emit_expr(rhs);
				let rhs_expr = rhs_val.into_expression(self.ast, self.alloc);

				// Wrap RHS in thunk for lazy evaluation
				let thunk_body = self.ast.function_body(SPAN, self.ast.vec(), {
					let mut s = self.ast.vec();
					s.push(self.ast.statement_return(SPAN, Some(rhs_expr)));
					s
				});
				let thunk = self.ast.expression_arrow_function(
					SPAN,
					true,
					false,
					NONE,
					self.ast.formal_parameters(
						SPAN,
						FormalParameterKind::ArrowFormalParameters,
						self.ast.vec(),
						NONE,
					),
					NONE,
					thunk_body,
				);

				let mut args = self.ast.vec();
				args.push(Argument::from(thunk));

				JsValue {
					stmts: self.ast.vec(),
					expr: self.method_call(lhs_expr, "unwrap_or", args),
				}
			}
			// In: a in b → b.contains(a)
			BinaryOperator::In => {
				let a = self.emit_expr(lhs).into_expression(self.ast, self.alloc);
				let b = self.emit_expr(rhs).into_expression(self.ast, self.alloc);
				let mut args = self.ast.vec();
				args.push(Argument::from(a));
				JsValue {
					stmts: self.ast.vec(),
					expr: self.method_call(b, "contains", args),
				}
			}
			// NotIn: a !in b → !b.contains(a)
			BinaryOperator::NotIn => {
				let a = self.emit_expr(lhs).into_expression(self.ast, self.alloc);
				let b = self.emit_expr(rhs).into_expression(self.ast, self.alloc);
				let mut args = self.ast.vec();
				args.push(Argument::from(a));
				let contains = self.method_call(b, "contains", args);
				JsValue {
					stmts: self.ast.vec(),
					expr: self
						.ast
						.expression_unary(SPAN, UnaryOperator::LogicalNot, contains),
				}
			}
			// All other operators: a.method(b)
			_ => {
				let method = binary_op_method(op);
				let lhs_val = self.emit_expr(lhs);
				let lhs_expr = lhs_val.into_expression(self.ast, self.alloc);
				let rhs_val = self.emit_expr(rhs);
				let rhs_expr = rhs_val.into_expression(self.ast, self.alloc);
				let mut args = self.ast.vec();
				args.push(Argument::from(rhs_expr));
				JsValue {
					stmts: self.ast.vec(),
					expr: self.method_call(lhs_expr, method, args),
				}
			}
		}
	}

	// ───────────────── assignment ops ─────────────────

	fn emit_assign_op(
		&mut self,
		lhs: &'a Spanned<Expr>,
		op: AssignOperator,
		rhs: &'a Spanned<Expr>,
	) -> JsValue<'a> {
		let rhs_val = self.emit_expr(rhs);
		let rhs_expr = rhs_val.into_expression(self.ast, self.alloc);

		let final_rhs = if let Some(bin_op) = assign_op_to_binary(op) {
			// Compound assignment: lhs = lhs.method(rhs)
			let lhs_read = self.emit_expr(lhs);
			let lhs_read_expr = lhs_read.into_expression(self.ast, self.alloc);
			let method = binary_op_method(bin_op);
			let mut args = self.ast.vec();
			args.push(Argument::from(rhs_expr));
			self.method_call(lhs_read_expr, method, args)
		} else {
			rhs_expr
		};

		// Emit the LHS as an assignment target
		let target = self.emit_assignment_target(lhs);
		JsValue {
			stmts: self.ast.vec(),
			expr: self
				.ast
				.expression_assignment(SPAN, AssignmentOperator::Assign, target, final_rhs),
		}
	}

	fn emit_assignment_target(&mut self, expr: &'a Spanned<Expr>) -> AssignmentTarget<'a> {
		match &expr.0 {
			Expr::Identifier(ident) => self
				.ast
				.simple_assignment_target_assignment_target_identifier(SPAN, ident.0.as_str())
				.into(),
			Expr::MemberAccess {
				parent,
				member,
				optional: _,
			} => {
				let obj = self.emit_expr(parent);
				let obj_expr = obj.into_expression(self.ast, self.alloc);
				self.assign_target_static_member(obj_expr, member.0.as_str())
			}
			Expr::IndexAccess {
				parent,
				index,
				optional: _,
			} => {
				let obj = self.emit_expr(parent);
				let obj_expr = obj.into_expression(self.ast, self.alloc);
				let idx = self.emit_expr(index);
				let idx_expr = idx.into_expression(self.ast, self.alloc);
				self.assign_target_computed_member(obj_expr, idx_expr)
			}
			_ => {
				// Fallback: emit as expression and wrap
				self
					.ast
					.simple_assignment_target_assignment_target_identifier(SPAN, "_")
					.into()
			}
		}
	}

	// ───────────────── pattern ops (is / !is) ─────────────────

	fn emit_pattern_op(
		&mut self,
		lhs: &'a Spanned<Expr>,
		op: PatternOperator,
		pattern: &Pattern,
	) -> JsValue<'a> {
		let val = self.emit_expr(lhs);
		let val_expr = val.into_expression(self.ast, self.alloc);
		let tmp = self.gensym("pat");
		let mut stmts = self.ast.vec();
		stmts.push(self.var_decl_stmt(VariableDeclarationKind::Const, &tmp, val_expr));
		let check = self.emit_pattern_check(&tmp, pattern);
		let final_expr = match op {
			PatternOperator::Is => check,
			PatternOperator::NotIs => self
				.ast
				.expression_unary(SPAN, UnaryOperator::LogicalNot, check),
		};
		JsValue {
			stmts,
			expr: final_expr,
		}
	}

	/// Emit a boolean expression that checks if `var_name` matches `pattern`.
	fn emit_pattern_check(&mut self, var_name: &str, pattern: &Pattern) -> Expression<'a> {
		match pattern {
			Pattern::Int(Spanned(n, _)) => self.ast.expression_binary(
				SPAN,
				self.ident_ref(var_name),
				oxc_syntax::operator::BinaryOperator::StrictEquality,
				self.number_lit(*n as f64),
			),
			Pattern::Float(Spanned(f, _)) => self.ast.expression_binary(
				SPAN,
				self.ident_ref(var_name),
				oxc_syntax::operator::BinaryOperator::StrictEquality,
				self.number_lit(f.into_inner()),
			),
			Pattern::Char(Spanned(c, _)) => self.ast.expression_binary(
				SPAN,
				self.ident_ref(var_name),
				oxc_syntax::operator::BinaryOperator::StrictEquality,
				self.number_lit(*c as u32 as f64),
			),
			Pattern::Boolean(Spanned(b, _)) => self.ast.expression_binary(
				SPAN,
				self.ident_ref(var_name),
				oxc_syntax::operator::BinaryOperator::StrictEquality,
				self.bool_lit(*b),
			),
			Pattern::Placeholder => self.bool_lit(true),
			Pattern::Binding { name: _, inner } => self.emit_pattern_check(var_name, &inner.0),
			Pattern::Struct { path, fields: _ } => {
				// Check _tag for enum variant matching
				if let Some(last) = path.last() {
					self.ast.expression_binary(
						SPAN,
						self.member(self.ident_ref(var_name), "~tag"),
						oxc_syntax::operator::BinaryOperator::StrictEquality,
						self.string_lit(last.0.as_str()),
					)
				} else {
					self.bool_lit(true)
				}
			}
			Pattern::Grouped(inner) => self.emit_pattern_check(var_name, &inner.0),
			Pattern::Union(a, b) => {
				let check_a = self.emit_pattern_check(var_name, &a.0);
				let check_b = self.emit_pattern_check(var_name, &b.0);
				self
					.ast
					.expression_logical(SPAN, check_a, LogicalOperator::Or, check_b)
			}
			_ => {
				// TODO: more complex patterns (list, tuple, map, range, string)
				self.bool_lit(true)
			}
		}
	}

	// ───────────────── if expression ─────────────────

	fn emit_if(
		&mut self,
		condition: &'a Spanned<Expr>,
		then: &'a Spanned<Expr>,
		otherwise: Option<&'a Spanned<Expr>>,
	) -> JsValue<'a> {
		let cond = self.emit_expr(condition);
		let cond_expr = cond.into_expression(self.ast, self.alloc);
		let then_val = self.emit_expr(then);
		let else_val = otherwise.map(|e| self.emit_expr(e));

		// Optimization: simple ternary when both branches are pure expressions
		let then_simple = then_val.stmts.is_empty();
		let else_simple = else_val.as_ref().is_none_or(|v| v.stmts.is_empty());

		if then_simple && else_simple {
			let else_expr = else_val.map(|v| v.expr).unwrap_or_else(|| self.undefined());
			JsValue {
				stmts: self.ast.vec(),
				expr: self
					.ast
					.expression_conditional(SPAN, cond_expr, then_val.expr, else_expr),
			}
		} else {
			// Full IIFE with if/else statements
			let then_expr = then_val.into_expression(self.ast, self.alloc);
			let then_stmt = self.ast.statement_return(SPAN, Some(then_expr));
			let else_stmt = else_val.map(|v| {
				let e = v.into_expression(self.ast, self.alloc);
				self.ast.statement_return(SPAN, Some(e))
			});

			JsValue {
				stmts: self
					.ast
					.vec1(self.ast.statement_if(SPAN, cond_expr, then_stmt, else_stmt)),
				expr: self.undefined(),
			}
		}
	}

	// ───────────────── match expression ─────────────────

	fn emit_match(&mut self, value: &'a Spanned<Expr>, arms: &'a [MatchArm]) -> JsValue<'a> {
		let scrutinee = self.emit_expr(value);
		let scrutinee_expr = scrutinee.into_expression(self.ast, self.alloc);
		let tmp = self.gensym("match");

		let mut stmts = self.ast.vec();
		stmts.push(self.var_decl_stmt(VariableDeclarationKind::Const, &tmp, scrutinee_expr));

		// Build chained if/else
		let mut current_else: Option<Statement<'a>> = None;

		for arm in arms.iter().rev() {
			let check = self.emit_pattern_check(&tmp, &arm.pattern.0);

			// Evaluate guard if present
			let full_check = if let Some(guard) = &arm.guard {
				let guard_val = self.emit_expr(guard);
				let guard_expr = guard_val.into_expression(self.ast, self.alloc);
				self
					.ast
					.expression_logical(SPAN, check, LogicalOperator::And, guard_expr)
			} else {
				check
			};

			let body_val = self.emit_expr(&arm.body);
			let body_expr = body_val.into_expression(self.ast, self.alloc);
			let consequent = self.ast.statement_return(SPAN, Some(body_expr));

			let if_stmt = self
				.ast
				.statement_if(SPAN, full_check, consequent, current_else);
			current_else = Some(if_stmt);
		}

		if let Some(chain) = current_else {
			stmts.push(chain);
		}

		JsValue {
			stmts,
			expr: self.undefined(),
		}
	}

	// ───────────────── block expression ─────────────────

	fn emit_block(&mut self, body: &'a [Spanned<NymphStatement>]) -> JsValue<'a> {
		if body.is_empty() {
			return JsValue {
				stmts: self.ast.vec(),
				expr: self.undefined(),
			};
		}

		let mut stmts = self.ast.vec();
		let last_idx = body.len() - 1;

		for (i, stmt) in body.iter().enumerate() {
			if i == last_idx {
				match &stmt.0 {
					NymphStatement::Expr(e) => {
						let val = self.emit_expr(e);
						for s in val.stmts {
							stmts.push(s);
						}
						return JsValue {
							stmts,
							expr: val.expr,
						};
					}
					NymphStatement::Let { .. } => {
						stmts.push(self.emit_statement(&stmt.0));
						return JsValue {
							stmts,
							expr: self.undefined(),
						};
					}
				}
			} else {
				stmts.push(self.emit_statement(&stmt.0));
			}
		}

		JsValue {
			stmts,
			expr: self.undefined(),
		}
	}

	// ───────────────── string emit ─────────────────

	fn emit_string_parts(&mut self, parts: &'a [Spanned<StringPart>]) -> Expression<'a> {
		// String in Nymph is Uint8Array (UTF-8 encoded).
		// For now, emit as a helper call: __nymph_str("...")
		// The runtime will provide __nymph_str that converts to Uint8Array.
		let mut text = String::new();
		let mut has_interpolation = false;

		for part in parts {
			match &part.0 {
				StringPart::Text(t) => text.push_str(t.as_str()),
				StringPart::EscapeSequence(esc) => match esc {
					StringEscape::Backslash => text.push('\\'),
					StringEscape::Newline => text.push('\n'),
					StringEscape::Carriage => text.push('\r'),
					StringEscape::Tab => text.push('\t'),
					StringEscape::Interpolation => text.push_str("${"),
					StringEscape::Quote => text.push('"'),
					StringEscape::Unicode(c) => text.push(*c),
				},
				StringPart::InterpolatedExpr(_) => {
					has_interpolation = true;
				}
			}
		}

		if !has_interpolation {
			// Simple string: __nymph_str("text")
			let mut args = self.ast.vec();
			args.push(Argument::from(self.string_lit(&text)));
			self.call(self.ident_ref("__nymph_str"), args)
		} else {
			// Template literal with interpolation
			// For now, build concatenation: __nymph_str("part1" + String(expr) + "part2")
			let mut segments: Vec<Expression<'a>> = vec![];
			let mut current_text = String::new();

			for part in parts {
				match &part.0 {
					StringPart::Text(t) => current_text.push_str(t.as_str()),
					StringPart::EscapeSequence(esc) => match esc {
						StringEscape::Backslash => current_text.push('\\'),
						StringEscape::Newline => current_text.push('\n'),
						StringEscape::Carriage => current_text.push('\r'),
						StringEscape::Tab => current_text.push('\t'),
						StringEscape::Interpolation => current_text.push_str("${"),
						StringEscape::Quote => current_text.push('"'),
						StringEscape::Unicode(c) => current_text.push(*c),
					},
					StringPart::InterpolatedExpr(e) => {
						if !current_text.is_empty() {
							segments.push(self.string_lit(&current_text));
							current_text.clear();
						}
						let val = self.emit_expr(e);
						let expr = val.into_expression(self.ast, self.alloc);
						segments.push(expr);
					}
				}
			}

			if !current_text.is_empty() {
				segments.push(self.string_lit(&current_text));
			}

			// Concatenate all segments, then wrap in __nymph_str
			let concat = segments
				.into_iter()
				.reduce(|a, b| {
					self
						.ast
						.expression_binary(SPAN, a, oxc_syntax::operator::BinaryOperator::Addition, b)
				})
				.unwrap_or_else(|| self.string_lit(""));

			let mut args = self.ast.vec();
			args.push(Argument::from(concat));
			self.call(self.ident_ref("__nymph_str"), args)
		}
	}

	// ───────────────── helpers ─────────────────

	fn pattern_to_binding_name(&self, pat: &Pattern) -> String {
		match pat {
			Pattern::Binding { name, .. } => name.0.to_string(),
			Pattern::Struct { path, fields } if fields.is_empty() && path.len() == 1 => {
				path[0].0.to_string()
			}
			Pattern::Placeholder => "_".to_string(),
			_ => "_".to_string(),
		}
	}

	fn export_stmt(&self, stmt: Statement<'a>) -> Statement<'a> {
		let decl = match stmt {
			Statement::VariableDeclaration(d) => oxc_ast::ast::Declaration::VariableDeclaration(d),
			Statement::ClassDeclaration(d) => oxc_ast::ast::Declaration::ClassDeclaration(d),
			Statement::FunctionDeclaration(d) => oxc_ast::ast::Declaration::FunctionDeclaration(d),
			_ => return stmt,
		};
		let export = self.ast.module_declaration_export_named_declaration(
			SPAN,
			Some(decl),
			self.ast.vec(),
			None,
			ImportOrExportKind::Value,
			NONE,
		);
		Statement::from(export)
	}

	fn re_export_stmt(&self, name: &str) -> Statement<'a> {
		let arena_name = self.arena_str(name);
		let mut specifiers = self.ast.vec();
		specifiers.push(
			self.ast.export_specifier(
				SPAN,
				self
					.ast
					.module_export_name_identifier_name(SPAN, arena_name),
				self
					.ast
					.module_export_name_identifier_name(SPAN, arena_name),
				ImportOrExportKind::Value,
			),
		);
		let export = self.ast.module_declaration_export_named_declaration(
			SPAN,
			Option::<oxc_ast::ast::Declaration<'a>>::None,
			specifiers,
			None,
			ImportOrExportKind::Value,
			NONE,
		);
		Statement::from(export)
	}
}
