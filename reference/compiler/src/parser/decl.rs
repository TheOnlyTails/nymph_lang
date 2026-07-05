use ecow::EcoString;

use crate::{
	ast::{
		Ident, Spanned,
		declaration::{
			Declaration, EnumVariant, FuncDeclaration, FuncParam, ImplMember, ImportRoot,
			InterfaceElement, InterfaceMember, LetDeclaration, Module, StructField, StructInnerMember,
			TypeAliasDeclaration, Visibility,
		},
		expr::Pattern,
	},
	lexer::token::Token,
	parser::error::ParseErrorKind,
};

use super::{
	core::{Parser, span},
	error::ParseError,
};

impl<'src> Parser<'src> {
	pub fn parse_module(&mut self) -> Spanned<Module> {
		let start_span = self.current_span();
		let mut members = Vec::new();

		while !self.at_end() {
			match self.parse_declaration() {
				Some(decl) => members.push(decl),
				None => {
					self.synchronize_to_declaration();
				}
			}
		}

		let end_span = if members.is_empty() {
			start_span
		} else {
			self.previous_span()
		};

		Spanned(
			Module {
				members,
				path: self.file_path.clone(),
			},
			span(start_span.start, end_span.end),
		)
	}

	pub fn parse_declaration(&mut self) -> Option<Declaration> {
		match self.peek()? {
			Token::Import => self.parse_import(),
			Token::Public | Token::Internal | Token::Private => {
				let visibility = self.parse_visibility();
				self.parse_declaration_with_visibility(visibility)
			}
			Token::External => self.parse_external_declaration(None),
			Token::Let => self.parse_let_declaration_top(),
			Token::Func => self.parse_func_declaration_top(None),
			Token::Type => self.parse_type_alias(None),
			Token::Struct => self.parse_struct(None),
			Token::Enum => self.parse_enum(None),
			Token::Namespace => self.parse_namespace(None),
			Token::Interface => self.parse_interface(None),
			Token::Impl => self.parse_impl_or_impl_for(None),
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error(ParseError::expected_declaration(found, self.current_span()));
				None
			}
		}
	}

	fn parse_declaration_with_visibility(
		&mut self,
		visibility: Option<Visibility>,
	) -> Option<Declaration> {
		match self.peek()? {
			Token::External => self.parse_external_declaration(visibility),
			Token::Let => self.parse_let_declaration_with_visibility(visibility),
			Token::Func => self.parse_func_declaration_top(visibility),
			Token::Type => self.parse_type_alias(visibility),
			Token::Struct => self.parse_struct(visibility),
			Token::Enum => self.parse_enum(visibility),
			Token::Namespace => self.parse_namespace(visibility),
			Token::Interface => self.parse_interface(visibility),
			Token::Impl => self.parse_impl_or_impl_for(visibility),
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error(ParseError::expected_declaration(found, self.current_span()));
				None
			}
		}
	}

	fn parse_external_declaration(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		self.expect(&Token::External, "external")?;

		let external_name = if matches!(self.peek(), Some(Token::Parens(_))) {
			Some(self.parse_external_name()?)
		} else {
			None
		};

		match self.peek()? {
			Token::Let => {
				let meta = self.parse_let_meta()?;
				let external_name = external_name
					.or_else(|| shorthand_external_name(&meta.name.0))
					.unwrap_or_default();
				Some(Declaration::ExternalLet(visibility, external_name, meta))
			}
			Token::Func => {
				let meta = self.parse_func_meta()?;
				let external_name = external_name.unwrap_or_else(|| meta.name.0.clone());
				Some(Declaration::ExternalFunc(visibility, external_name, meta))
			}
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error({
					let expected = vec!["let".into(), "func".into()];
					let span = self.current_span();
					ParseError {
						kind: ParseErrorKind::UnexpectedToken { found, expected },
						span,
						context: vec![],
					}
				});
				None
			}
		}
	}

	fn parse_external_name(&mut self) -> Option<EcoString> {
		let Token::Parens(inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected external name in parentheses, e.g. `external(my_export)`",
				self.current_span(),
			));
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		self.with_nested(&inner, parens_span, |p| {
			p.expect_identifier().map(|ident| ident.0)
		})
	}

	fn parse_visibility(&mut self) -> Option<Visibility> {
		match self.peek()? {
			Token::Public => {
				self.advance();
				Some(Visibility::Public)
			}
			Token::Internal => {
				self.advance();
				Some(Visibility::Internal)
			}
			Token::Private => {
				self.advance();
				Some(Visibility::Private)
			}
			_ => None,
		}
	}

	fn parse_import(&mut self) -> Option<Declaration> {
		self.expect(&Token::Import, "import")?;

		let root = match self.peek()? {
			Token::AtSign => {
				self.advance();
				ImportRoot::Project
			}
			Token::DotDot => {
				self.advance();
				ImportRoot::Parent
			}
			Token::Dot => {
				self.advance();
				ImportRoot::Current
			}
			Token::Identifier(_) => {
				let ident = self.identifier()?;
				ImportRoot::Package(ident)
			}
			_ => {
				let found = self.peek().cloned().unwrap();
				self.error({
					let expected = vec!["@".into(), "..".into(), ".".into(), "package name".into()];
					let span = self.current_span();
					ParseError {
						kind: ParseErrorKind::UnexpectedToken { found, expected },
						span,
						context: vec![],
					}
				});
				return None;
			}
		};

		let mut path = Vec::new();
		while self.consume(&Token::Slash).is_some() {
			path.push(self.expect_identifier()?);
		}

		let idents = if self.consume(&Token::With).is_some() {
			let Token::Parens(inner) = self.peek()?.clone() else {
				self.error(ParseError::custom(
					"expected import items in parentheses",
					self.current_span(),
				));
				return None;
			};
			let parens_span = self.current_span();
			self.advance();

			Some(self.with_nested(&inner, parens_span, |p| p.parse_import_idents()))
		} else {
			None
		};

		Some(Declaration::Import { root, path, idents })
	}

	fn parse_import_idents(&mut self) -> Vec<(Ident, Option<Ident>)> {
		let mut idents = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let Some(name) = self.identifier() else {
				break;
			};

			let alias = if self.consume(&Token::As).is_some() {
				self.expect_identifier()
			} else {
				None
			};

			idents.push((name, alias));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		idents
	}

	fn parse_let_declaration_top(&mut self) -> Option<Declaration> {
		let meta = self.parse_let_meta()?;
		self.expect(&Token::Eq, "=")?;
		let value = self.parse_expression()?;

		Some(Declaration::Let {
			visibility: None,
			meta,
			value,
		})
	}

	fn parse_let_declaration_with_visibility(
		&mut self,
		visibility: Option<Visibility>,
	) -> Option<Declaration> {
		let meta = self.parse_let_meta()?;
		self.expect(&Token::Eq, "=")?;
		let value = self.parse_expression()?;

		Some(Declaration::Let {
			visibility,
			meta,
			value,
		})
	}

	fn parse_let_meta(&mut self) -> Option<LetDeclaration> {
		self.expect(&Token::Let, "let")?;
		let mutable = self.consume(&Token::Mut).is_some();
		let name = self.parse_pattern()?;
		let type_ = if self.consume(&Token::Colon).is_some() {
			Some(self.parse_type()?)
		} else {
			None
		};

		Some(LetDeclaration {
			mutable,
			name,
			type_,
		})
	}

	fn parse_func_declaration_top(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		let meta = self.parse_func_meta()?;
		self.expect(&Token::Arrow, "->")?;
		let body = self.parse_expression()?;

		Some(Declaration::Func {
			visibility,
			meta,
			body,
		})
	}

	fn parse_func_meta(&mut self) -> Option<FuncDeclaration> {
		self.expect(&Token::Func, "func")?;
		let name = self.expect_identifier()?;
		let generics = self.parse_generic_params().unwrap_or_default();

		let Token::Parens(inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected function parameters",
				self.current_span(),
			));
			return None;
		};
		let parens_span = self.current_span();
		self.advance();

		let params = self.with_nested(&inner, parens_span, |p| p.parse_func_params());

		let return_type = if self.consume(&Token::Colon).is_some() {
			Some(self.parse_type()?)
		} else {
			None
		};

		Some(FuncDeclaration {
			name,
			generics,
			params,
			return_type,
		})
	}

	fn parse_func_params(&mut self) -> Vec<Spanned<FuncParam>> {
		let mut params = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();

			let spread = self.consume(&Token::DotDotDot).is_some();
			let mutable = self.consume(&Token::Mut).is_some();

			let Some(name) = self.parse_pattern() else {
				break;
			};

			if self.expect(&Token::Colon, ":").is_none() {
				break;
			}

			let Some(type_) = self.parse_type() else {
				break;
			};

			let end_span = type_.1;
			params.push(Spanned(
				FuncParam {
					spread,
					mutable,
					name,
					type_,
				},
				span(start_span.start, end_span.end),
			));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		params
	}

	fn parse_type_alias(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		self.expect(&Token::Type, "type")?;
		let name = self.expect_identifier()?;
		let generics = self.parse_generic_params().unwrap_or_default();

		self.expect(&Token::Eq, "=")?;
		let value = self.parse_type()?;

		Some(Declaration::TypeAlias {
			visibility,
			meta: TypeAliasDeclaration { name, generics },
			value,
		})
	}

	fn parse_struct(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		self.expect(&Token::Struct, "struct")?;
		let name = self.expect_identifier()?;
		let generics = self.parse_generic_params().unwrap_or_default();

		let fields = if let Some(Token::Parens(inner)) = self.peek() {
			let inner = inner.clone();
			let parens_span = self.current_span();
			self.advance();
			self.with_nested(&inner, parens_span, |p| p.parse_struct_fields())
		} else {
			vec![]
		};

		let members = if let Some(Token::Braces(inner)) = self.peek() {
			let inner = inner.clone();
			let braces_span = self.current_span();
			self.advance();
			self.with_nested(&inner, braces_span, |p| p.parse_struct_members())
		} else {
			vec![]
		};

		Some(Declaration::Struct {
			visibility,
			name,
			generics,
			fields,
			members,
		})
	}

	fn parse_struct_fields(&mut self) -> Vec<Spanned<StructField>> {
		let mut fields = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			let start_span = self.current_span();
			let visibility = self.parse_visibility();

			let Some(name) = self.identifier() else {
				break;
			};

			if self.expect(&Token::Colon, ":").is_none() {
				break;
			}

			let Some(type_) = self.parse_type() else {
				break;
			};

			let default = if self.consume(&Token::Eq).is_some() {
				self.parse_expression()
			} else {
				None
			};

			let end_span = self.previous_span();
			fields.push(Spanned(
				StructField {
					visibility,
					name,
					type_,
					default,
				},
				span(start_span.start, end_span.end),
			));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		fields
	}

	fn parse_struct_members(&mut self) -> Vec<Spanned<StructInnerMember>> {
		let mut members = Vec::new();

		while !self.at_end() {
			let start_span = self.current_span();

			let member = if self.check(&Token::Namespace) {
				self.advance();
				let Some(Token::Braces(inner)) = self.peek().cloned() else {
					self.error(ParseError::custom(
						"expected namespace body",
						self.current_span(),
					));
					continue;
				};
				let braces_span = self.current_span();
				self.advance();

				let inner_members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());
				StructInnerMember::Namespace(inner_members)
			} else if self.check(&Token::Impl) {
				self.advance();

				if self.check(&Token::Mut) {
					self.advance();
					let Some(Token::Braces(inner)) = self.peek().cloned() else {
						self.error(ParseError::custom(
							"expected impl mut body",
							self.current_span(),
						));
						continue;
					};
					let braces_span = self.current_span();
					self.advance();

					let inner_members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());
					StructInnerMember::ImplMut(inner_members)
				} else {
					let generics = self.parse_generic_params().unwrap_or_default();
					let Some(interface_name) = self.expect_identifier() else {
						continue;
					};
					let interface_generics = self.parse_generic_args().unwrap_or_default();

					let Some(Token::Braces(inner)) = self.peek().cloned() else {
						self.error(ParseError::custom(
							"expected impl body",
							self.current_span(),
						));
						continue;
					};
					let braces_span = self.current_span();
					self.advance();

					let inner_members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());
					StructInnerMember::Impl {
						interface: (interface_name, interface_generics),
						generics,
						members: inner_members,
					}
				}
			} else {
				let Some(impl_member) = self.parse_impl_member() else {
					self.advance();
					continue;
				};
				StructInnerMember::Member(Box::new(impl_member))
			};

			let end_span = self.previous_span();
			members.push(Spanned(member, span(start_span.start, end_span.end)));
		}

		members
	}

	fn parse_enum(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		self.expect(&Token::Enum, "enum")?;
		let name = self.expect_identifier()?;
		let generics = self.parse_generic_params().unwrap_or_default();

		let Token::Braces(inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected enum body",
				self.current_span(),
			));
			return None;
		};
		let braces_span = self.current_span();
		self.advance();

		let (variants, members) = self.with_nested(&inner, braces_span, |p| {
			let variants = p.parse_enum_variants();
			let members = p.parse_struct_members();
			(variants, members)
		});

		Some(Declaration::Enum {
			visibility,
			name,
			generics,
			variants,
			members,
		})
	}

	fn parse_enum_variants(&mut self) -> Vec<Spanned<EnumVariant>> {
		let mut variants = Vec::new();

		loop {
			if self.at_end() {
				break;
			}

			if !matches!(self.peek(), Some(Token::Identifier(_))) {
				break;
			}

			if (self.peek_nth(1) == Some(&Token::Arrow) || self.peek_nth(1) == Some(&Token::Colon))
				&& self.peek_nth(2) != Some(&Token::Colon)
			{
				break;
			}

			let start_span = self.current_span();
			let Some(name) = self.identifier() else {
				break;
			};

			let fields = if let Some(Token::Parens(inner)) = self.peek() {
				let inner = inner.clone();
				let parens_span = self.current_span();
				self.advance();
				self.with_nested(&inner, parens_span, |p| p.parse_struct_fields())
			} else {
				vec![]
			};

			let end_span = self.previous_span();
			variants.push(Spanned(
				EnumVariant { name, fields },
				span(start_span.start, end_span.end),
			));

			if self.consume(&Token::Comma).is_none() {
				break;
			}
		}

		variants
	}

	fn parse_namespace(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		self.expect(&Token::Namespace, "namespace")?;
		let name = self.expect_identifier()?;

		let Token::Braces(inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected namespace body",
				self.current_span(),
			));
			return None;
		};
		let braces_span = self.current_span();
		self.advance();

		let members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());

		Some(Declaration::Namespace {
			visibility,
			name,
			members,
		})
	}

	fn parse_interface(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		self.expect(&Token::Interface, "interface")?;
		let name = self.expect_identifier()?;
		let generics = self.parse_generic_params().unwrap_or_default();

		let super_interfaces = if self.consume(&Token::Colon).is_some() {
			self.parse_super_interfaces()
		} else {
			vec![]
		};

		let Token::Braces(inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected interface body",
				self.current_span(),
			));
			return None;
		};
		let braces_span = self.current_span();
		self.advance();

		let members = self.with_nested(&inner, braces_span, |p| p.parse_interface_members());

		Some(Declaration::Interface {
			visibility,
			name,
			generics,
			super_interfaces,
			members,
		})
	}

	fn parse_super_interfaces(
		&mut self,
	) -> Vec<Spanned<(Ident, Vec<Spanned<crate::ast::types::GenericArg>>)>> {
		let mut interfaces = Vec::new();

		loop {
			let start_span = self.current_span();
			let Some(name) = self.identifier() else {
				break;
			};
			let generics = self.parse_generic_args().unwrap_or_default();
			let end_span = self.previous_span();
			interfaces.push(Spanned(
				(name, generics),
				span(start_span.start, end_span.end),
			));

			if self.consume(&Token::Plus).is_none() {
				break;
			}
		}

		interfaces
	}

	fn parse_interface_members(&mut self) -> Vec<Spanned<InterfaceMember>> {
		let mut members = Vec::new();

		while !self.at_end() {
			let start_span = self.current_span();

			let member = if self.check(&Token::Namespace) {
				self.advance();
				let Some(token) = self.peek() else {
					break;
				};
				let Token::Braces(inner) = token.clone() else {
					self.error(ParseError::custom(
						"expected namespace body",
						self.current_span(),
					));
					continue;
				};
				let braces_span = self.current_span();
				self.advance();

				let inner_members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());
				InterfaceMember::Namespace(inner_members)
			} else if self.check(&Token::Impl) {
				self.advance();

				if self.check(&Token::Mut) {
					self.advance();
					let Some(token) = self.peek() else {
						break;
					};
					let Token::Braces(inner) = token.clone() else {
						self.error(ParseError::custom(
							"expected impl mut body",
							self.current_span(),
						));
						continue;
					};
					let braces_span = self.current_span();
					self.advance();

					let inner_elements =
						self.with_nested(&inner, braces_span, |p| p.parse_interface_elements());
					InterfaceMember::ImplMut(inner_elements)
				} else {
					let generics = self.parse_generic_params().unwrap_or_default();
					let Some(interface_name) = self.expect_identifier() else {
						self.advance();
						continue;
					};
					let interface_generics = self.parse_generic_args().unwrap_or_default();

					let Some(token) = self.peek() else {
						break;
					};
					let Token::Braces(inner) = token.clone() else {
						self.error(ParseError::custom(
							"expected impl body",
							self.current_span(),
						));
						continue;
					};
					let braces_span = self.current_span();
					self.advance();

					let inner_members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());
					InterfaceMember::Impl {
						interface: (interface_name, interface_generics),
						generics,
						members: inner_members,
					}
				}
			} else {
				let Some(element) = self.parse_interface_element() else {
					self.advance();
					continue;
				};
				InterfaceMember::Element(Box::new(element))
			};

			let end_span = self.previous_span();
			members.push(Spanned(member, span(start_span.start, end_span.end)));
		}

		members
	}

	fn parse_interface_elements(&mut self) -> Vec<Spanned<InterfaceElement>> {
		let mut elements = Vec::new();

		while !self.at_end() {
			if let Some(element) = self.parse_interface_element() {
				elements.push(element);
			} else {
				self.advance();
			}
		}

		elements
	}

	fn parse_interface_element(&mut self) -> Option<Spanned<InterfaceElement>> {
		let start_span = self.current_span();

		let element = if self.check(&Token::Let) {
			self.advance();
			let mutable = self.consume(&Token::Mut).is_some();
			let name = self.parse_pattern()?;
			let type_ = if self.consume(&Token::Colon).is_some() {
				Some(self.parse_type()?)
			} else {
				None
			};

			let value = if self.consume(&Token::Eq).is_some() {
				Some(self.parse_expression()?)
			} else {
				None
			};

			InterfaceElement::Let {
				meta: LetDeclaration {
					mutable,
					name,
					type_,
				},
				value,
			}
		} else if self.check(&Token::Func) {
			let meta = self.parse_func_meta()?;
			let body = if self.consume(&Token::Arrow).is_some() {
				Some(self.parse_expression()?)
			} else {
				None
			};
			InterfaceElement::Func { meta, body }
		} else {
			return None;
		};

		let end_span = self.previous_span();
		Some(Spanned(element, span(start_span.start, end_span.end)))
	}

	fn parse_impl_or_impl_for(&mut self, visibility: Option<Visibility>) -> Option<Declaration> {
		self.expect(&Token::Impl, "impl")?;
		let generics = self.parse_generic_params().unwrap_or_default();
		let mutable = self.consume(&Token::Mut).is_some();

		let pos = self.position();
		let first_ident = self.identifier();
		let first_generics = self.parse_generic_args();

		if self.check(&Token::For) {
			self.advance();
			let type_ = self.parse_type()?;

			let Token::Braces(inner) = self.peek()?.clone() else {
				self.error(ParseError::custom(
					"expected impl body",
					self.current_span(),
				));
				return None;
			};
			let braces_span = self.current_span();
			self.advance();

			let members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());

			return Some(Declaration::ImplFor {
				visibility,
				generics,
				mutable,
				type_,
				for_interface: (first_ident?, first_generics.unwrap_or_default()),
				members,
			});
		}

		self.restore(pos);
		let type_ = self.parse_type()?;

		let Token::Braces(inner) = self.peek()?.clone() else {
			self.error(ParseError::custom(
				"expected impl body",
				self.current_span(),
			));
			return None;
		};
		let braces_span = self.current_span();
		self.advance();

		let members = self.with_nested(&inner, braces_span, |p| p.parse_impl_members());

		Some(Declaration::Impl {
			visibility,
			generics,
			mutable,
			type_,
			members,
		})
	}

	fn parse_impl_members(&mut self) -> Vec<Spanned<ImplMember>> {
		let mut members = Vec::new();

		while !self.at_end() {
			if let Some(member) = self.parse_impl_member() {
				members.push(member);
			} else {
				self.advance();
			}
		}

		members
	}

	fn parse_impl_member(&mut self) -> Option<Spanned<ImplMember>> {
		let start_span = self.current_span();
		let visibility = self.parse_visibility();

		let member = if self.check(&Token::External) {
			self.advance();
			let external_name = if matches!(self.peek(), Some(Token::Parens(_))) {
				Some(self.parse_external_name()?)
			} else {
				None
			};
			if self.check(&Token::Let) {
				self.advance();
				let mutable = self.consume(&Token::Mut).is_some();
				let name = self.parse_pattern()?;
				let type_ = if self.consume(&Token::Colon).is_some() {
					Some(self.parse_type()?)
				} else {
					None
				};
				let external_name = external_name
					.or_else(|| shorthand_external_name(&name.0))
					.unwrap_or_default();
				ImplMember::ExternalLet(
					visibility,
					external_name,
					LetDeclaration {
						mutable,
						name,
						type_,
					},
				)
			} else {
				let meta = self.parse_func_meta()?;
				let external_name = external_name.unwrap_or_else(|| meta.name.0.clone());
				ImplMember::ExternalFunc(visibility, external_name, meta)
			}
		} else if self.check(&Token::Let) {
			self.advance();
			let mutable = self.consume(&Token::Mut).is_some();
			let name = self.parse_pattern()?;
			let type_ = if self.consume(&Token::Colon).is_some() {
				Some(self.parse_type()?)
			} else {
				None
			};
			self.expect(&Token::Eq, "=")?;
			let value = self.parse_expression()?;
			ImplMember::Let {
				visibility,
				meta: LetDeclaration {
					mutable,
					name,
					type_,
				},
				value,
			}
		} else if self.check(&Token::Func) {
			let meta = self.parse_func_meta()?;
			self.expect(&Token::Arrow, "->")?;
			let body = self.parse_expression()?;
			ImplMember::Func {
				visibility,
				meta,
				body,
			}
		} else {
			return None;
		};

		let end_span = self.previous_span();
		Some(Spanned(member, span(start_span.start, end_span.end)))
	}
}

fn shorthand_external_name(pattern: &Pattern) -> Option<EcoString> {
	match pattern {
		Pattern::Binding { name, .. } => Some(name.0.clone()),
		Pattern::Struct { path, fields } if path.len() == 1 && fields.is_empty() => {
			Some(path[0].0.clone())
		}
		_ => None,
	}
}
