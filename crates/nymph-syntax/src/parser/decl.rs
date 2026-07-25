//! Parsing of top-level and member declarations.

use crate::errors::ParseError;
use ecow::EcoString;
use nymph_ast::{
	Ident, Spanned,
	decl::{
		Declaration, EnumVariant, FuncDeclaration, FuncKind, FuncParam, ImplMember, ImportRoot,
		InterfaceElement, InterfaceMember, LetDeclaration, LetKind, StructField, StructImpl,
		TypeAliasDeclaration, Visibility,
	},
	expr::{Expr, ExprKind},
	token::Token,
	ty::{GenericArg, Type},
};

use super::Parser;

impl Parser<'_> {
	pub(super) fn parse_module_members(&mut self) -> Vec<Declaration> {
		let mut members = Vec::new();
		while !self.at_end() {
			let before = self.position();
			if let Some(decl) = self.parse_declaration() {
				members.push(decl);
			}
			// Guarantee forward progress even on unrecognised input.
			if self.position() == before {
				self.advance();
				self.recover_to_declaration();
			}
		}
		members
	}

	fn parse_visibility(&mut self) -> Option<Visibility> {
		if self.eat(&Token::Public).is_some() {
			Some(Visibility::Public)
		} else if self.eat(&Token::Internal).is_some() {
			Some(Visibility::Internal)
		} else if self.eat(&Token::Private).is_some() {
			Some(Visibility::Private)
		} else {
			None
		}
	}

	fn parse_declaration(&mut self) -> Option<Declaration> {
		if self.check(&Token::Import) {
			return Some(self.parse_import());
		}
		let visibility = self.parse_visibility();
		match self.peek() {
			Some(Token::External) => Some(self.parse_external(visibility)),
			Some(Token::Let) => Some(self.parse_let_decl(visibility)),
			Some(Token::Func) => Some(self.parse_func_decl(visibility, FuncKind::Instance)),
			// `mut func` at module level has no receiver to mutate — diagnose, but
			// still parse it so recovery stays clean.
			Some(Token::Mut) => {
				self.emit(
					self.current_span(),
					ParseError::FuncKindOutsideType {
						kind: "mut func".into(),
					},
				);
				Some(self.parse_func_decl(visibility, FuncKind::Mut))
			}
			// `namespace` is overloaded: `namespace func …` / `namespace let …` are
			// (misplaced at top level) namespaced members, while `namespace Name { … }`
			// is a named namespace block. Disambiguate on the following token.
			Some(Token::Namespace) => match self.peek_nth(1) {
				Some(Token::Func) => {
					self.emit(
						self.current_span(),
						ParseError::FuncKindOutsideType {
							kind: "namespace func".into(),
						},
					);
					Some(self.parse_func_decl(visibility, FuncKind::Namespace))
				}
				Some(Token::Let) => {
					self.emit(
						self.current_span(),
						ParseError::FuncKindOutsideType {
							kind: "namespace let".into(),
						},
					);
					self.advance(); // `namespace`
					let (meta, value) = self.parse_let_binding_kinded(true);
					Some(Declaration::Let {
						visibility,
						meta,
						value,
					})
				}
				_ => Some(self.parse_namespace(visibility)),
			},
			Some(Token::Type) => Some(self.parse_type_alias(visibility)),
			Some(Token::Struct) => Some(self.parse_struct(visibility)),
			Some(Token::Enum) => Some(self.parse_enum(visibility)),
			Some(Token::Interface) => Some(self.parse_interface(visibility)),
			Some(Token::Impl) => Some(self.parse_impl(visibility)),
			_ => {
				let span = self.current_span();
				let found = self.peek().map_or("end of input", Token::describe);
				self.emit(
					span,
					ParseError::ExpectedDeclaration {
						found: found.into(),
					},
				);
				None
			}
		}
	}

	fn parse_import(&mut self) -> Declaration {
		self.advance(); // `import`
		let root = if self.eat(&Token::At).is_some() {
			self.expect(&Token::Slash);
			ImportRoot::Project
		} else if self.eat(&Token::DotDot).is_some() {
			self.expect(&Token::Slash);
			ImportRoot::Parent
		} else if self.eat(&Token::Dot).is_some() {
			self.expect(&Token::Slash);
			ImportRoot::Current
		} else {
			let pkg = self.expect_ident();
			if self.check(&Token::Slash) {
				self.advance();
			}
			ImportRoot::Package(pkg)
		};

		let mut path = Vec::new();
		if matches!(self.peek(), Some(Token::Identifier(_))) {
			loop {
				path.push(self.expect_ident());
				if self.eat(&Token::Slash).is_none() {
					break;
				}
			}
		}

		let alias = if self.eat(&Token::As).is_some() {
			Some(self.expect_ident())
		} else {
			None
		};

		let idents = if self.eat(&Token::With).is_some() {
			self.expect(&Token::LParen);
			Some(self.comma_separated(&Token::RParen, |p| {
				let name = p.expect_ident();
				let alias = if p.eat(&Token::As).is_some() {
					Some(p.expect_ident())
				} else {
					None
				};
				(name, alias)
			}))
		} else {
			None
		};

		Declaration::Import {
			root,
			path,
			alias,
			idents,
		}
	}

	/// Parse `let [mut] pattern [: type] = expr`, shared by declarations and statements.
	pub(super) fn parse_let_binding(&mut self) -> (LetDeclaration, Expr) {
		self.parse_let_binding_kinded(false)
	}

	/// Parse a `[namespace] let [mut] …` binding. When `namespaced`, the caller has
	/// already consumed the `namespace` keyword; a `mut` is then rejected (a static
	/// binding can never be mutable — `namespace let mut` is unrepresentable).
	fn parse_let_binding_kinded(&mut self, namespaced: bool) -> (LetDeclaration, Expr) {
		self.advance(); // `let`
		let mut_span = self.eat(&Token::Mut);
		let kind = if namespaced {
			if let Some(span) = mut_span {
				self.emit(span, ParseError::MutableNamespaceLet);
			}
			LetKind::Namespace
		} else if mut_span.is_some() {
			LetKind::Mut
		} else {
			LetKind::Instance
		};
		let name = self.parse_let_binding_pattern();
		let type_ = if self.eat(&Token::Colon).is_some() {
			Some(self.parse_type())
		} else {
			None
		};
		self.expect(&Token::Eq);
		let value = self.parse_expr();
		(LetDeclaration { kind, name, type_ }, value)
	}

	fn parse_let_decl(&mut self, visibility: Option<Visibility>) -> Declaration {
		let (meta, value) = self.parse_let_binding();
		Declaration::Let {
			visibility,
			meta,
			value,
		}
	}

	/// If the cursor is at the start of a function member — `func`, `mut func`, or
	/// `namespace func` — return its [`FuncKind`], else `None`.
	fn func_kind_here(&self) -> Option<FuncKind> {
		match self.peek() {
			Some(Token::Func) => Some(FuncKind::Instance),
			Some(Token::Mut) if self.peek_nth(1) == Some(&Token::Func) => Some(FuncKind::Mut),
			Some(Token::Namespace) if self.peek_nth(1) == Some(&Token::Func) => Some(FuncKind::Namespace),
			_ => None,
		}
	}

	/// Parse the signature `name<generics>(params) [: return_type]`.
	fn parse_func_signature(&mut self, kind: FuncKind) -> FuncDeclaration {
		match kind {
			FuncKind::Namespace | FuncKind::Mut => {
				self.advance();
			} // `namespace` or `mut`
			FuncKind::Instance => {}
		};
		self.advance(); // `func`
		let name = self.expect_ident();
		let generics = self.parse_generic_params();
		self.expect(&Token::LParen);
		let params = self.comma_separated(&Token::RParen, |p| p.parse_func_param());
		let return_type = if self.eat(&Token::Colon).is_some() {
			Some(self.parse_type())
		} else {
			None
		};
		FuncDeclaration {
			kind,
			name,
			generics,
			params,
			return_type,
		}
	}

	fn parse_func_param(&mut self) -> Spanned<FuncParam> {
		let start = self.position();
		let spread = self.eat(&Token::DotDotDot).is_some();
		let mutable = self.eat(&Token::Mut).is_some();
		let name = self.parse_binding_pattern();
		self.expect(&Token::Colon);
		let type_ = self.parse_type();
		Spanned(
			FuncParam {
				name,
				type_,
				mutable,
				spread,
			},
			self.span_from(start),
		)
	}

	fn parse_func_decl(&mut self, visibility: Option<Visibility>, kind: FuncKind) -> Declaration {
		let meta = self.parse_func_signature(kind);
		self.expect(&Token::Eq);
		let body = self.parse_expr();
		Declaration::Func {
			visibility,
			meta,
			body,
		}
	}

	fn parse_external(&mut self, visibility: Option<Visibility>) -> Declaration {
		self.advance(); // `external`
		let explicit_name = if self.eat(&Token::LParen).is_some() {
			let name = self.expect_ident();
			self.expect(&Token::RParen);
			Some(name.0)
		} else {
			None
		};

		if let Some(kind) = self.func_kind_here() {
			// `external func`, `external mut func`, `external namespace func`.
			let meta = self.parse_func_signature(kind);
			let js_name = explicit_name.unwrap_or_else(|| meta.name.0.clone());
			Declaration::ExternalFunc(visibility, js_name, meta)
		} else {
			// external let: `external(name) let x: T`
			self.advance(); // `let`
			let kind = if self.eat(&Token::Mut).is_some() {
				LetKind::Mut
			} else {
				LetKind::Instance
			};
			let name = self.parse_binding_pattern();
			let type_ = if self.eat(&Token::Colon).is_some() {
				Some(self.parse_type())
			} else {
				None
			};
			let js_name = explicit_name
				.unwrap_or_else(|| name.0.as_binding().map(|i| i.0.clone()).unwrap_or_default());
			Declaration::ExternalLet(visibility, js_name, LetDeclaration { kind, name, type_ })
		}
	}

	fn parse_type_alias(&mut self, visibility: Option<Visibility>) -> Declaration {
		self.advance(); // `type`
		let name = self.expect_ident();
		let generics = self.parse_generic_params();
		self.expect(&Token::Eq);
		let value = self.parse_type();
		Declaration::TypeAlias {
			visibility,
			meta: TypeAliasDeclaration { name, generics },
			value,
		}
	}

	fn parse_struct(&mut self, visibility: Option<Visibility>) -> Declaration {
		self.advance(); // `struct`
		let name = self.expect_ident();
		let generics = self.parse_generic_params();
		let fields = if self.check(&Token::LParen) {
			self.advance();
			self.comma_separated(&Token::RParen, |p| p.parse_struct_field())
		} else {
			Vec::new()
		};
		let (members, impls) = if self.check(&Token::LBrace) {
			self.advance();
			let m = self.parse_inner_members();
			self.expect(&Token::RBrace);
			m
		} else {
			(Vec::new(), Vec::new())
		};
		Declaration::Struct {
			visibility,
			name,
			generics,
			fields,
			members,
			impls,
		}
	}

	fn parse_struct_field(&mut self) -> Spanned<StructField> {
		let start = self.position();
		let visibility = self.parse_visibility();
		if let Some(span) = self.eat(&Token::Mut) {
			self.emit(span, ParseError::MutFieldModifier);
		}
		let name = self.expect_ident();
		self.expect(&Token::Colon);
		let type_ = self.parse_type();
		let default = if self.eat(&Token::Eq).is_some() {
			Some(self.parse_expr())
		} else {
			None
		};
		Spanned(
			StructField {
				visibility,
				name,
				type_,
				default,
			},
			self.span_from(start),
		)
	}

	fn parse_enum(&mut self, visibility: Option<Visibility>) -> Declaration {
		self.advance(); // `enum`
		let name = self.expect_ident();
		let generics = self.parse_generic_params();
		self.expect(&Token::LBrace);

		let mut variants = Vec::new();
		// Variants come first: a bare identifier that is not a member keyword.
		while matches!(self.peek(), Some(Token::Identifier(_))) {
			let start = self.position();
			let variant_name = self.expect_ident();
			let fields = if self.check(&Token::LParen) {
				self.advance();
				self.comma_separated(&Token::RParen, |p| p.parse_struct_field())
			} else {
				Vec::new()
			};
			variants.push(Spanned(
				EnumVariant {
					name: variant_name,
					fields,
				},
				self.span_from(start),
			));
			if self.eat(&Token::Comma).is_none() {
				break;
			}
		}

		let (members, impls) = self.parse_inner_members();
		self.expect(&Token::RBrace);

		Declaration::Enum {
			visibility,
			name,
			generics,
			variants,
			members,
			impls,
		}
	}

	/// Parse the members inside a `struct` / `enum` body (until, but not consuming,
	/// `}`), splitting flat members (instance / `mut` / `namespace` funcs and lets)
	/// from nested interface `impl` blocks.
	fn parse_inner_members(&mut self) -> (Vec<Spanned<ImplMember>>, Vec<Spanned<StructImpl>>) {
		let mut members = Vec::new();
		let mut impls = Vec::new();
		while !self.check(&Token::RBrace) && !self.at_end() {
			let start = self.position();
			if self.check(&Token::Impl) {
				self.advance();
				let generics = self.parse_generic_params();
				let interface = self.parse_interface_ref();
				self.expect(&Token::LBrace);
				let inner = self.parse_impl_members();
				self.expect(&Token::RBrace);
				impls.push(Spanned(
					StructImpl {
						interface,
						generics,
						members: inner,
					},
					self.span_from(start),
				));
			} else {
				let before = self.position();
				let inner = self.parse_impl_member();
				if self.position() == before {
					// no progress: bail to avoid an infinite loop
					self.advance();
					continue;
				}
				members.push(inner);
			}
		}
		(members, impls)
	}

	fn parse_impl_members(&mut self) -> Vec<Spanned<ImplMember>> {
		let mut members = Vec::new();
		while !self.check(&Token::RBrace) && !self.at_end() {
			let before = self.position();
			members.push(self.parse_impl_member());
			if self.position() == before {
				self.advance();
			}
		}
		members
	}

	fn parse_impl_member(&mut self) -> Spanned<ImplMember> {
		let start = self.position();
		let visibility = self.parse_visibility();
		let member = if self.check(&Token::External) {
			match self.parse_external(visibility) {
				Declaration::ExternalFunc(v, n, m) => ImplMember::ExternalFunc(v, n, m),
				Declaration::ExternalLet(v, n, m) => ImplMember::ExternalLet(v, n, m),
				_ => unreachable!(),
			}
		} else if let Some(kind) = self.func_kind_here() {
			let meta = self.parse_func_signature(kind);
			self.expect(&Token::Eq);
			let body = self.parse_expr();
			ImplMember::Func {
				visibility,
				meta,
				body,
			}
		} else if self.check(&Token::Let) {
			let (meta, value) = self.parse_let_binding();
			ImplMember::Let {
				visibility,
				meta,
				value,
			}
		} else if self.check(&Token::Namespace) && self.peek_nth(1) == Some(&Token::Let) {
			// `namespace let` — a static (type-level) binding.
			self.advance(); // `namespace`
			let (meta, value) = self.parse_let_binding_kinded(true);
			ImplMember::Let {
				visibility,
				meta,
				value,
			}
		} else {
			let span = self.current_span();
			let found = self.peek().map_or("end of input", Token::describe);
			self.emit(
				span,
				ParseError::ExpectedMember {
					found: found.into(),
				},
			);
			// Return a dummy so the caller can make progress.
			ImplMember::Let {
				visibility,
				meta: LetDeclaration {
					kind: LetKind::Instance,
					name: Spanned(nymph_ast::expr::Pattern::Placeholder, span),
					type_: None,
				},
				value: self.mk_expr(ExprKind::Tuple(Vec::new()), span),
			}
		};
		Spanned(member, self.span_from(start))
	}

	fn parse_interface(&mut self, visibility: Option<Visibility>) -> Declaration {
		self.advance(); // `interface`
		let name = self.expect_ident();
		let generics = self.parse_generic_params();
		let mut super_interfaces = Vec::new();
		if self.eat(&Token::Colon).is_some() {
			loop {
				let start = self.position();
				let r = self.parse_interface_ref();
				super_interfaces.push(Spanned(r, self.span_from(start)));
				if self.eat(&Token::Comma).is_none() {
					break;
				}
			}
		}
		self.expect(&Token::LBrace);
		let mut members = Vec::new();
		while !self.check(&Token::RBrace) && !self.at_end() {
			let before = self.position();
			if let Some(member) = self.parse_interface_member() {
				members.push(member);
			}
			if self.position() == before {
				self.advance();
			}
		}
		self.expect(&Token::RBrace);
		Declaration::Interface {
			visibility,
			name,
			generics,
			super_interfaces,
			members,
		}
	}

	fn parse_interface_member(&mut self) -> Option<Spanned<InterfaceMember>> {
		let start = self.position();
		if let Some(kind) = self.func_kind_here() {
			let meta = self.parse_func_signature(kind);
			let body = if self.eat(&Token::Eq).is_some() {
				Some(self.parse_expr())
			} else {
				None
			};
			Some(Spanned(
				InterfaceMember::Element(Box::new(Spanned(
					InterfaceElement::Func { meta, body },
					self.span_from(start),
				))),
				self.span_from(start),
			))
		} else if self.check(&Token::Let) {
			self.advance(); // `let`
			let kind = if self.eat(&Token::Mut).is_some() {
				LetKind::Mut
			} else {
				LetKind::Instance
			};
			let name = self.parse_binding_pattern();
			let type_ = if self.eat(&Token::Colon).is_some() {
				Some(self.parse_type())
			} else {
				None
			};
			let value = if self.eat(&Token::Eq).is_some() {
				Some(self.parse_expr())
			} else {
				None
			};
			Some(Spanned(
				InterfaceMember::Element(Box::new(Spanned(
					InterfaceElement::Let {
						meta: LetDeclaration { kind, name, type_ },
						value,
					},
					self.span_from(start),
				))),
				self.span_from(start),
			))
		} else if self.check(&Token::Impl) {
			self.advance();
			let generics = self.parse_generic_params();
			let interface = self.parse_interface_ref();
			self.expect(&Token::LBrace);
			let inner = self.parse_impl_members();
			self.expect(&Token::RBrace);
			Some(Spanned(
				InterfaceMember::Impl {
					interface,
					generics,
					members: inner,
				},
				self.span_from(start),
			))
		} else {
			let span = self.current_span();
			let found = self.peek().map_or("end of input", Token::describe);
			self.emit(
				span,
				ParseError::ExpectedInterfaceMember {
					found: found.into(),
				},
			);
			None
		}
	}

	fn parse_namespace(&mut self, visibility: Option<Visibility>) -> Declaration {
		self.advance(); // `namespace`
		let name = self.expect_ident();
		self.expect(&Token::LBrace);
		let members = self.parse_impl_members();
		self.expect(&Token::RBrace);
		// A namespace has no `this` to mutate and nesting another namespace is
		// pointless, so it may only hold regular `func`s and `let`s.
		for m in &members {
			match &m.0 {
				ImplMember::Func { meta, .. } => {
					let kind = match meta.kind {
						FuncKind::Mut => "mut func",
						FuncKind::Namespace => "namespace func",
						FuncKind::Instance => continue,
					};
					self.emit(
						meta.name.1,
						ParseError::FuncKindInNamespace { kind: kind.into() },
					);
				}
				ImplMember::Let { meta, .. } if meta.is_namespaced() => {
					self.emit(
						meta.name.1,
						ParseError::FuncKindInNamespace {
							kind: "namespace let".into(),
						},
					);
				}
				_ => {}
			}
		}
		Declaration::Namespace {
			visibility,
			name,
			members,
		}
	}

	fn parse_impl(&mut self, visibility: Option<Visibility>) -> Declaration {
		self.advance(); // `impl`
		let generics = self.parse_generic_params();
		let mutable = self.eat(&Token::Mut).is_some();
		let first = self.parse_type();

		if self.eat(&Token::For).is_some() {
			// `first` was the interface; the target type follows `for`.
			let for_interface = self.type_to_interface_ref(first);
			let type_ = self.parse_type();
			self.expect(&Token::LBrace);
			let members = self.parse_impl_members();
			self.expect(&Token::RBrace);
			Declaration::ImplFor {
				visibility,
				generics,
				mutable,
				type_,
				for_interface,
				members,
			}
		} else {
			self.expect(&Token::LBrace);
			let members = self.parse_impl_members();
			self.expect(&Token::RBrace);
			Declaration::Impl {
				visibility,
				generics,
				mutable,
				type_: first,
				members,
			}
		}
	}

	/// Parse an interface reference `Name<args>` into an `(Ident, generic args)` pair.
	fn parse_interface_ref(&mut self) -> (Ident, Vec<Spanned<GenericArg>>) {
		let name = self.expect_ident();
		let generics = self.parse_generic_args();
		(name, generics)
	}

	fn type_to_interface_ref(&mut self, ty: Spanned<Type>) -> (Ident, Vec<Spanned<GenericArg>>) {
		match ty.0 {
			Type::Reference { name, generics } => (name, generics),
			_ => {
				self.emit(ty.1, ParseError::ExpectedInterfaceName);
				(Spanned(EcoString::new(), ty.1), Vec::new())
			}
		}
	}
}
