use oxc::{
	allocator::{Allocator, Box as ArenaBox, Vec as ArenaVec},
	ast::{AstBuilder, ast::*},
	codegen::Codegen,
	span::SPAN,
};

use nymph_hir::hir::{
	BinOp, HirArrayElem, HirClass, HirEnum, HirExpr, HirFunc, HirLet, HirLit, HirMapElem, HirMethod,
	HirModule, HirPat, HirRange, HirStmt, ScalarCastKind, UnOp,
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
	/// `<base>[<index>]`.
	Index(Box<Subject>, usize),
	/// `<base>[<base>.length - <offset>]` — a list element counted from the end
	/// (`offset` ≥ 1; the last element is offset 1).
	IndexFromEnd(Box<Subject>, usize),
	/// `<base>.get(<key>)` — a map value.
	MapGet(Box<Subject>, HirLit),
	/// `<base>.slice(<start>, <base>.length - <end_from_end>)` — a list rest slice
	/// (`end_from_end == 0` ⇒ `<base>.slice(<start>)`).
	Slice(Box<Subject>, usize, usize),
	/// The rest-of-map for a map pattern's `...rest` — a shallow copy of `<base>`
	/// minus the named keys: `new Map(<base>)` when `keys` is empty, else an
	/// IIFE that copies then deletes each key.
	MapRest(Box<Subject>, Vec<HirLit>),
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
	#[allow(dead_code)]
	alloc: &'a Allocator,
	/// Counter for fresh temporary names (result temporaries for value-position
	/// control flow). `Cell` keeps the emit methods `&self`.
	gensym: std::cell::Cell<u32>,
	/// Set while emitting a control-flow expression's (`Block`/`If`/`While`/
	/// `Match`) IIFE body from `emit_expr`'s subexpression-position fallthrough —
	/// the ONLY place `JsValue::into_expression` wraps in an arrow-IIFE (the enum
	/// factory path in `emit_enum` never contains user statement code, so it needs
	/// no guard). A JS `return` emitted while this is set would return from that
	/// IIFE, not the enclosing function, so `emit_stmt`'s `HirStmt::Return` arm
	/// asserts against it. `Cell` keeps emit methods `&self`; save/restore (never a
	/// bare `set(true)`) around each use so the flag can't stay stuck true after a
	/// nested subexpression-position construct returns control to a statement-
	/// position caller (Slice 4E, Y1).
	in_iife_subexpr: std::cell::Cell<bool>,
	/// Every `(module, symbol)` pair a `HirExpr::ExternCall` lowered during
	/// this emit run needs imported (Gap 3, L0) — populated by the
	/// `HirExpr::ExternCall` arm of [`Self::emit_expr`], drained by
	/// [`Self::emit_module`] into a deduped, deterministically-ordered
	/// `import { symbol } from "module";` per pair, prepended ahead of every
	/// other top-level statement. A `BTreeSet` (not a `HashSet`) so the
	/// prepended import order — and therefore the emitted JS text — stays
	/// stable across runs, which the golden/e2e tests rely on.
	needed_imports: std::cell::RefCell<std::collections::BTreeSet<(&'static str, &'static str)>>,
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
			in_iife_subexpr: std::cell::Cell::new(false),
			needed_imports: std::cell::RefCell::new(std::collections::BTreeSet::new()),
		}
	}

	/// A fresh temporary variable name (`_t0`, `_t1`, …).
	fn gensym(&self) -> String {
		let n = self.gensym.get();
		self.gensym.set(n + 1);
		format!("_t{n}")
	}

	pub fn emit_module(&self, module: &HirModule) -> String {
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
		}
		// Top-level `let`s (Slice 4E, Y3) after classes/enums (a let constructing or
		// referencing one must see it already defined — module-scope `const`/`let`
		// is TDZ, unlike a function declaration) and before functions (whose JS
		// `function` declarations hoist, so a let calling one is safe regardless of
		// relative placement). Kept in source order relative to each other.
		for let_ in &module.lets {
			stmts.push(self.emit_module_let(let_));
		}
		for func in &module.funcs {
			stmts.push(self.emit_func(func));
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
			for (module_specifier, symbol) in imports.iter() {
				with_imports.push(self.build_import_statement(module_specifier, symbol));
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
		Codegen::new().build(&program).code
	}

	/// Build `import { <symbol> } from "<module_specifier>";` (Gap 3, L0).
	fn build_import_statement(&self, module_specifier: &str, symbol: &str) -> Statement<'a> {
		let imported = ModuleExportName::IdentifierName(IdentifierName::new(
			SPAN,
			self.ast.allocator.alloc_str(symbol),
			&self.ast,
		));
		let local = BindingIdentifier::new(SPAN, self.ast.allocator.alloc_str(symbol), &self.ast);
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

	fn emit_func(&self, func: &HirFunc) -> Statement<'a> {
		// function <name>(<params>) { return <body>; }
		//
		// When the body is itself a `Block`, emit its statements directly into the
		// function body (followed by `return <tail>;`) instead of wrapping them in a
		// needless IIFE via `emit_expr`/`into_expression`.
		let mut body_stmts = ArenaVec::new_in(&self.ast);
		match &func.body {
			HirExpr::Block { .. } => {
				let value = self.emit_value(&func.body);
				body_stmts.extend(value.stmts);
				body_stmts.push(Statement::ReturnStatement(ReturnStatement::boxed(
					SPAN,
					Some(value.expr),
					&self.ast,
				)));
			}
			other => {
				let body_expr = self.emit_expr(other);
				body_stmts.push(Statement::ReturnStatement(ReturnStatement::boxed(
					SPAN,
					Some(body_expr),
					&self.ast,
				)));
			}
		}
		let mut js_params = ArenaVec::new_in(&self.ast);
		for param in &func.params {
			let binding_pattern = BindingPattern::BindingIdentifier(BindingIdentifier::boxed(
				SPAN,
				self.ast.allocator.alloc_str(param),
				&self.ast,
			));
			js_params.push(FormalParameter::new_plain(SPAN, binding_pattern, &self.ast));
		}
		let params = FormalParameters::new(
			SPAN,
			FormalParameterKind::FormalParameter,
			js_params,
			oxc::ast::NONE,
			&self.ast,
		);
		let fn_body = FunctionBody::new(SPAN, ArenaVec::new_in(&self.ast), body_stmts, &self.ast);
		let function = Function::boxed(
			SPAN,
			FunctionType::FunctionDeclaration,
			Some(BindingIdentifier::new(
				SPAN,
				self.ast.allocator.alloc_str(&func.name),
				&self.ast,
			)),
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			params,
			oxc::ast::NONE,
			Some(fn_body),
			&self.ast,
		);
		Statement::FunctionDeclaration(function)
	}

	/// `const <name> = <value>;` (or `let` when `mutable`) — a top-level `let`
	/// (Slice 4E, Y3). Mirrors `HirStmt::Let`'s mutable → `Let`/`Const` mapping in
	/// `emit_stmt`, generalizing the const-only `const_decl` helper for the `let
	/// mut` case (the checker accepts top-level `let mut`, so codegen honors it).
	fn emit_module_let(&self, let_: &HirLet) -> Statement<'a> {
		let kind = if let_.mutable {
			VariableDeclarationKind::Let
		} else {
			VariableDeclarationKind::Const
		};
		let init = self.emit_expr(&let_.value);
		let pat = BindingPattern::new_binding_identifier(
			SPAN,
			self.ast.allocator.alloc_str(&let_.name),
			&self.ast,
		);
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

	/// Emit a struct as `class <Name> { constructor(fields) { Object.assign(this, fields); } }`.
	///
	/// The object-argument constructor lets construction pass labeled fields as a
	/// plain object (`new Point({ x, y })`) without depending on field order.
	/// `Object.assign` copies each property onto the instance; field defaults and
	/// validation are deferred to a later slice.
	fn emit_class(&self, class: &HirClass) -> Statement<'a> {
		// Object.assign(this, fields)
		let object_assign = Expression::StaticMemberExpression(StaticMemberExpression::boxed(
			SPAN,
			Expression::Identifier(IdentifierReference::boxed(SPAN, "Object", &self.ast)),
			IdentifierName::new(SPAN, "assign", &self.ast),
			false,
			&self.ast,
		));
		let mut call_args = ArenaVec::new_in(&self.ast);
		call_args.push(Argument::from(Expression::ThisExpression(
			ThisExpression::boxed(SPAN, &self.ast),
		)));
		call_args.push(Argument::from(Expression::Identifier(
			IdentifierReference::boxed(SPAN, "fields", &self.ast),
		)));
		let assign_call = Expression::CallExpression(CallExpression::boxed(
			SPAN,
			object_assign,
			oxc::ast::NONE,
			call_args,
			false,
			&self.ast,
		));
		let mut ctor_stmts = ArenaVec::new_in(&self.ast);
		ctor_stmts.push(Statement::new_expression_statement(
			SPAN,
			assign_call,
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
		let name = BindingIdentifier::new(SPAN, self.ast.allocator.alloc_str(&class.name), &self.ast);
		let class = Class::boxed(
			SPAN,
			ClassType::ClassDeclaration,
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
		Statement::ClassDeclaration(class)
	}

	/// Build a method's params/body into a plain JS `FunctionExpression`
	/// (`(<params>) { return <body>; }`), independent of how the caller wraps
	/// it — a class method definition (struct/class instance methods) or an
	/// object-literal method property (the enum prototype ABI, Slice 4D) both
	/// share this exactly. Mirrors [`Self::emit_func`]'s param/body handling.
	/// Deliberately a plain function, never an arrow: prototype methods need
	/// their own `this` bound to the receiver at call time.
	fn method_function(&self, method: &HirMethod) -> ArenaBox<'a, Function<'a>> {
		let mut body_stmts = ArenaVec::new_in(&self.ast);
		match &method.body {
			HirExpr::Block { .. } => {
				let value = self.emit_value(&method.body);
				body_stmts.extend(value.stmts);
				body_stmts.push(Statement::new_return_statement(
					SPAN,
					Some(value.expr),
					&self.ast,
				));
			}
			other => {
				let body_expr = self.emit_expr(other);
				body_stmts.push(Statement::new_return_statement(
					SPAN,
					Some(body_expr),
					&self.ast,
				));
			}
		}
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
			Expression::FunctionExpression(func),
			true,
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
	/// `nymph-compiler::intrinsics`'s injected `std/option` virtual module)
	/// values of "the same" enum variant compare equal by construction. The
	/// TAG KEY itself (`emit_tag_const`, above) was already global via
	/// `Symbol.for("nymph.tag")` — only the per-variant discriminant VALUE
	/// was the gap.
	///
	/// X1: when the enum has methods, a `const proto = { … };` object (built the
	/// same way as struct class methods, see [`Self::emit_method_property`]) is
	/// also emitted inside the IIFE, and every variant value is created with
	/// `Object.create(proto)` as its prototype instead of a plain object literal
	/// — so `c.m()` and `this` inside a method work natively. A method-less enum
	/// emits none of that, staying byte-identical to before Slice 4D.
	fn emit_enum(&self, hir_enum: &HirEnum) -> Statement<'a> {
		let mut stmts = ArenaVec::new_in(&self.ast);
		let has_methods = !hir_enum.methods.is_empty();
		if has_methods {
			stmts.push(self.emit_enum_proto(&hir_enum.methods));
		}
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
				self.member_call(
					Expression::new_identifier(SPAN, "Object", &self.ast),
					"freeze",
					vec![base],
				)
			} else {
				let factory = self.variant_factory(&t_name, has_methods);
				self.member_call(
					Expression::new_identifier(SPAN, "Object", &self.ast),
					"assign",
					vec![factory, tag_obj],
				)
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
		let return_obj = Expression::new_object_expression(SPAN, props, &self.ast);
		let iife = JsValue {
			stmts,
			expr: return_obj,
		}
		.into_expression(self.ast);
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
		self.member_call(
			Expression::new_identifier(SPAN, "Object", &self.ast),
			"assign",
			vec![create_call, props],
		)
	}

	/// `(fields) => { return { [TAG]: <t_name>, ...fields }; }` — a field variant's
	/// object-argument factory. When `has_methods`, the returned object is instead
	/// `Object.assign(Object.create(proto), { [TAG]: <t_name>, ...fields })` so the
	/// constructed value carries the shared prototype's methods (Slice 4D, X1).
	fn variant_factory(&self, t_name: &str, has_methods: bool) -> Expression<'a> {
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
		let pat =
			BindingPattern::new_binding_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast);
		let declarator = VariableDeclarator::new(
			SPAN,
			VariableDeclarationKind::Const,
			pat,
			oxc::ast::NONE,
			Some(init),
			false,
			&self.ast,
		);
		let decl = VariableDeclaration::new(
			SPAN,
			VariableDeclarationKind::Const,
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

	/// `<callee>(<arg>)` — a single-argument call.
	fn call1(&self, callee: Expression<'a>, arg: Expression<'a>) -> Expression<'a> {
		let mut args = ArenaVec::new_in(&self.ast);
		args.push(Argument::from(arg));
		Expression::new_call_expression(SPAN, callee, oxc::ast::NONE, args, false, &self.ast)
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

	/// A bare global identifier reference (`Math`, `Number`, `Infinity`, or a
	/// cast-IIFE's gensym parameter — see `saturating_scalar_cast`).
	fn ident(&self, name: &'a str) -> Expression<'a> {
		Expression::new_identifier(SPAN, name, &self.ast)
	}

	/// `Math.<method>(<arg>)`.
	fn math_call(&self, method: &str, arg: Expression<'a>) -> Expression<'a> {
		let math = self.ident("Math");
		self.member_call(math, method, vec![arg])
	}

	/// `Number.isNaN(<arg>)`.
	fn number_is_nan(&self, arg: Expression<'a>) -> Expression<'a> {
		let number = self.ident("Number");
		self.member_call(number, "isNaN", vec![arg])
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

	/// `-Infinity` — a unary negation of the `Infinity` global.
	fn neg_infinity(&self) -> Expression<'a> {
		let infinity = self.ident("Infinity");
		Expression::new_unary_expression(SPAN, UnaryOperator::UnaryNegation, infinity, &self.ast)
	}

	/// An `i64` value as a JS numeric literal. `i64::MAX` (`2^63 - 1`) isn't
	/// exactly representable as an `f64` — JS stores/prints the nearest double,
	/// `2^63`, exactly as if `9223372036854775807` had been written directly in
	/// JS source and parsed as a `Number`. `i64::MIN` (`-2^63`) IS exactly
	/// representable.
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

	/// The saturating JS runtime mapping for a numeric `ScalarCast` (the change
	/// that supersedes Slice 4K's plain `Math.trunc` passthrough): Nymph defines
	/// its own float→int/uint semantics rather than inheriting JS's (`Math.trunc`
	/// passes `NaN`/`±Infinity` straight through) or Rust's (`as` saturates, but
	/// isn't reproducible on JS numbers as-is). Builds an `arrow_iife` around
	/// `operand` — evaluating it exactly once — with `t` (a gensym) standing in
	/// for it in the body:
	///
	/// * `unsigned == false` (`float as int`): `Number.isNaN(t) ? 0 : t ===
	///   Infinity ? i64::MAX : t === -Infinity ? i64::MIN : Math.trunc(t)`.
	/// * `unsigned == true` (`float as uint` / `int as uint`): `t` is
	///   `Math.abs`-ed first, so `-Infinity` collapses onto the same `Infinity`
	///   branch as `+Infinity`, and a negative finite value (or a negative `int`)
	///   saturates to its absolute value: `Number.isNaN(a) ? 0 : a === Infinity ?
	///   i64::MAX : Math.trunc(a)` where `a = Math.abs(t)`.
	fn saturating_scalar_cast(&self, operand: Expression<'a>, unsigned: bool) -> Expression<'a> {
		let param = self.gensym();
		let param = self.ast.allocator.alloc_str(&param);
		let max = self.i64_literal(i64::MAX);

		let body = if unsigned {
			let is_nan = self.number_is_nan(self.math_call("abs", self.ident(param)));
			let is_inf = self.strict_eq(
				self.math_call("abs", self.ident(param)),
				self.ident("Infinity"),
			);
			let trunc = self.math_call("trunc", self.math_call("abs", self.ident(param)));
			Expression::new_conditional_expression(
				SPAN,
				is_nan,
				self.zero(),
				Expression::new_conditional_expression(SPAN, is_inf, max, trunc, &self.ast),
				&self.ast,
			)
		} else {
			let is_nan = self.number_is_nan(self.ident(param));
			let is_pos_inf = self.strict_eq(self.ident(param), self.ident("Infinity"));
			let is_neg_inf = self.strict_eq(self.ident(param), self.neg_infinity());
			let min = self.i64_literal(i64::MIN);
			let trunc = self.math_call("trunc", self.ident(param));
			Expression::new_conditional_expression(
				SPAN,
				is_nan,
				self.zero(),
				Expression::new_conditional_expression(
					SPAN,
					is_pos_inf,
					max,
					Expression::new_conditional_expression(SPAN, is_neg_inf, min, trunc, &self.ast),
					&self.ast,
				),
				&self.ast,
			)
		};

		self.arrow_iife(param, body, operand)
	}

	fn emit_expr(&self, expr: &HirExpr) -> Expression<'a> {
		match expr {
			HirExpr::Num(value) => {
				Expression::new_numeric_literal(SPAN, *value, None, NumberBase::Decimal, &self.ast)
			}
			HirExpr::Str(s) => {
				Expression::new_string_literal(SPAN, self.ast.allocator.alloc_str(s), None, &self.ast)
			}
			HirExpr::Bool(b) => Expression::new_boolean_literal(SPAN, *b, &self.ast),
			HirExpr::Char(c) => {
				// A Nymph char is a single-character JS string.
				let s = self.ast.allocator.alloc_str(&c.to_string());
				Expression::new_string_literal(SPAN, s, None, &self.ast)
			}
			HirExpr::Local(name) => {
				Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(name), &self.ast)
			}
			// The `this` receiver.
			HirExpr::This => Expression::new_this_expression(SPAN, &self.ast),
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
					UnOp::BitNot => UnaryOperator::BitwiseNot,
				};
				Expression::new_unary_expression(SPAN, operator, inner, &self.ast)
			}
			HirExpr::Call { callee, args } => {
				let callee = self.emit_expr(callee);
				let mut arguments = ArenaVec::new_in(&self.ast);
				for arg in args {
					arguments.push(Argument::from(self.emit_expr(arg)));
				}
				Expression::new_call_expression(SPAN, callee, oxc::ast::NONE, arguments, false, &self.ast)
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
			} => {
				self.needed_imports.borrow_mut().insert((*module, *symbol));
				let callee =
					Expression::new_identifier(SPAN, self.ast.allocator.alloc_str(symbol), &self.ast);
				let mut arguments = ArenaVec::new_in(&self.ast);
				for arg in args {
					arguments.push(Argument::from(self.emit_expr(arg)));
				}
				Expression::new_call_expression(SPAN, callee, oxc::ast::NONE, arguments, false, &self.ast)
			}
			// A tuple/list literal → a JS array `[a, b, …]`.
			HirExpr::Array(items) => {
				let mut elems = ArenaVec::new_in(&self.ast);
				for item in items {
					elems.push(ArrayExpressionElement::from(self.emit_expr(item)));
				}
				Expression::new_array_expression(SPAN, elems, &self.ast)
			}
			// A list literal with at least one spread element (SS1) → a JS array
			// `[a, ...xs, b]`, preserving left-to-right source order. Each
			// `HirArrayElem::Spread` payload is already a JS-array-valued
			// expression (a native source or a `lower_spread_source` drain IIFE),
			// so it always emits with JS spread syntax.
			HirExpr::ArraySpread(elems) => {
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
				Expression::new_array_expression(SPAN, arr, &self.ast)
			}
			// A map literal → `new Map([[k, v], …])`.
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
				let callee = Expression::new_identifier(SPAN, "Map", &self.ast);
				let mut args = ArenaVec::new_in(&self.ast);
				args.push(Argument::from(outer));
				Expression::new_new_expression(SPAN, callee, oxc::ast::NONE, args, &self.ast)
			}
			// A map literal with at least one spread entry (SS1) → `new Map([...])`
			// merging the spread entries in, left-to-right (a later duplicate key
			// wins — the `Map` constructor processes its entries array in order,
			// SS4). Each `HirMapElem::Spread` payload is already an array of
			// `[k, v]` pairs (a native `Map` — iterates as `[k, v]` pairs — or a
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
				let callee = Expression::new_identifier(SPAN, "Map", &self.ast);
				let mut args = ArenaVec::new_in(&self.ast);
				args.push(Argument::from(outer));
				Expression::new_new_expression(SPAN, callee, oxc::ast::NONE, args, &self.ast)
			}
			// A list/tuple subscript → a computed member `recv[index]`.
			HirExpr::Index { recv, index } => {
				let object = self.emit_expr(recv);
				let property = self.emit_expr(index);
				Expression::ComputedMemberExpression(ComputedMemberExpression::boxed(
					SPAN, object, property, false, &self.ast,
				))
			}
			// Struct construction → `new <class>({ field: value, … })`.
			HirExpr::New { class, fields } => {
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
				Expression::new_new_expression(SPAN, callee, oxc::ast::NONE, args, &self.ast)
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
				self.call1(callee, obj)
			}
			// Nullary variant reference → `<enum>.<variant>` (the frozen singleton).
			HirExpr::VariantRef { enum_name, variant } => self.variant_member(enum_name, variant),
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
			HirExpr::Assign { target, value } => {
				let value_expr = self.emit_expr(value);
				// A `Map` target has no JS assignment-expression form at all — `m[k] =
				// v` on a real `Map` would silently set an own property on the `Map`
				// object rather than mutating its entries — so it lowers to a
				// `.set(key, value)` call instead of an `AssignmentTarget` (confirmed
				// reachable: `infer_assign`'s field/index arm, infer_expr.rs, accepts
				// any place expression including `IndexAccess` with no restriction,
				// and a `Map`-typed receiver's `IndexAccess` lowers to
				// `HirExpr::MapGet` just like a read, lower_hir.rs — so `m[k] = v`
				// reaches here as `Assign { target: MapGet { .. }, .. }` from a
				// zero-diagnostic program).
				if let HirExpr::MapGet { recv, key } = target.as_ref() {
					let object = self.emit_expr(recv);
					let key_expr = self.emit_expr(key);
					return self.member_call(object, "set", vec![key_expr, value_expr]);
				}
				// Slice 4J, Task 2: a plain `this.field = value` assignment (a
				// `mut func` — or, per the checker's own permissiveness, ANY
				// method's — field mutation) lowers to `HirExpr::Assign { target:
				// Field { .. }, .. }` and reaches here from a zero-diagnostic
				// program (the checker imposes no mutability restriction beyond
				// an ordinary field-assignment target, confirmed by probe). A
				// member-expression target needs its own `AssignmentTarget`
				// (`SimpleAssignmentTarget` inherits `MemberExpression`), not the
				// identifier-only path `HirExpr::Local` uses.
				//
				// A list/tuple subscript target (`xs[i] = value`) is the same shape
				// but with a COMPUTED member (`SimpleAssignmentTarget` also inherits
				// `ComputedMemberExpression`) — confirmed reachable the same way as
				// the `Map` case above: a non-`Map` receiver's `IndexAccess` lowers to
				// `HirExpr::Index` (lower_hir.rs), so `xs[i] = value` reaches here as
				// `Assign { target: Index { .. }, .. }` from a zero-diagnostic
				// program. This `unreachable!` used to fire (an ICE) on exactly that
				// valid input (confirmed by probe: `func f(xs: #[int], i: int): void
				// = { xs[i] = 5 }` type-checks with zero diagnostics yet panicked
				// here) — this codebase never treats a crash on valid input as an
				// acceptable substitute for correct codegen.
				let assignment_target = match target.as_ref() {
					HirExpr::Local(n) => self.assign_target(self.ast.allocator.alloc_str(n)),
					HirExpr::Field { recv, name } => {
						let object = self.emit_expr(recv);
						let member = StaticMemberExpression::boxed(
							SPAN,
							object,
							IdentifierName::new(SPAN, self.ast.allocator.alloc_str(name), &self.ast),
							false,
							&self.ast,
						);
						AssignmentTarget::from(MemberExpression::StaticMemberExpression(member))
					}
					HirExpr::Index { recv, index } => {
						let object = self.emit_expr(recv);
						let property = self.emit_expr(index);
						let member = ComputedMemberExpression::boxed(SPAN, object, property, false, &self.ast);
						AssignmentTarget::from(MemberExpression::ComputedMemberExpression(member))
					}
					other => unreachable!(
						"lowering never produces an assignment target other than a local, a field access, a list/tuple subscript, or a map index (got {other:?})"
					),
				};
				Expression::new_assignment_expression(
					SPAN,
					AssignmentOperator::Assign,
					assignment_target,
					value_expr,
					&self.ast,
				)
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
			| HirExpr::While { .. }
			| HirExpr::Match { .. } => {
				let prev = self.in_iife_subexpr.replace(true);
				let result = self.emit_value(expr).into_expression(self.ast);
				self.in_iife_subexpr.set(prev);
				result
			}
			// A built-in `as` cast's JS runtime mapping (Slice 4K, extended by the
			// saturating-cast change) — see `HirExpr::ScalarCast`'s doc comment for
			// why these are dedicated calls rather than composed `Field`/`Call`
			// nodes over a `Local("Math"/"String"/"Number")` (shadow-proofing a user
			// local of that name).
			HirExpr::ScalarCast { kind, operand } => {
				let operand = self.emit_expr(operand);
				match kind {
					ScalarCastKind::SaturatingToInt => self.saturating_scalar_cast(operand, false),
					ScalarCastKind::SaturatingToUInt => self.saturating_scalar_cast(operand, true),
					ScalarCastKind::CharToNum => {
						let zero =
							Expression::new_numeric_literal(SPAN, 0.0, None, NumberBase::Decimal, &self.ast);
						self.member_call(operand, "codePointAt", vec![zero])
					}
					ScalarCastKind::NumToChar => {
						let string = Expression::new_identifier(SPAN, "String", &self.ast);
						self.member_call(string, "fromCodePoint", vec![operand])
					}
					ScalarCastKind::FloatToChar => {
						let math = Expression::new_identifier(SPAN, "Math", &self.ast);
						let truncated = self.member_call(math, "trunc", vec![operand]);
						let string = Expression::new_identifier(SPAN, "String", &self.ast);
						self.member_call(string, "fromCodePoint", vec![truncated])
					}
				}
			}
			// A closure → a JS arrow function `(<params>) => { … }` (Slice 4L).
			HirExpr::Closure { params, body } => self.closure_arrow(params, body),
		}
	}

	/// `(<params>) => { <body stmts>; return <tail>; }` — a closure's arrow
	/// function (Slice 4L). Mirrors `emit_func`'s body split exactly: a `Block`
	/// body's own statements/tail flatten directly into the arrow's
	/// `FunctionBody` (no needless nested IIFE), any other body becomes a
	/// single `return <expr>;`.
	///
	/// Saves and resets `in_iife_subexpr` to `false` around the body emission —
	/// the arrow is a real function boundary, exactly like `emit_func`'s
	/// top-level function body implicitly is (that path never sets the flag at
	/// all). Lowering already rejects every `return` lexically inside a closure
	/// body (Slice 4L, JJ2), so no `HirStmt::Return` can actually reach this
	/// boundary today — but it's the correct boundary story regardless (a
	/// closure built while emitting an enclosing subexpression-position
	/// construct, e.g. a closure passed as a call argument inside a match arm
	/// used as a subexpression, must not inherit that outer IIFE's `return`
	/// target), and it stops being merely defensive the moment closure-scoped
	/// `return` is ever allowed.
	fn closure_arrow(&self, params: &[ecow::EcoString], body: &HirExpr) -> Expression<'a> {
		let prev = self.in_iife_subexpr.replace(false);
		let mut body_stmts = ArenaVec::new_in(&self.ast);
		match body {
			HirExpr::Block { .. } => {
				let value = self.emit_value(body);
				body_stmts.extend(value.stmts);
				body_stmts.push(Statement::new_return_statement(
					SPAN,
					Some(value.expr),
					&self.ast,
				));
			}
			other => {
				let body_expr = self.emit_expr(other);
				body_stmts.push(Statement::new_return_statement(
					SPAN,
					Some(body_expr),
					&self.ast,
				));
			}
		}
		self.in_iife_subexpr.set(prev);

		let mut js_params = ArenaVec::new_in(&self.ast);
		for param in params {
			let binding_pattern = BindingPattern::new_binding_identifier(
				SPAN,
				self.ast.allocator.alloc_str(param),
				&self.ast,
			);
			js_params.push(FormalParameter::new_plain(SPAN, binding_pattern, &self.ast));
		}
		let formal = FormalParameters::new(
			SPAN,
			FormalParameterKind::ArrowFormalParameters,
			js_params,
			oxc::ast::NONE,
			&self.ast,
		);
		let function_body = FunctionBody::new(SPAN, ArenaVec::new_in(&self.ast), body_stmts, &self.ast);
		Expression::new_arrow_function_expression(
			SPAN,
			false,
			false,
			oxc::ast::NONE,
			formal,
			oxc::ast::NONE,
			function_body,
			&self.ast,
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
				let pat = BindingPattern::new_binding_identifier(
					SPAN,
					self.ast.allocator.alloc_str(name),
					&self.ast,
				);
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
			// A statement-position control-flow expression flattens directly into a
			// plain JS `BlockStatement` via `block_stmt` (matching how a `while` body
			// already does), rather than going through `emit_expr`'s subexpression
			// fallthrough — which would otherwise wrap it in a needless IIFE, and
			// (post Slice 4E, Y1) trip the `return`-inside-IIFE guard for a
			// statement-position `if`/`while`/`match` that legitimately contains a
			// `return`. The `BlockStatement` still gives it its own JS scope
			// (unaffected by Y2 shadowing) and keeps any gensym `let _tN` temps
			// scoped to it, same as before.
			HirStmt::Expr(
				e @ (HirExpr::Block { .. }
				| HirExpr::If { .. }
				| HirExpr::While { .. }
				| HirExpr::Match { .. }),
			) => self.block_stmt(e),
			HirStmt::Expr(e) => {
				let expr = self.emit_expr(e);
				Statement::new_expression_statement(SPAN, expr, &self.ast)
			}
			// `return <value>;` (Slice 4E, Y1). The `assert!` is the load-bearing
			// half of the scope guard: a `Return` reached while `in_iife_subexpr` is
			// set means it's transitively underneath a subexpression-position
			// block/if/match's IIFE (see `emit_expr`'s control-flow arm) — a JS
			// `return` there would return from the IIFE, not the enclosing
			// function/method, so this must panic loudly rather than emit
			// quietly-wrong control flow.
			HirStmt::Return(value) => {
				assert!(
					!self.in_iife_subexpr.get(),
					"slice-4e: `return` inside an expression-position block/if/match would return from the emitted IIFE, not the enclosing function"
				);
				let value_expr = value.as_ref().map(|v| self.emit_expr(v));
				Statement::new_return_statement(SPAN, value_expr, &self.ast)
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
				let tail_expr = match tail {
					Some(tail) => self.emit_expr(tail),
					None => Expression::new_identifier(SPAN, "undefined", &self.ast),
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
			HirExpr::While { cond, body } => {
				// A `while` is a statement; its value is `undefined`.
				let cond_expr = self.emit_expr(cond);
				let body_stmt = self.block_stmt(body);
				let while_stmt = Statement::new_while_statement(SPAN, cond_expr, body_stmt, &self.ast);
				let mut stmts = ArenaVec::new_in(&self.ast);
				stmts.push(while_stmt);
				JsValue {
					stmts,
					expr: Expression::new_identifier(SPAN, "undefined", &self.ast),
				}
			}
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
					let (test, binds) = self.compile_pat(&arm.pat, &subj);
					let guard = arm.guard.as_ref().map(|g| self.emit_expr(g));
					// An unguarded last arm is the guaranteed fallback (exhaustiveness) → no
					// test, no break. Any other arm commits then breaks, guarded by its test.
					if is_last && arm.guard.is_none() {
						body.push(self.match_arm(r, &binds, &arm.body, None, None, None));
					} else {
						body.push(self.match_arm(r, &binds, &arm.body, guard, test, Some(label)));
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
				let object = self.emit_subject(base);
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
				let arr = self.emit_subject(base);
				let len = Expression::new_static_member_expression(
					SPAN,
					self.emit_subject(base),
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
				self.member_call(map, "get", vec![self.emit_lit(key)])
			}
			Subject::Slice(base, start, end_from_end) => {
				let arr = self.emit_subject(base);
				let start_lit = Expression::new_numeric_literal(
					SPAN,
					*start as f64,
					None,
					NumberBase::Decimal,
					&self.ast,
				);
				if *end_from_end == 0 {
					self.member_call(arr, "slice", vec![start_lit])
				} else {
					let len = Expression::new_static_member_expression(
						SPAN,
						self.emit_subject(base),
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
				}
			}
			Subject::MapRest(base, keys) => {
				let map_expr = self.emit_subject(base);
				let callee = Expression::new_identifier(SPAN, "Map", &self.ast);
				let mut args = ArenaVec::new_in(&self.ast);
				args.push(Argument::from(map_expr));
				let new_map = Expression::new_new_expression(SPAN, callee, oxc::ast::NONE, args, &self.ast);
				if keys.is_empty() {
					return new_map;
				}
				// `(() => { const _tN = new Map(<base>); _tN.delete(<k1>); ...; return _tN; })()`
				let tmp = self.ast.allocator.alloc_str(&self.gensym());
				let mut stmts = ArenaVec::new_in(&self.ast);
				stmts.push(self.const_decl(tmp, new_map));
				for key in keys {
					let m_ident = Expression::new_identifier(SPAN, tmp, &self.ast);
					let del = self.member_call(m_ident, "delete", vec![self.emit_lit(key)]);
					stmts.push(Statement::new_expression_statement(SPAN, del, &self.ast));
				}
				let value = JsValue {
					stmts,
					expr: Expression::new_identifier(SPAN, tmp, &self.ast),
				};
				value.into_expression(self.ast)
			}
		}
	}

	/// A scalar pattern literal as a JS expression (for `=== <lit>` tests).
	fn emit_lit(&self, lit: &HirLit) -> Expression<'a> {
		match lit {
			HirLit::Num(v) => {
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

	/// `<obj>[TAG]` (optional-chained when `optional`), reading the variant tag.
	fn tag_read(&self, obj: Expression<'a>, optional: bool) -> Expression<'a> {
		Expression::ComputedMemberExpression(ComputedMemberExpression::boxed(
			SPAN,
			obj,
			Expression::new_identifier(SPAN, "TAG", &self.ast),
			optional,
			&self.ast,
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
				let test = Expression::new_binary_expression(
					SPAN,
					subject,
					BinaryOperator::StrictEquality,
					value,
					&self.ast,
				);
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
				let mut test = Expression::new_binary_expression(
					SPAN,
					subject_tag,
					BinaryOperator::StrictEquality,
					variant_tag,
					&self.ast,
				);
				let mut binds = Vec::new();
				for (field, sub) in fields {
					let field_subj = Subject::Field(Box::new(subj.clone()), field.to_string());
					let (t, mut b) = self.compile_pat(sub, &field_subj);
					binds.append(&mut b);
					if let Some(t) = t {
						test =
							Expression::new_logical_expression(SPAN, test, LogicalOperator::And, t, &self.ast);
					}
				}
				(Some(test), binds)
			}
			// A struct pattern is irrefutable at its own level (nominal type guarantees
			// the shape); a field sub-pattern may still contribute a test.
			HirPat::Struct { fields } => {
				let mut test: Option<Expression<'a>> = None;
				let mut binds = Vec::new();
				for (field, sub) in fields {
					let field_subj = Subject::Field(Box::new(subj.clone()), field.to_string());
					let (t, mut b) = self.compile_pat(sub, &field_subj);
					binds.append(&mut b);
					test = self.and_test(test, t);
				}
				(test, binds)
			}
			// A tuple pattern binds by index; irrefutable at its own level.
			HirPat::Tuple(elems) => {
				let mut test: Option<Expression<'a>> = None;
				let mut binds = Vec::new();
				for (i, sub) in elems.iter().enumerate() {
					let elem_subj = Subject::Index(Box::new(subj.clone()), i);
					let (t, mut b) = self.compile_pat(sub, &elem_subj);
					binds.append(&mut b);
					test = self.and_test(test, t);
				}
				(test, binds)
			}
			// A list pattern: a length test (exact or `>=`), element bindings by index
			// (prefix from the front, suffix from the end), and an optional rest slice.
			HirPat::List {
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
						self.emit_subject(subj),
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
				for (i, sub) in prefix.iter().enumerate() {
					let elem = Subject::Index(Box::new(subj.clone()), i);
					let (t, mut b) = self.compile_pat(sub, &elem);
					binds.append(&mut b);
					test = self.and_test(test, t);
				}
				let suf_len = suffix.len();
				for (j, sub) in suffix.iter().enumerate() {
					// The j-th suffix element is `suf_len - j` from the end.
					let elem = Subject::IndexFromEnd(Box::new(subj.clone()), suf_len - j);
					let (t, mut b) = self.compile_pat(sub, &elem);
					binds.append(&mut b);
					test = self.and_test(test, t);
				}
				if let Some(Some(name)) = rest {
					let slice = Subject::Slice(Box::new(subj.clone()), prefix.len(), suffix.len());
					binds.push((name.to_string(), slice));
				}
				(test, binds)
			}
			// A map pattern: for each `key: vpat`, test `_s.has(key)` and match `vpat`
			// against `_s.get(key)`; an optional `...rest` binds the rest-of-map.
			HirPat::Map { entries, rest } => {
				let mut test: Option<Expression<'a>> = None;
				let mut binds = Vec::new();
				for (key, vpat) in entries {
					let has = self.member_call(self.emit_subject(subj), "has", vec![self.emit_lit(key)]);
					test = self.and_test(test, Some(has));
					let val = Subject::MapGet(Box::new(subj.clone()), key.clone());
					let (t, mut b) = self.compile_pat(vpat, &val);
					binds.append(&mut b);
					test = self.and_test(test, t);
				}
				if let Some(Some(name)) = rest {
					let keys = entries.iter().map(|(k, _)| k.clone()).collect();
					let rest_subj = Subject::MapRest(Box::new(subj.clone()), keys);
					binds.push((name.to_string(), rest_subj));
				}
				(test, binds)
			}
			// A range pattern: bound comparisons against the subject.
			HirPat::Range(range) => (Some(self.compile_range(range, subj)), Vec::new()),
			// A union: matches if either side matches. 3B unions bind nothing.
			HirPat::Or(a, b) => {
				let (ta, ba) = self.compile_pat(a, subj);
				let (tb, bb) = self.compile_pat(b, subj);
				// Lowering already rejects binding unions; this is a defensive check.
				debug_assert!(
					ba.is_empty() && bb.is_empty(),
					"union patterns cannot bind (should be rejected in lowering)"
				);
				// A `None` sub-test means that side is irrefutable ⇒ the whole `Or` is.
				let test = match (ta, tb) {
					(Some(a), Some(b)) => Some(Expression::new_logical_expression(
						SPAN,
						a,
						LogicalOperator::Or,
						b,
						&self.ast,
					)),
					_ => None,
				};
				(test, Vec::new())
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
				me.emit_subject(subj),
				&me.ast,
			)
		};
		// `<subj> <op> <lit>`
		let lt = |me: &Self, lit: &HirLit, op: BinaryOperator| {
			Expression::new_binary_expression(SPAN, me.emit_subject(subj), op, me.emit_lit(lit), &me.ast)
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
		body: &HirExpr,
		guard: Option<Expression<'a>>,
		test: Option<Expression<'a>>,
		label: Option<&'a str>,
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
		if let Some(label) = label {
			commit.push(Statement::new_break_statement(
				SPAN,
				Some(LabelIdentifier::new(SPAN, label, &self.ast)),
				&self.ast,
			));
		}
		let committed = match guard {
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
			block.push(self.const_decl(name, init));
		}
		block.push(committed);
		let block = Statement::new_block_statement(SPAN, block, &self.ast);
		match test {
			Some(test) => Statement::new_if_statement(SPAN, test, block, None, &self.ast),
			None => block,
		}
	}

	fn emit_binary(&self, op: BinOp, left: Expression<'a>, right: Expression<'a>) -> Expression<'a> {
		match op {
			// Logical operators are a distinct oxc node from binary operators.
			BinOp::And | BinOp::Or => Expression::LogicalExpression(LogicalExpression::boxed(
				SPAN,
				left,
				if op == BinOp::And {
					LogicalOperator::And
				} else {
					LogicalOperator::Or
				},
				right,
				&self.ast,
			)),
			_ => Expression::BinaryExpression(BinaryExpression::boxed(
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
			)),
		}
	}
}
