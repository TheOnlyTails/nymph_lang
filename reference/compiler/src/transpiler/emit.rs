use std::path::Path;

use oxc::{
	allocator::{Allocator, Vec as OxcVec},
	ast::{AstBuilder, NONE, ast::*},
	span::SPAN,
};

use crate::{
	ast::{
		self, Spanned,
		declaration::{
			Declaration, EnumVariant, FuncDeclaration, ImplMember, LetDeclaration, Module,
			StructInnerMember, Visibility,
		},
		expr::{
			Expr, ListItem, MapEntry, MatchArm, Pattern, Statement as NymphStatement, StringEscape,
			StringPart, anonymous_params, rewrite_anonymous_params,
		},
		ops::{AssignOperator, BinaryOperator, PatternOperator},
	},
	transpiler::{
		external::{bundled_external_module_name, find_external_module},
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

	fn object_property(&self, name: &str, value: Expression<'a>) -> ObjectPropertyKind<'a> {
		self.ast.object_property_kind_object_property(
			SPAN,
			PropertyKind::Init,
			self
				.ast
				.property_key_static_identifier(SPAN, self.ast.ident(self.arena_str(name))),
			value,
			false,
			false,
			false,
		)
	}

	fn object_string_property(&self, name: &str, value: Expression<'a>) -> ObjectPropertyKind<'a> {
		self.ast.object_property_kind_object_property(
			SPAN,
			PropertyKind::Init,
			self.string_lit(name).into(),
			value,
			false,
			false,
			false,
		)
	}

	fn function_expression(&self, params: &[&str], body_expr: Expression<'a>) -> Expression<'a> {
		let mut js_params = self.ast.vec();
		for param in params {
			js_params.push(
				self
					.ast
					.plain_formal_parameter(SPAN, self.binding_pattern(param)),
			);
		}

		let formal_params =
			self
				.ast
				.formal_parameters(SPAN, FormalParameterKind::FormalParameter, js_params, NONE);
		let body = self.ast.function_body(SPAN, self.ast.vec(), {
			let mut stmts = self.ast.vec();
			stmts.push(self.ast.statement_return(SPAN, Some(body_expr)));
			stmts
		});

		self.ast.expression_function(
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
			Some(body),
		)
	}

	fn bound_object(&self, tag: &str, value: Option<Expression<'a>>) -> Expression<'a> {
		let mut props = self.ast.vec();
		props.push(self.object_string_property("~tag", self.string_lit(tag)));
		if let Some(value) = value {
			props.push(self.object_property("value", value));
		}
		self.ast.expression_object(SPAN, props)
	}

	fn nymph_string(&self, value: Expression<'a>) -> Expression<'a> {
		let mut args = self.ast.vec();
		args.push(Argument::from(value));
		self.call(self.ident_ref("__nymph_str"), args)
	}

	fn concat_expressions(&self, parts: Vec<Expression<'a>>) -> Expression<'a> {
		parts
			.into_iter()
			.reduce(|left, right| {
				self.ast.expression_binary(
					SPAN,
					left,
					oxc::syntax::operator::BinaryOperator::Addition,
					right,
				)
			})
			.unwrap_or_else(|| self.string_lit(""))
	}

	fn var_decl_stmt(
		&self,
		kind: VariableDeclarationKind,
		name: &str,
		init: Expression<'a>,
	) -> Statement<'a> {
		self.var_decl_stmt_pat(kind, self.binding_pattern(name), init)
	}

	fn var_decl_stmt_pat(
		&self,
		kind: VariableDeclarationKind,
		pat: BindingPattern<'a>,
		init: Expression<'a>,
	) -> Statement<'a> {
		let declarator = self
			.ast
			.variable_declarator(SPAN, kind, pat, NONE, Some(init), false);
		let decl = self
			.ast
			.variable_declaration(SPAN, kind, self.ast.vec1(declarator), false);
		Statement::from(oxc::ast::ast::Declaration::VariableDeclaration(
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
				let pat = self.emit_pattern_binding(&meta.name.0);
				let stmt = self.var_decl_stmt_pat(kind, pat, expr);
				if is_export {
					vec![self.export_stmt(stmt)]
				} else {
					vec![stmt]
				}
			}

			Declaration::ExternalLet(visibility, external_name, meta) => {
				self.emit_external_let(meta, *visibility, external_name)
			}

			Declaration::Func {
				visibility,
				meta,
				body,
			} => {
				let func_stmt = self.emit_func(meta, body, matches!(visibility, Some(Visibility::Public)));
				vec![func_stmt]
			}

			Declaration::ExternalFunc(visibility, external_name, meta) => {
				self.emit_external_func(meta, *visibility, external_name)
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
		self.emit_class_declaration(name.0.as_str(), fields, inner_members, export)
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
		let mut hoisted: Vec<Statement<'a>> = vec![];

		for inner in inner_members {
			match &inner.0 {
				StructInnerMember::Member(m) => {
					let (extra, el) = self.emit_impl_member_as_class_element(&m.0, false);
					hoisted.extend(extra);
					if let Some(el) = el {
						elements.push(el);
					}
				}
				StructInnerMember::Namespace(members) => {
					for m in members {
						let (extra, el) = self.emit_impl_member_as_class_element(&m.0, true);
						hoisted.extend(extra);
						if let Some(el) = el {
							elements.push(el);
						}
					}
				}
				StructInnerMember::Impl { members, .. } | StructInnerMember::ImplMut(members) => {
					for m in members {
						let (extra, el) = self.emit_impl_member_as_class_element(&m.0, false);
						hoisted.extend(extra);
						if let Some(el) = el {
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
		let mut result = hoisted;
		if export {
			result.push(self.export_stmt(stmt));
		} else {
			result.push(stmt);
		}
		result
	}

	/// Emit an `ImplMember` as a class element (for structs).
	///
	/// Returns `(hoisted_stmts, class_element)`. The hoisted statements (e.g.
	/// import declarations for external functions) must be placed *before* the
	/// class declaration.
	fn emit_impl_member_as_class_element(
		&mut self,
		member: &'a ImplMember,
		is_static: bool,
	) -> (Vec<Statement<'a>>, Option<ClassElement<'a>>) {
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

				(
					vec![],
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
					)),
				)
			}
			ImplMember::Let { meta, value, .. } => {
				let js_val = self.emit_expr(value);
				let init = js_val.into_expression(self.ast, self.alloc);
				let prop_name = self.pattern_to_binding_name(&meta.name.0);
				(
					vec![],
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
					),
				)
			}
			ImplMember::ExternalFunc(_, external_name, meta) => {
				self.emit_external_impl_func_as_class_element(meta, external_name, is_static)
			}
			ImplMember::ExternalLet(_, external_name, meta) => {
				self.emit_external_impl_let_as_class_element(meta, external_name, is_static)
			}
		}
	}

	/// Emit an external function inside an impl block as a class method.
	///
	/// Generates:
	/// 1. `import { externalName as __ext$N } from "./file.ext";` (hoisted)
	/// 2. A class method `methodName(...args) { return __ext$N(this, ...args); }`
	fn emit_external_impl_func_as_class_element(
		&mut self,
		meta: &'a FuncDeclaration,
		external_name: &str,
		is_static: bool,
	) -> (Vec<Statement<'a>>, Option<ClassElement<'a>>) {
		let Some(ext_path) = self.source_path.and_then(find_external_module) else {
			return (vec![], None);
		};

		let method_name = meta.name.0.as_str();
		let import_alias = self.gensym("ext");
		let import_alias_str = self.arena_str(&import_alias);

		// import { externalName as __ext$N } from "./file.ext";
		let rel = format!(
			"./{}",
			bundled_external_module_name(&ext_path)
				.unwrap_or_else(|| { ext_path.file_name().unwrap().to_string_lossy().into_owned() })
		);
		let mut specifiers = self.ast.vec();
		specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
			self.ast.alloc(
				self.ast.import_specifier(
					SPAN,
					self
						.ast
						.module_export_name_identifier_name(SPAN, self.arena_str(external_name)),
					self.ast.binding_identifier(SPAN, import_alias_str),
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

		// Build wrapper method: methodName(...params) { return __ext$N(this, ...params); }
		let mut params = self.ast.vec();
		let mut call_args = self.ast.vec();

		// First argument to the external function: `this`
		if !is_static {
			call_args.push(Argument::from(self.ast.expression_this(SPAN)));
		}

		for p in &meta.params {
			let pname = self.pattern_to_binding_name(&p.0.name.0);
			params.push(
				self
					.ast
					.plain_formal_parameter(SPAN, self.binding_pattern(&pname)),
			);
			call_args.push(Argument::from(self.ident_ref(&pname)));
		}

		let formal_params =
			self
				.ast
				.formal_parameters(SPAN, FormalParameterKind::FormalParameter, params, NONE);

		let call_expr = self.call(self.ident_ref(import_alias_str), call_args);
		let mut body_stmts = self.ast.vec();
		body_stmts.push(self.ast.statement_return(SPAN, Some(call_expr)));
		let fn_body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);

		let class_el = self.ast.class_element_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			self.ast.vec(),
			PropertyKey::StaticIdentifier(self.ast.alloc(self.ast.identifier_name(SPAN, method_name))),
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
		);

		(vec![import_stmt], Some(class_el))
	}

	/// Emit an external let inside an impl block as a class property.
	fn emit_external_impl_let_as_class_element(
		&mut self,
		meta: &LetDeclaration,
		external_name: &str,
		is_static: bool,
	) -> (Vec<Statement<'a>>, Option<ClassElement<'a>>) {
		let Some(ext_path) = self.source_path.and_then(find_external_module) else {
			return (vec![], None);
		};

		let prop_name = self.pattern_to_binding_name(&meta.name.0);
		let import_alias = self.gensym("ext");
		let import_alias_str = self.arena_str(&import_alias);

		let rel = format!(
			"./{}",
			bundled_external_module_name(&ext_path)
				.unwrap_or_else(|| { ext_path.file_name().unwrap().to_string_lossy().into_owned() })
		);
		let mut specifiers = self.ast.vec();
		specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
			self.ast.alloc(
				self.ast.import_specifier(
					SPAN,
					self
						.ast
						.module_export_name_identifier_name(SPAN, self.arena_str(external_name)),
					self.ast.binding_identifier(SPAN, import_alias_str),
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

		let class_el = self.ast.class_element_property_definition(
			SPAN,
			PropertyDefinitionType::PropertyDefinition,
			self.ast.vec(),
			PropertyKey::StaticIdentifier(
				self
					.ast
					.alloc(self.ast.identifier_name(SPAN, self.arena_str(&prop_name))),
			),
			NONE,
			Some(self.ident_ref(import_alias_str)),
			false,
			is_static,
			false,
			false,
			false,
			false,
			false,
			None,
		);

		(vec![import_stmt], Some(class_el))
	}

	// ───────────────── enum emit ─────────────────

	fn emit_enum(
		&mut self,
		name: &ast::Ident,
		variants: &[Spanned<EnumVariant>],
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
				props.push(self.ast.object_property_kind_object_property(
					SPAN,
					PropertyKind::Init,
					self.string_lit("~tag").into(),
					self.string_lit(variant_name),
					false,
					false,
					false,
				));
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
				obj_props.push(self.ast.object_property_kind_object_property(
					SPAN,
					PropertyKind::Init,
					self.string_lit("~tag").into(),
					self.string_lit(variant_name),
					false,
					false,
					false,
				));

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
								.property_key_static_identifier(SPAN, self.ast.ident(fname)),
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
					.expression_arrow_function(SPAN, false, false, NONE, params, NONE, arrow_body);

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

				let arrow = self.ast.expression_arrow_function(
					SPAN,
					false,
					false,
					NONE,
					formal_params,
					NONE,
					fn_body,
				);

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
			ImplMember::ExternalFunc(_, external_name, meta) => {
				self.emit_external_impl_func_on_object(meta, external_name, obj_name, stmts);
			}
			ImplMember::ExternalLet(_, external_name, meta) => {
				self.emit_external_impl_let_on_object(meta, external_name, obj_name, stmts);
			}
		}
	}

	/// Emit an external function as a property on a namespace/enum object.
	///
	/// Generates:
	/// 1. `import { externalName as __ext$N } from "./file.ext";`
	/// 2. `ObjName.methodName = __ext$N;`
	fn emit_external_impl_func_on_object(
		&mut self,
		meta: &FuncDeclaration,
		external_name: &str,
		obj_name: &str,
		stmts: &mut Vec<Statement<'a>>,
	) {
		let Some(ext_path) = self.source_path.and_then(find_external_module) else {
			return;
		};

		let method_name = meta.name.0.as_str();
		let import_alias = self.gensym("ext");
		let import_alias_str = self.arena_str(&import_alias);

		let rel = format!(
			"./{}",
			bundled_external_module_name(&ext_path)
				.unwrap_or_else(|| { ext_path.file_name().unwrap().to_string_lossy().into_owned() })
		);
		let mut specifiers = self.ast.vec();
		specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
			self.ast.alloc(
				self.ast.import_specifier(
					SPAN,
					self
						.ast
						.module_export_name_identifier_name(SPAN, self.arena_str(external_name)),
					self.ast.binding_identifier(SPAN, import_alias_str),
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
		stmts.push(Statement::from(import));

		let assign = self.ast.expression_assignment(
			SPAN,
			AssignmentOperator::Assign,
			self.assign_target_static_member(self.ident_ref(obj_name), method_name),
			self.ident_ref(import_alias_str),
		);
		stmts.push(self.ast.statement_expression(SPAN, assign));
	}

	/// Emit an external let as a property on a namespace/enum object.
	fn emit_external_impl_let_on_object(
		&mut self,
		meta: &LetDeclaration,
		external_name: &str,
		obj_name: &str,
		stmts: &mut Vec<Statement<'a>>,
	) {
		let Some(ext_path) = self.source_path.and_then(find_external_module) else {
			return;
		};

		let prop_name = self.pattern_to_binding_name(&meta.name.0);
		let import_alias = self.gensym("ext");
		let import_alias_str = self.arena_str(&import_alias);

		let rel = format!(
			"./{}",
			bundled_external_module_name(&ext_path)
				.unwrap_or_else(|| { ext_path.file_name().unwrap().to_string_lossy().into_owned() })
		);
		let mut specifiers = self.ast.vec();
		specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
			self.ast.alloc(
				self.ast.import_specifier(
					SPAN,
					self
						.ast
						.module_export_name_identifier_name(SPAN, self.arena_str(external_name)),
					self.ast.binding_identifier(SPAN, import_alias_str),
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
		stmts.push(Statement::from(import));

		let assign = self.ast.expression_assignment(
			SPAN,
			AssignmentOperator::Assign,
			self.assign_target_static_member(self.ident_ref(obj_name), &prop_name),
			self.ident_ref(import_alias_str),
		);
		stmts.push(self.ast.statement_expression(SPAN, assign));
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
			match &m.0 {
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
				ImplMember::ExternalFunc(_, external_name, meta) => {
					self.emit_external_impl_func_on_prototype(meta, external_name, type_name, &mut stmts);
				}
				_ => {}
			}
		}
		stmts
	}

	/// Emit an external function as a prototype method on a type.
	///
	/// Generates:
	/// 1. `import { externalName as __ext$N } from "./file.ext";`
	/// 2. `TypeName.prototype.methodName = function(...args) { return __ext$N(this, ...args); };`
	fn emit_external_impl_func_on_prototype(
		&mut self,
		meta: &FuncDeclaration,
		external_name: &str,
		type_name: &str,
		stmts: &mut Vec<Statement<'a>>,
	) {
		let Some(ext_path) = self.source_path.and_then(find_external_module) else {
			return;
		};

		let method_name = meta.name.0.as_str();
		let import_alias = self.gensym("ext");
		let import_alias_str = self.arena_str(&import_alias);

		let rel = format!(
			"./{}",
			bundled_external_module_name(&ext_path)
				.unwrap_or_else(|| { ext_path.file_name().unwrap().to_string_lossy().into_owned() })
		);
		let mut specifiers = self.ast.vec();
		specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
			self.ast.alloc(
				self.ast.import_specifier(
					SPAN,
					self
						.ast
						.module_export_name_identifier_name(SPAN, self.arena_str(external_name)),
					self.ast.binding_identifier(SPAN, import_alias_str),
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
		stmts.push(Statement::from(import));

		// Build wrapper: function(...params) { return __ext$N(this, ...params); }
		let mut params = self.ast.vec();
		let mut call_args = self.ast.vec();

		call_args.push(Argument::from(self.ast.expression_this(SPAN)));

		for p in &meta.params {
			let pname = self.pattern_to_binding_name(&p.0.name.0);
			params.push(
				self
					.ast
					.plain_formal_parameter(SPAN, self.binding_pattern(&pname)),
			);
			call_args.push(Argument::from(self.ident_ref(&pname)));
		}

		let formal_params =
			self
				.ast
				.formal_parameters(SPAN, FormalParameterKind::FormalParameter, params, NONE);

		let call_expr = self.call(self.ident_ref(import_alias_str), call_args);
		let fn_body = self.ast.function_body(SPAN, self.ast.vec(), {
			let mut s = self.ast.vec();
			s.push(self.ast.statement_return(SPAN, Some(call_expr)));
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

	// ───────────────── func emit ─────────────────

	fn emit_func(
		&mut self,
		meta: &'a FuncDeclaration,
		body: &Spanned<Expr>,
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
		external_name: &str,
	) -> Vec<Statement<'a>> {
		let name = self.pattern_to_binding_name(&meta.name.0);

		if let Some(ext_path) = self.source_path.and_then(find_external_module) {
			let rel = format!(
				"./{}",
				bundled_external_module_name(&ext_path)
					.unwrap_or_else(|| { ext_path.file_name().unwrap().to_string_lossy().into_owned() })
			);
			let mut specifiers = self.ast.vec();
			specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
				self.ast.alloc(
					self.ast.import_specifier(
						SPAN,
						self
							.ast
							.module_export_name_identifier_name(SPAN, self.arena_str(external_name)),
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
		external_name: &str,
	) -> Vec<Statement<'a>> {
		let name = meta.name.0.as_str();

		if let Some(ext_path) = self.source_path.and_then(find_external_module) {
			let rel = format!(
				"./{}",
				bundled_external_module_name(&ext_path)
					.unwrap_or_else(|| { ext_path.file_name().unwrap().to_string_lossy().into_owned() })
			);
			let mut specifiers = self.ast.vec();
			specifiers.push(ImportDeclarationSpecifier::ImportSpecifier(
				self.ast.alloc(
					self.ast.import_specifier(
						SPAN,
						self
							.ast
							.module_export_name_identifier_name(SPAN, self.arena_str(external_name)),
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

	pub fn emit_statement(&mut self, stmt: &NymphStatement) -> Statement<'a> {
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

	fn expr_contains_anonymous_params(&self, expr: &Spanned<Expr>) -> bool {
		!anonymous_params(expr).is_empty()
	}

	fn should_emit_anonymous_function(&self, expr: &Spanned<Expr>) -> bool {
		match &expr.0 {
			Expr::AnonymousParam(_) => true,
			Expr::Grouped(inner) => self.should_emit_anonymous_function(inner),
			Expr::MemberAccess { parent, .. }
			| Expr::PrefixOp { value: parent, .. }
			| Expr::PostfixOp { value: parent, .. }
			| Expr::TypeOp { lhs: parent, .. }
			| Expr::PatternOp { lhs: parent, .. } => self.expr_contains_anonymous_params(parent),
			Expr::IndexAccess { parent, index, .. } => {
				self.expr_contains_anonymous_params(parent) || self.expr_contains_anonymous_params(index)
			}
			Expr::BinaryOp { lhs, op, rhs } => {
				if *op == BinaryOperator::Pipe {
					self.expr_contains_anonymous_params(lhs)
				} else {
					self.expr_contains_anonymous_params(lhs) || self.expr_contains_anonymous_params(rhs)
				}
			}
			Expr::AssignOp { lhs, .. } => self.expr_contains_anonymous_params(lhs),
			Expr::Call { func, .. } => self.expr_contains_anonymous_params(func),
			Expr::If {
				condition,
				then,
				otherwise,
			} => {
				self.expr_contains_anonymous_params(condition)
					|| self.expr_contains_anonymous_params(then)
					|| otherwise
						.as_ref()
						.map(|it| self.expr_contains_anonymous_params(it))
						.unwrap_or_default()
			}
			Expr::Match { value, .. } => self.expr_contains_anonymous_params(value),
			Expr::List(items) | Expr::Tuple(items) => items.iter().any(|Spanned(item, _)| match item {
				ListItem::Expr(it) | ListItem::Spread(it) => self.expr_contains_anonymous_params(it),
			}),
			Expr::Map(entries) => entries.iter().any(|Spanned(item, _)| match item {
				MapEntry::Expr(key, value) => {
					self.expr_contains_anonymous_params(key) || self.expr_contains_anonymous_params(value)
				}
				MapEntry::Spread(it) => self.expr_contains_anonymous_params(it),
			}),
			Expr::Int(_)
			| Expr::UInt(_)
			| Expr::Float(_)
			| Expr::Char(_)
			| Expr::String(_)
			| Expr::Boolean(_)
			| Expr::Identifier(_)
			| Expr::Range(_)
			| Expr::Closure { .. }
			| Expr::Return { .. }
			| Expr::Break { .. }
			| Expr::Continue { .. }
			| Expr::While { .. }
			| Expr::For { .. }
			| Expr::This
			| Expr::Placeholder
			| Expr::Block { .. } => false,
		}
	}

	pub fn emit_expr(&mut self, expr: &Spanned<Expr>) -> JsValue<'a> {
		if self.should_emit_anonymous_function(expr)
			&& let Some(js) = self.emit_anonymous_function(expr)
		{
			return js;
		}

		self.emit_expr_inner(&expr.0)
	}

	fn emit_anonymous_function(&mut self, expr: &Spanned<Expr>) -> Option<JsValue<'a>> {
		let placeholders = anonymous_params(expr);
		if placeholders.is_empty() {
			return None;
		}

		let arity = placeholders.keys().next_back().map_or(0, |index| index + 1);
		let mut names = std::collections::BTreeMap::new();
		for index in 0..arity {
			names.insert(index, format!("__anon_param_{index}").into());
		}

		let rewritten = rewrite_anonymous_params(expr, &names);
		let mut arrow_params = self.ast.vec();
		for index in 0..arity {
			let name = names
				.get(&index)
				.expect("anonymous param name should exist");
			arrow_params.push(
				self
					.ast
					.plain_formal_parameter(SPAN, self.binding_pattern(name.as_str())),
			);
		}
		let formal = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			arrow_params,
			NONE,
		);
		let js_body = self.emit_expr(&rewritten);
		let body_expr = js_body.into_expression(self.ast, self.alloc);
		let fn_body = self.ast.function_body(SPAN, self.ast.vec(), {
			let mut s = self.ast.vec();
			s.push(self.ast.statement_return(SPAN, Some(body_expr)));
			s
		});

		Some(JsValue {
			stmts: self.ast.vec(),
			expr: self
				.ast
				.expression_arrow_function(SPAN, false, false, NONE, formal, NONE, fn_body),
		})
	}

	fn emit_range_expr(&mut self, range: &ast::expr::RangeKind) -> Expression<'a> {
		match range {
			ast::expr::RangeKind::From(start) => {
				let start = self.emit_expr(start).into_expression(self.ast, self.alloc);
				let contains = self.ast.expression_binary(
					SPAN,
					self.member(self.ast.expression_this(SPAN), "start"),
					oxc::syntax::operator::BinaryOperator::LessEqualThan,
					self.ident_ref("item"),
				);
				let into = self.nymph_string(self.concat_expressions(vec![
					self.member(self.ast.expression_this(SPAN), "start"),
					self.string_lit("..<"),
				]));

				let mut props = self.ast.vec();
				props.push(self.object_property("start", start));
				props.push(self.object_property("contains", self.function_expression(&["item"], contains)));
				props.push(self.object_property(
					"start_bound",
					self.function_expression(
						&[],
						self.bound_object(
							"Included",
							Some(self.member(self.ast.expression_this(SPAN), "start")),
						),
					),
				));
				props.push(self.object_property(
					"end_bound",
					self.function_expression(&[], self.bound_object("Unbounded", None)),
				));
				props.push(self.object_property("into", self.function_expression(&[], into)));

				self.ast.expression_object(SPAN, props)
			}
			ast::expr::RangeKind::To(end) => {
				let end = self.emit_expr(end).into_expression(self.ast, self.alloc);
				let contains = self.ast.expression_binary(
					SPAN,
					self.ident_ref("item"),
					oxc::syntax::operator::BinaryOperator::LessThan,
					self.member(self.ast.expression_this(SPAN), "end"),
				);
				let into = self.nymph_string(self.concat_expressions(vec![
					self.string_lit("..<"),
					self.member(self.ast.expression_this(SPAN), "end"),
				]));

				let mut props = self.ast.vec();
				props.push(self.object_property("end", end));
				props.push(self.object_property("contains", self.function_expression(&["item"], contains)));
				props.push(self.object_property(
					"start_bound",
					self.function_expression(&[], self.bound_object("Unbounded", None)),
				));
				props.push(self.object_property(
					"end_bound",
					self.function_expression(
						&[],
						self.bound_object(
							"Excluded",
							Some(self.member(self.ast.expression_this(SPAN), "end")),
						),
					),
				));
				props.push(self.object_property("into", self.function_expression(&[], into)));

				self.ast.expression_object(SPAN, props)
			}
			ast::expr::RangeKind::Exclusive { min, max } => {
				let start = self.emit_expr(min).into_expression(self.ast, self.alloc);
				let end = self.emit_expr(max).into_expression(self.ast, self.alloc);
				let contains = self.ast.expression_logical(
					SPAN,
					self.ast.expression_binary(
						SPAN,
						self.member(self.ast.expression_this(SPAN), "start"),
						oxc::syntax::operator::BinaryOperator::LessEqualThan,
						self.ident_ref("item"),
					),
					LogicalOperator::And,
					self.ast.expression_binary(
						SPAN,
						self.ident_ref("item"),
						oxc::syntax::operator::BinaryOperator::LessThan,
						self.member(self.ast.expression_this(SPAN), "end"),
					),
				);
				let is_empty = self.ast.expression_binary(
					SPAN,
					self.member(self.ast.expression_this(SPAN), "start"),
					oxc::syntax::operator::BinaryOperator::GreaterEqualThan,
					self.member(self.ast.expression_this(SPAN), "end"),
				);
				let into = self.nymph_string(self.concat_expressions(vec![
					self.member(self.ast.expression_this(SPAN), "start"),
					self.string_lit("..<"),
					self.member(self.ast.expression_this(SPAN), "end"),
				]));

				let mut props = self.ast.vec();
				props.push(self.object_property("start", start));
				props.push(self.object_property("end", end));
				props.push(self.object_property("contains", self.function_expression(&["item"], contains)));
				props.push(self.object_property(
					"start_bound",
					self.function_expression(
						&[],
						self.bound_object(
							"Included",
							Some(self.member(self.ast.expression_this(SPAN), "start")),
						),
					),
				));
				props.push(self.object_property(
					"end_bound",
					self.function_expression(
						&[],
						self.bound_object(
							"Excluded",
							Some(self.member(self.ast.expression_this(SPAN), "end")),
						),
					),
				));
				props.push(self.object_property("is_empty", self.function_expression(&[], is_empty)));
				props.push(self.object_property("into", self.function_expression(&[], into)));

				self.ast.expression_object(SPAN, props)
			}
			ast::expr::RangeKind::ToInclusive(end) => {
				let end = self.emit_expr(end).into_expression(self.ast, self.alloc);
				let contains = self.ast.expression_binary(
					SPAN,
					self.ident_ref("item"),
					oxc::syntax::operator::BinaryOperator::LessEqualThan,
					self.member(self.ast.expression_this(SPAN), "end"),
				);
				let into = self.nymph_string(self.concat_expressions(vec![
					self.string_lit("..="),
					self.member(self.ast.expression_this(SPAN), "end"),
				]));

				let mut props = self.ast.vec();
				props.push(self.object_property("end", end));
				props.push(self.object_property("contains", self.function_expression(&["item"], contains)));
				props.push(self.object_property(
					"start_bound",
					self.function_expression(&[], self.bound_object("Unbounded", None)),
				));
				props.push(self.object_property(
					"end_bound",
					self.function_expression(
						&[],
						self.bound_object(
							"Included",
							Some(self.member(self.ast.expression_this(SPAN), "end")),
						),
					),
				));
				props.push(self.object_property("into", self.function_expression(&[], into)));

				self.ast.expression_object(SPAN, props)
			}
			ast::expr::RangeKind::Inclusive { min, max } => {
				let start = self.emit_expr(min).into_expression(self.ast, self.alloc);
				let end = self.emit_expr(max).into_expression(self.ast, self.alloc);
				let contains = self.ast.expression_logical(
					SPAN,
					self.ast.expression_binary(
						SPAN,
						self.member(self.ast.expression_this(SPAN), "start"),
						oxc::syntax::operator::BinaryOperator::LessEqualThan,
						self.ident_ref("item"),
					),
					LogicalOperator::And,
					self.ast.expression_binary(
						SPAN,
						self.ident_ref("item"),
						oxc::syntax::operator::BinaryOperator::LessEqualThan,
						self.member(self.ast.expression_this(SPAN), "end"),
					),
				);
				let is_empty = self.ast.expression_binary(
					SPAN,
					self.member(self.ast.expression_this(SPAN), "start"),
					oxc::syntax::operator::BinaryOperator::GreaterThan,
					self.member(self.ast.expression_this(SPAN), "end"),
				);
				let into = self.nymph_string(self.concat_expressions(vec![
					self.member(self.ast.expression_this(SPAN), "start"),
					self.string_lit("..="),
					self.member(self.ast.expression_this(SPAN), "end"),
				]));

				let mut props = self.ast.vec();
				props.push(self.object_property("start", start));
				props.push(self.object_property("end", end));
				props.push(self.object_property("contains", self.function_expression(&["item"], contains)));
				props.push(self.object_property(
					"start_bound",
					self.function_expression(
						&[],
						self.bound_object(
							"Included",
							Some(self.member(self.ast.expression_this(SPAN), "start")),
						),
					),
				));
				props.push(self.object_property(
					"end_bound",
					self.function_expression(
						&[],
						self.bound_object(
							"Included",
							Some(self.member(self.ast.expression_this(SPAN), "end")),
						),
					),
				));
				props.push(self.object_property("is_empty", self.function_expression(&[], is_empty)));
				props.push(self.object_property("into", self.function_expression(&[], into)));

				self.ast.expression_object(SPAN, props)
			}
		}
	}

	fn emit_expr_inner(&mut self, expr: &Expr) -> JsValue<'a> {
		match expr {
			Expr::Int(Spanned(n, _)) => JsValue {
				stmts: self.ast.vec(),
				expr: self.number_lit(*n as f64),
			},
			Expr::UInt(Spanned(n, _)) => JsValue {
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
			Expr::AnonymousParam(_) => unreachable!("anonymous params are lowered before emission"),
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
			Expr::Range(range) => JsValue {
				stmts: self.ast.vec(),
				expr: self.emit_range_expr(range),
			},
			Expr::Call {
				func,
				args,
				generics: _,
			} => {
				let is_constructor = match &func.0 {
					Expr::Identifier(ident) => self.ctx.lookup_type_ref(&ident.0).is_some_and(|ty| {
						matches!(
							ty,
							Type::Function {
								constructor: true,
								..
							}
						)
					}),
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
				let prop = self.arena_str(member.0.as_str());
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
						.expression_arrow_function(SPAN, false, false, NONE, formal, NONE, fn_body),
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
									.label_identifier(SPAN, self.ast.ident(self.arena_str(label.0.as_str()))),
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
		lhs: &Spanned<Expr>,
		op: BinaryOperator,
		rhs: &Spanned<Expr>,
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
			// Unwrap: a ?? b → a.unwrap(() => b)
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
					expr: self.method_call(lhs_expr, "unwrap", args),
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
		lhs: &Spanned<Expr>,
		op: AssignOperator,
		rhs: &Spanned<Expr>,
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

	fn emit_assignment_target(&mut self, expr: &Spanned<Expr>) -> AssignmentTarget<'a> {
		match &expr.0 {
			Expr::Identifier(ident) => self
				.ast
				.simple_assignment_target_assignment_target_identifier(
					SPAN,
					self.arena_str(ident.0.as_str()),
				)
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
		lhs: &Spanned<Expr>,
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
				oxc::syntax::operator::BinaryOperator::StrictEquality,
				self.number_lit(*n as f64),
			),
			Pattern::Float(Spanned(f, _)) => self.ast.expression_binary(
				SPAN,
				self.ident_ref(var_name),
				oxc::syntax::operator::BinaryOperator::StrictEquality,
				self.number_lit(f.into_inner()),
			),
			Pattern::Char(Spanned(c, _)) => self.ast.expression_binary(
				SPAN,
				self.ident_ref(var_name),
				oxc::syntax::operator::BinaryOperator::StrictEquality,
				self.number_lit(*c as u32 as f64),
			),
			Pattern::Boolean(Spanned(b, _)) => self.ast.expression_binary(
				SPAN,
				self.ident_ref(var_name),
				oxc::syntax::operator::BinaryOperator::StrictEquality,
				self.bool_lit(*b),
			),
			Pattern::Placeholder => self.bool_lit(true),
			Pattern::Binding { name: _, inner } => self.emit_pattern_check(var_name, &inner.0),
			Pattern::Struct { path, fields } if fields.is_empty() && path.len() == 1 => {
				// A single identifier with no fields is a variable binding (catch-all)
				self.bool_lit(true)
			}
			Pattern::Struct { path, fields: _ } => {
				// Check _tag for enum variant matching
				if let Some(last) = path.last() {
					self.ast.expression_binary(
						SPAN,
						Expression::ComputedMemberExpression(self.ast.alloc_computed_member_expression(
							SPAN,
							self.ident_ref(var_name),
							self.string_lit("~tag"),
							false,
						)),
						oxc::syntax::operator::BinaryOperator::StrictEquality,
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

	/// Collect `const` declarations for variables bound by a pattern.
	/// `var_name` is the JS variable holding the matched value.
	fn collect_pattern_bindings(
		&self,
		var_name: &str,
		pattern: &Pattern,
		out: &mut OxcVec<'a, Statement<'a>>,
	) {
		match pattern {
			Pattern::Binding { name, inner } => {
				// `name @ inner` — bind `name` to the scrutinee, then recurse into inner
				let init = self.ident_ref(var_name);
				out.push(self.var_decl_stmt(VariableDeclarationKind::Const, name.0.as_str(), init));
				self.collect_pattern_bindings(var_name, &inner.0, out);
			}
			Pattern::Struct { path, fields } if fields.is_empty() && path.len() == 1 => {
				// Bare identifier used as a variable binding
				let init = self.ident_ref(var_name);
				out.push(self.var_decl_stmt(VariableDeclarationKind::Const, path[0].0.as_str(), init));
			}
			Pattern::Struct { fields, .. } => {
				// Enum variant / struct pattern — bind fields
				for field in fields {
					match &field.0 {
						ast::expr::StructPatternField::Named(ident) => {
							let init = self.member(self.ident_ref(var_name), ident.0.as_str());
							out.push(self.var_decl_stmt(VariableDeclarationKind::Const, ident.0.as_str(), init));
						}
						ast::expr::StructPatternField::Value { name, value } => {
							let field_tmp = format!("{var_name}.{}", name.0);
							let field_access = self.member(self.ident_ref(var_name), name.0.as_str());
							let binding_name = self.pattern_to_binding_name(&value.0);
							out.push(self.var_decl_stmt(
								VariableDeclarationKind::Const,
								&binding_name,
								field_access,
							));
							// Recurse for nested patterns (e.g., `field = inner @ _`)
							self.collect_pattern_bindings(&field_tmp, &value.0, out);
						}
						ast::expr::StructPatternField::Rest => {}
					}
				}
			}
			Pattern::Grouped(inner) => self.collect_pattern_bindings(var_name, &inner.0, out),
			Pattern::Union(a, _) => {
				// Union patterns must bind the same names; just use the first branch
				self.collect_pattern_bindings(var_name, &a.0, out);
			}
			_ => {}
		}
	}

	// ───────────────── if expression ─────────────────

	fn emit_if(
		&mut self,
		condition: &Spanned<Expr>,
		then: &Spanned<Expr>,
		otherwise: Option<&Spanned<Expr>>,
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

	fn emit_match(&mut self, value: &Spanned<Expr>, arms: &[MatchArm]) -> JsValue<'a> {
		let scrutinee = self.emit_expr(value);
		let scrutinee_expr = scrutinee.into_expression(self.ast, self.alloc);
		let tmp = self.gensym("match");

		let mut stmts = self.ast.vec();
		stmts.push(self.var_decl_stmt(VariableDeclarationKind::Const, &tmp, scrutinee_expr));

		// Build chained if/else
		let mut current_else: Option<Statement<'a>> = None;

		for arm in arms.iter().rev() {
			let check = self.emit_pattern_check(&tmp, &arm.pattern.0);

			// Collect pattern bindings and emit them as const declarations
			let mut binding_stmts = self.ast.vec();
			self.collect_pattern_bindings(&tmp, &arm.pattern.0, &mut binding_stmts);

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

			// Wrap bindings + body in a block
			let mut body_stmts = self.ast.vec();
			for s in binding_stmts {
				body_stmts.push(s);
			}
			body_stmts.push(self.ast.statement_return(SPAN, Some(body_expr)));
			let consequent = self.ast.statement_block(SPAN, body_stmts);

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

	fn emit_block(&mut self, body: &[Spanned<NymphStatement>]) -> JsValue<'a> {
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

	fn emit_string_parts(&mut self, parts: &[Spanned<StringPart>]) -> Expression<'a> {
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
						.expression_binary(SPAN, a, oxc::syntax::operator::BinaryOperator::Addition, b)
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

	fn emit_pattern_binding(&self, pat: &Pattern) -> BindingPattern<'a> {
		match pat {
			Pattern::Binding { name, inner } => {
				if matches!(inner.0, Pattern::Placeholder) {
					self.binding_pattern(name.0.as_str())
				} else {
					self.emit_pattern_binding(&inner.0)
				}
			}
			Pattern::Struct { path, fields } if fields.is_empty() && path.len() == 1 => {
				self.binding_pattern(path[0].0.as_str())
			}
			Pattern::Struct { fields, .. } => {
				let mut props = self.ast.vec();
				let mut rest = None;
				for field in fields {
					match &field.0 {
						ast::expr::StructPatternField::Named(ident) => {
							let key = PropertyKey::StaticIdentifier(
								self.ast.alloc(
									self
										.ast
										.identifier_name(SPAN, self.arena_str(ident.0.as_str())),
								),
							);
							let value = self.binding_pattern(ident.0.as_str());
							props.push(self.ast.binding_property(SPAN, key, value, true, false));
						}
						ast::expr::StructPatternField::Value { name, value } => {
							let key = PropertyKey::StaticIdentifier(
								self.ast.alloc(
									self
										.ast
										.identifier_name(SPAN, self.arena_str(name.0.as_str())),
								),
							);
							let val_pat = self.emit_pattern_binding(&value.0);
							props.push(self.ast.binding_property(SPAN, key, val_pat, false, false));
						}
						ast::expr::StructPatternField::Rest => {
							rest = Some(
								self
									.ast
									.alloc_binding_rest_element(SPAN, self.binding_pattern("_")),
							);
						}
					}
				}
				self.ast.binding_pattern_object_pattern(SPAN, props, rest)
			}
			Pattern::List(items) | Pattern::Tuple(items) => {
				let mut elems = self.ast.vec();
				let mut rest = None;
				for item in items {
					match &item.0 {
						ast::expr::ListPatternEntry::Item(inner) => {
							elems.push(Some(self.emit_pattern_binding(&inner.0)));
						}
						ast::expr::ListPatternEntry::Rest(name) => {
							let rest_name = name.as_ref().map(|n| n.0.as_str()).unwrap_or("_");
							rest = Some(
								self
									.ast
									.alloc_binding_rest_element(SPAN, self.binding_pattern(rest_name)),
							);
						}
					}
				}
				self.ast.binding_pattern_array_pattern(SPAN, elems, rest)
			}
			Pattern::Placeholder => self.binding_pattern("_"),
			Pattern::Grouped(inner) => self.emit_pattern_binding(&inner.0),
			_ => self.binding_pattern("_"),
		}
	}

	fn export_stmt(&self, stmt: Statement<'a>) -> Statement<'a> {
		let decl = match stmt {
			Statement::VariableDeclaration(d) => oxc::ast::ast::Declaration::VariableDeclaration(d),
			Statement::ClassDeclaration(d) => oxc::ast::ast::Declaration::ClassDeclaration(d),
			Statement::FunctionDeclaration(d) => oxc::ast::ast::Declaration::FunctionDeclaration(d),
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
			Option::<oxc::ast::ast::Declaration<'a>>::None,
			specifiers,
			None,
			ImportOrExportKind::Value,
			NONE,
		);
		Statement::from(export)
	}
}
