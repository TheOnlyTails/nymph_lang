//! The lexer: turns Nymph source text into a flat stream of [`Spanned<Token>`].
//!
//! Design notes:
//! - The stream is **flat**. Delimiters are single tokens; the `#`-collection sigils
//!   (`#[`, `#(`, `#{`) are single combined tokens.
//! - Numeric values are decoded here (radix, digit separators, suffixes) so later
//!   stages never re-parse text.
//! - Strings keep a localized nested structure ([`StrFragment`]) because interpolation
//!   genuinely embeds expressions.
//! - Bare `<<` / `>>` are intentionally *not* lexed; the parser recombines two adjacent
//!   `<` / `>` tokens into a shift, which keeps generic close-brackets (`Foo<Bar<T>>`)
//!   unambiguous.
//! - `!in` / `!is` are produced by merging an adjacent `!` with the `in` / `is` keyword,
//!   which correctly leaves `!inside` as `!` followed by the identifier `inside`.

use crate::errors::LexError;
use nymph_ast::{
	Span, Spanned,
	expr::StringEscape,
	token::{StrFragment, Token},
};
use nymph_diagnostics::{Diagnostic, IntoDiagnostic};

/// The result of lexing: the token stream plus any diagnostics gathered along the way.
pub struct LexResult {
	pub tokens: Vec<Spanned<Token>>,
	pub diagnostics: Vec<Diagnostic>,
	/// Whether lexing stopped because the source ended while a token was open.
	/// REPL clients use this typed signal instead of guessing from rendered
	/// diagnostic text.
	pub incomplete: bool,
}

fn clean(s: &str) -> String {
	s.chars().filter(|c| *c != '_').collect()
}

fn parse_f64(s: &str) -> f64 {
	if s.contains('_') {
		clean(s).parse()
	} else {
		s.parse()
	}
	.unwrap_or(f64::NAN)
}

/// Lex a whole source file.
pub fn lex(source: &str) -> LexResult {
	Lexer::new(source).lex()
}

struct Lexer<'src> {
	source: &'src str,
	position: usize,
	diagnostics: Vec<Diagnostic>,
	incomplete: bool,
}

impl<'src> Lexer<'src> {
	fn new(source: &'src str) -> Self {
		Self {
			source,
			position: 0,
			diagnostics: Vec::new(),
			incomplete: false,
		}
	}

	fn lex(mut self) -> LexResult {
		let mut tokens = self.tokens(false).unwrap_or_default();
		normalize_tokens(&mut tokens);
		LexResult {
			tokens,
			diagnostics: self.diagnostics,
			incomplete: self.incomplete,
		}
	}

	fn tokens(&mut self, interpolation: bool) -> Option<Vec<Spanned<Token>>> {
		let mut tokens = Vec::new();
		let mut brace_depth = 0;
		loop {
			self.skip_trivia();
			if self.position == self.source.len() {
				return if interpolation {
					self.fail(&["'}'"])
				} else {
					Some(tokens)
				};
			}
			if interpolation && self.starts_with("}") && brace_depth == 0 {
				self.position += 1;
				return Some(tokens);
			}

			let token = self.token()?;
			match &token.0 {
				Token::LBrace | Token::HashLBrace if interpolation => brace_depth += 1,
				Token::RBrace if interpolation => brace_depth -= 1,
				_ => {}
			}
			tokens.push(token);
		}
	}

	fn skip_trivia(&mut self) {
		loop {
			while self.peek().is_some_and(char::is_whitespace) {
				self.bump();
			}
			if self.starts_with("//") {
				self.position += 2;
				while self.peek().is_some_and(|c| c != '\n') {
					self.bump();
				}
			} else if self.starts_with("/*") {
				if let Some(end) = self.source[self.position + 2..].find("*/") {
					self.position += end + 4;
				} else {
					self.incomplete = true;
					return;
				}
			} else {
				return;
			}
		}
	}

	fn token(&mut self) -> Option<Spanned<Token>> {
		let start = self.position;
		let first = self.peek()?;
		let token = if first.is_ascii_digit() {
			self.number()
		} else if first == '\'' {
			self.character()?
		} else if first == '"' {
			self.string()?
		} else if first == '$' {
			self.anonymous_param()
		} else if first == '_' || unicode_ident::is_xid_start(first) {
			self.identifier()
		} else {
			self.operator()?
		};
		Some(Spanned(token, Span::new(start, self.position)))
	}

	fn number(&mut self) -> Token {
		if self.starts_with("0x") || self.starts_with("0X") {
			return self.radix_number(16, |c| c.is_ascii_hexdigit());
		}
		if self.starts_with("0o") || self.starts_with("0O") {
			return self.radix_number(8, |c| matches!(c, '0'..='7'));
		}
		if self.starts_with("0b") || self.starts_with("0B") {
			return self.radix_number(2, |c| matches!(c, '0' | '1'));
		}

		let start = self.position;
		self.digit_sequence(|c| c.is_ascii_digit());
		if self.starts_with(".") && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
			self.position += 1;
			self.digit_sequence(|c| c.is_ascii_digit());
			self.optional_exponent();
			return Token::Float(parse_f64(&self.source[start..self.position]).into());
		}
		if self.has_exponent() {
			self.optional_exponent();
			return Token::Float(parse_f64(&self.source[start..self.position]).into());
		}
		if self.peek().is_some_and(|c| matches!(c, 'f' | 'F')) {
			self.bump();
			return Token::Float(parse_f64(&self.source[start..self.position - 1]).into());
		}
		if self.peek().is_some_and(|c| matches!(c, 'u' | 'U')) {
			self.bump();
		}
		int_decimal(&self.source[start..self.position]).unwrap_or_else(|| {
			self.emit(
				LexError::IntegerLiteralOutOfRange,
				Span::new(start, self.position),
			);
			Token::Int(u64::MAX)
		})
	}

	fn radix_number(&mut self, radix: u32, is_digit: impl Fn(char) -> bool + Copy) -> Token {
		let start = self.position;
		if !self.peek_at(2).is_some_and(is_digit) {
			self.position += 1;
			return Token::Int(0);
		}
		self.position += 2;
		self.digit_sequence(is_digit);
		if self.peek().is_some_and(|c| matches!(c, 'u' | 'U')) {
			self.bump();
		}
		int_radix(&self.source[start..self.position], radix).unwrap_or_else(|| {
			self.emit(
				LexError::IntegerLiteralOutOfRange,
				Span::new(start, self.position),
			);
			Token::Int(u64::MAX)
		})
	}

	fn digit_sequence(&mut self, is_digit: impl Fn(char) -> bool + Copy) {
		while self.peek().is_some_and(is_digit) {
			self.bump();
			if self.starts_with("_") && self.peek_at(1).is_some_and(is_digit) {
				self.position += 1;
			}
		}
	}

	fn has_exponent(&self) -> bool {
		if !self.peek().is_some_and(|c| matches!(c, 'e' | 'E')) {
			return false;
		}
		let sign = usize::from(self.peek_at(1).is_some_and(|c| matches!(c, '+' | '-')));
		self.peek_at(sign + 1).is_some_and(|c| c.is_ascii_digit())
	}

	fn optional_exponent(&mut self) {
		if self.has_exponent() {
			self.bump();
			if self.peek().is_some_and(|c| matches!(c, '+' | '-')) {
				self.bump();
			}
			self.digit_sequence(|c| c.is_ascii_digit());
		}
	}

	fn character(&mut self) -> Option<Token> {
		self.position += 1;
		let value = if self.starts_with("\\") {
			let escape_start = self.position;
			self.position += 1;
			let escape = match self.bump() {
				Some(escape) => escape,
				None => return self.fail(&["a character escape"]),
			};
			match escape {
				'n' => '\n',
				'r' => '\r',
				't' => '\t',
				'\\' => '\\',
				'\'' => '\'',
				'u' | 'U' => self.unicode_escape(escape_start)?,
				_ => return self.fail_at(self.position - 1, &["a valid character escape"]),
			}
		} else {
			let value = match self.bump() {
				Some(value) => value,
				None => return self.fail(&["a character"]),
			};
			if matches!(value, '\\' | '\'') {
				return self.fail_at(self.position - value.len_utf8(), &["a character"]);
			}
			value
		};
		if !self.starts_with("'") {
			return self.fail(&["'''"]);
		}
		self.position += 1;
		Some(Token::Char(value))
	}

	fn string(&mut self) -> Option<Token> {
		self.position += 1;
		let mut fragments = Vec::new();
		loop {
			let start = self.position;
			let Some(next) = self.peek() else {
				return self.fail(&["text", "an escape", "an interpolation", "'\"'"]);
			};
			match next {
				'"' => {
					self.position += 1;
					return Some(Token::Str(fragments));
				}
				'\\' => {
					self.position += 1;
					let escaped = match self.bump() {
						Some(escaped) => escaped,
						None => return self.fail(&["a string escape"]),
					};
					let escape = match escaped {
						'n' => StringEscape::Newline,
						'r' => StringEscape::Carriage,
						't' => StringEscape::Tab,
						'\\' => StringEscape::Backslash,
						'"' => StringEscape::Quote,
						'u' | 'U' => StringEscape::Unicode(self.unicode_escape(start)?),
						'$' if self.starts_with("{") => {
							self.position += 1;
							StringEscape::Interpolation
						}
						_ => {
							return self.fail_at(
								self.position - 1,
								&["'n'", "'r'", "'t'", "'\\\\'", "'\"'", "'$'"],
							);
						}
					};
					fragments.push(Spanned(
						StrFragment::Escape(escape),
						Span::new(start, self.position),
					));
				}
				'$' if self.peek_at(1) == Some('{') => {
					self.position += 2;
					let tokens = self.tokens(true)?;
					fragments.push(Spanned(
						StrFragment::Interpolation(tokens),
						Span::new(start, self.position),
					));
				}
				_ => {
					while self
						.peek()
						.is_some_and(|c| c != '"' && c != '\\' && !(c == '$' && self.peek_at(1) == Some('{')))
					{
						self.bump();
					}
					fragments.push(Spanned(
						StrFragment::Text(self.source[start..self.position].into()),
						Span::new(start, self.position),
					));
				}
			}
		}
	}

	fn unicode_escape(&mut self, escape_start: usize) -> Option<char> {
		let start = self.position;
		while self.position - start < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
			self.bump();
		}
		if self.position == start {
			return self.fail(&["a hexadecimal digit"]);
		}
		let value = u32::from_str_radix(&self.source[start..self.position], 16)
			.ok()
			.and_then(char::from_u32);
		Some(value.unwrap_or_else(|| {
			self.emit(
				LexError::InvalidUnicodeCodePoint,
				Span::new(escape_start, self.position),
			);
			'\u{FFFD}'
		}))
	}

	fn anonymous_param(&mut self) -> Token {
		let token_start = self.position;
		self.position += 1;
		let start = self.position;
		while self.peek().is_some_and(|c| c.is_ascii_digit()) {
			self.bump();
		}
		let index = if start == self.position {
			None
		} else {
			Some(
				self.source[start..self.position]
					.parse::<u8>()
					.unwrap_or_else(|_| {
						self.emit(
							LexError::ClosureIndexTooLarge,
							Span::new(token_start, self.position),
						);
						0
					}),
			)
		};
		Token::AnonymousParam(index)
	}

	fn identifier(&mut self) -> Token {
		let start = self.position;
		self.bump();
		while self.peek().is_some_and(unicode_ident::is_xid_continue) {
			self.bump();
		}
		match &self.source[start..self.position] {
			"true" => Token::True,
			"false" => Token::False,
			"public" => Token::Public,
			"internal" => Token::Internal,
			"private" => Token::Private,
			"import" => Token::Import,
			"with" => Token::With,
			"async" => Token::Async,
			"await" => Token::Await,
			"type" => Token::Type,
			"struct" => Token::Struct,
			"enum" => Token::Enum,
			"let" => Token::Let,
			"external" => Token::External,
			"effect" => Token::Effect,
			"func" => Token::Func,
			"interface" => Token::Interface,
			"impl" => Token::Impl,
			"namespace" => Token::Namespace,
			"for" => Token::For,
			"loop" => Token::Loop,
			"if" => Token::If,
			"else" => Token::Else,
			"match" => Token::Match,
			"int" => Token::IntType,
			"uint" => Token::UIntType,
			"float" => Token::FloatType,
			"boolean" => Token::BooleanType,
			"char" => Token::CharType,
			"string" => Token::StringType,
			"void" => Token::VoidType,
			"never" => Token::NeverType,
			"self" => Token::SelfType,
			"as" => Token::As,
			"is" => Token::Is,
			"in" => Token::In,
			"return" => Token::Return,
			"break" => Token::Break,
			"continue" => Token::Continue,
			"echo" => Token::Echo,
			"this" => Token::This,
			"_" => Token::Underscore,
			other => Token::Identifier((*other).into()),
		}
	}

	fn operator(&mut self) -> Option<Token> {
		let pairs = [
			("..=", Token::DotDotEq),
			("...", Token::DotDotDot),
			("..", Token::DotDot),
			("**", Token::StarStar),
			("<=", Token::LtEq),
			(">=", Token::GtEq),
			("?.", Token::QuestionDot),
			("??", Token::DoubleQuestion),
			("->", Token::Arrow),
			("|>", Token::PipeArrow),
			("||", Token::PipePipe),
			("&&", Token::AmpAmp),
			("==", Token::EqEq),
			("!=", Token::BangEq),
			("::", Token::ColonColon),
			("#(", Token::HashLParen),
			("#[", Token::HashLBracket),
			("#{", Token::HashLBrace),
		];
		for (text, token) in pairs {
			if self.starts_with(text) {
				self.position += text.len();
				return Some(token);
			}
		}
		if self.starts_with("#") {
			self.position += 1;
			return self.fail(&["'('", "'['", "'{'"]);
		}
		let start = self.position;
		let token = match self.bump()? {
			'.' => Token::Dot,
			'*' => Token::Star,
			'<' => Token::Lt,
			'>' => Token::Gt,
			'?' => Token::Question,
			'-' => Token::Minus,
			'|' => Token::Pipe,
			'&' => Token::Amp,
			'^' => Token::Caret,
			'~' => Token::Tilde,
			'=' => Token::Eq,
			'!' => Token::Bang,
			'+' => Token::Plus,
			'/' => Token::Slash,
			'%' => Token::Percent,
			':' => Token::Colon,
			'(' => Token::LParen,
			')' => Token::RParen,
			'[' => Token::LBracket,
			']' => Token::RBracket,
			'{' => Token::LBrace,
			'}' => Token::RBrace,
			',' => Token::Comma,
			';' => Token::Semicolon,
			'@' => Token::At,
			_ => return self.fail_at(start, &["a token"]),
		};
		Some(token)
	}

	fn emit(&mut self, error: LexError, span: Span) {
		self.diagnostics.push(error.as_diagnostic(span));
	}

	fn fail<T>(&mut self, expected: &[&str]) -> Option<T> {
		self.fail_at(self.position, expected)
	}

	fn fail_at<T>(&mut self, position: usize, expected: &[&str]) -> Option<T> {
		let found = self.source[position..].chars().next();
		let end = found.map_or(position, |c| position + c.len_utf8());
		self.incomplete |= found.is_none();
		self.emit(
			LexError::ExpectedFound {
				found,
				expected: expected.iter().map(|value| (*value).into()).collect(),
			},
			Span::new(position, end),
		);
		None
	}

	fn starts_with(&self, text: &str) -> bool {
		self.source[self.position..].starts_with(text)
	}

	fn peek(&self) -> Option<char> {
		self.source[self.position..].chars().next()
	}

	fn peek_at(&self, offset: usize) -> Option<char> {
		self.source[self.position..].chars().nth(offset)
	}

	fn bump(&mut self) -> Option<char> {
		let value = self.peek()?;
		self.position += value.len_utf8();
		Some(value)
	}
}

/// Apply post-lex normalization to this token stream and every interpolation nested in it.
fn normalize_tokens(tokens: &mut Vec<Spanned<Token>>) {
	for token in tokens.iter_mut() {
		if let Token::Str(fragments) = &mut token.0 {
			for fragment in fragments {
				if let StrFragment::Interpolation(tokens) = &mut fragment.0 {
					normalize_tokens(tokens);
				}
			}
		}
	}
	merge_bang_keywords(tokens);
}

/// Merge an adjacent `!` and `in`/`is` keyword into a single `!in` / `!is` token.
fn merge_bang_keywords(tokens: &mut Vec<Spanned<Token>>) {
	let mut merged = Vec::with_capacity(tokens.len());
	let mut i = 0;
	while i < tokens.len() {
		let cur = &tokens[i];
		if cur.0 == Token::Bang
			&& let Some(next) = tokens.get(i + 1)
			&& cur.1.end == next.1.start
		{
			match next.0 {
				Token::In => {
					merged.push(Spanned(Token::BangIn, Span::new(cur.1.start, next.1.end)));
					i += 2;
					continue;
				}
				Token::Is => {
					merged.push(Spanned(Token::BangIs, Span::new(cur.1.start, next.1.end)));
					i += 2;
					continue;
				}
				_ => {}
			}
		}
		merged.push(cur.clone());
		i += 1;
	}
	*tokens = merged;
}

fn int_radix(s: &str, radix: u32) -> Option<Token> {
	let unsigned = s.ends_with(['u', 'U']);
	let body = if unsigned {
		&s[2..s.len() - 1]
	} else {
		&s[2..]
	};
	let value = u64::from_str_radix(&clean(body), radix).ok()?;
	if unsigned {
		Some(Token::UInt(value))
	} else {
		Some(Token::Int(value))
	}
}

fn int_decimal(s: &str) -> Option<Token> {
	let unsigned = s.ends_with(['u', 'U']);
	let body = if unsigned { &s[..s.len() - 1] } else { s };
	let value = clean(body).parse::<u64>().ok()?;
	if unsigned {
		Some(Token::UInt(value))
	} else {
		Some(Token::Int(value))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use nymph_ast::token::StrFragment;

	/// Lex, asserting no diagnostics, and return just the token values.
	fn toks(src: &str) -> Vec<Token> {
		let result = lex(src);
		assert!(
			result.diagnostics.is_empty(),
			"unexpected diagnostics: {:?}",
			result.diagnostics
		);
		result.tokens.into_iter().map(|t| t.0).collect()
	}

	#[test]
	fn integers() {
		assert_eq!(toks("1234"), vec![Token::Int(1234)]);
		assert_eq!(toks("1_000_000"), vec![Token::Int(1_000_000)]);
		assert_eq!(toks("0xDEAD"), vec![Token::Int(0xDEAD)]);
		assert_eq!(toks("0o755"), vec![Token::Int(0o755)]);
		assert_eq!(toks("0b1010"), vec![Token::Int(0b1010)]);
		assert_eq!(toks("1234u"), vec![Token::UInt(1234)]);
		assert_eq!(toks("0xFFu"), vec![Token::UInt(255)]);
	}

	#[test]
	fn floats() {
		assert_eq!(toks("1.0"), vec![Token::Float(1.0.into())]);
		assert_eq!(toks("9e-1"), vec![Token::Float(0.9.into())]);
		assert_eq!(toks("0.24e10"), vec![Token::Float(0.24e10.into())]);
		assert_eq!(toks("2f"), vec![Token::Float(2.0.into())]);
	}

	#[test]
	fn range_is_not_float() {
		// `1..10` must lex as int, range, int — not as a float.
		assert_eq!(
			toks("1..10"),
			vec![Token::Int(1), Token::DotDot, Token::Int(10)]
		);
		assert_eq!(
			toks("1..=10"),
			vec![Token::Int(1), Token::DotDotEq, Token::Int(10)]
		);
	}

	#[test]
	fn collection_sigils() {
		assert_eq!(
			toks("#[1]"),
			vec![Token::HashLBracket, Token::Int(1), Token::RBracket]
		);
		assert_eq!(toks("#()"), vec![Token::HashLParen, Token::RParen]);
		assert_eq!(toks("#{}"), vec![Token::HashLBrace, Token::RBrace]);
	}

	#[test]
	fn closure_params_and_underscore() {
		assert_eq!(toks("$"), vec![Token::AnonymousParam(None)]);
		assert_eq!(toks("$0"), vec![Token::AnonymousParam(Some(0))]);
		assert_eq!(toks("$12"), vec![Token::AnonymousParam(Some(12))]);
		assert_eq!(toks("_"), vec![Token::Underscore]);
	}

	#[test]
	fn keywords_vs_identifiers() {
		assert_eq!(toks("func"), vec![Token::Func]);
		// `internal` must not be lexed as `in` + `ternal`.
		assert_eq!(toks("internal"), vec![Token::Internal]);
		assert_eq!(toks("inside"), vec![Token::Identifier("inside".into())]);
		assert_eq!(toks("match"), vec![Token::Match]);
	}

	#[test]
	fn arrow_and_assign() {
		// `->` for types/closures/arms; `=` for bindings.
		assert_eq!(
			toks("(int) -> int"),
			vec![
				Token::LParen,
				Token::IntType,
				Token::RParen,
				Token::Arrow,
				Token::IntType
			]
		);
		assert_eq!(
			toks("x = 1"),
			vec![Token::Identifier("x".into()), Token::Eq, Token::Int(1)]
		);
	}

	#[test]
	fn bang_in_and_is_merge() {
		assert_eq!(
			toks("x !in y"),
			vec![
				Token::Identifier("x".into()),
				Token::BangIn,
				Token::Identifier("y".into())
			]
		);
		assert_eq!(
			toks("x !is P"),
			vec![
				Token::Identifier("x".into()),
				Token::BangIs,
				Token::Identifier("P".into())
			]
		);
		// `!inside` stays `!` + identifier, never `!in` + `side`.
		assert_eq!(
			toks("!inside"),
			vec![Token::Bang, Token::Identifier("inside".into())]
		);
	}

	#[test]
	fn shift_operators_have_no_compound_assignment_token() {
		// The parser recombines bare `<<`; the retired spelling remains separate operators.
		assert_eq!(
			toks("a << b"),
			vec![
				Token::Identifier("a".into()),
				Token::Lt,
				Token::Lt,
				Token::Identifier("b".into())
			]
		);
		assert_eq!(
			toks("a <<= b"),
			vec![
				Token::Identifier("a".into()),
				Token::Lt,
				Token::LtEq,
				Token::Identifier("b".into())
			]
		);
	}

	#[test]
	fn string_with_interpolation() {
		let t = toks(r#""Hello, ${name}!""#);
		assert_eq!(t.len(), 1);
		let Token::Str(frags) = &t[0] else {
			panic!("expected string, got {:?}", t[0]);
		};
		assert_eq!(frags.len(), 3);
		assert_eq!(frags[0].0, StrFragment::Text("Hello, ".into()));
		match &frags[1].0 {
			StrFragment::Interpolation(inner) => {
				assert_eq!(inner.len(), 1);
				assert_eq!(inner[0].0, Token::Identifier("name".into()));
			}
			other => panic!("expected interpolation, got {other:?}"),
		}
		assert_eq!(frags[2].0, StrFragment::Text("!".into()));
	}

	#[test]
	fn interpolation_balances_braces_with_absolute_spans() {
		let result = lex(r#"prefix "${{ #{1: {2}} }}" suffix"#);
		assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
		let Token::Str(fragments) = &result.tokens[1].0 else {
			panic!("expected string");
		};
		let StrFragment::Interpolation(inner) = &fragments[0].0 else {
			panic!("expected interpolation");
		};
		assert_eq!(fragments[0].1, Span::new(8, 24));
		assert_eq!(
			inner
				.iter()
				.map(|token| (&token.0, token.1))
				.collect::<Vec<_>>(),
			vec![
				(&Token::LBrace, Span::new(10, 11)),
				(&Token::HashLBrace, Span::new(12, 14)),
				(&Token::Int(1), Span::new(14, 15)),
				(&Token::Colon, Span::new(15, 16)),
				(&Token::LBrace, Span::new(17, 18)),
				(&Token::Int(2), Span::new(18, 19)),
				(&Token::RBrace, Span::new(19, 20)),
				(&Token::RBrace, Span::new(20, 21)),
				(&Token::RBrace, Span::new(22, 23)),
			]
		);
	}

	#[test]
	fn interpolation_ignores_braces_in_literals_comments_and_nested_interpolation() {
		for source in [
			r#""${f("}", '{', "${{ 1 }}")}""#,
			r#""${{ /* } { */ 1 // }
			}}""#,
		] {
			let result = lex(source);
			assert!(
				result.diagnostics.is_empty(),
				"diagnostics for {source:?}: {:?}",
				result.diagnostics
			);
		}
	}

	#[test]
	fn escaped_interpolation_stays_literal_without_corrupting_brace_depth() {
		let source = r#""${f("\${...}", { 1 })}""#;
		let result = lex(source);
		assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
		let Token::Str(outer) = &result.tokens[0].0 else {
			panic!("expected outer string");
		};
		let StrFragment::Interpolation(tokens) = &outer[0].0 else {
			panic!("expected outer interpolation");
		};
		let Token::Str(inner) = &tokens[2].0 else {
			panic!("expected nested string");
		};
		assert_eq!(inner.len(), 2);
		assert_eq!(inner[0].0, StrFragment::Escape(StringEscape::Interpolation));
		assert_eq!(inner[1].0, StrFragment::Text("...}".into()));
		assert!(
			inner
				.iter()
				.all(|fragment| !matches!(fragment.0, StrFragment::Interpolation(_)))
		);
		assert_eq!(tokens.last().map(|token| &token.0), Some(&Token::RParen));
	}

	#[test]
	fn unterminated_balanced_interpolation_reports_a_diagnostic() {
		for source in [r#""${{ 1 }""#, r#""${"nested ${1}""#] {
			let result = lex(source);
			assert!(
				!result.diagnostics.is_empty(),
				"unterminated source produced no diagnostic: {source:?}"
			);
			assert!(
				result
					.diagnostics
					.iter()
					.all(|diagnostic| diagnostic.span.end <= source.len()),
				"diagnostic spans escaped source: {:?}",
				result.diagnostics
			);
		}
	}

	#[test]
	fn comments_are_skipped() {
		assert_eq!(
			toks("1 // a line comment\n2"),
			vec![Token::Int(1), Token::Int(2)]
		);
		assert_eq!(toks("1 /* block */ 2"), vec![Token::Int(1), Token::Int(2)]);
	}

	#[test]
	fn char_escapes() {
		assert_eq!(toks(r"'\n'"), vec![Token::Char('\n')]);
		assert_eq!(toks("'a'"), vec![Token::Char('a')]);
		assert_eq!(toks(r"'A'"), vec![Token::Char('A')]);
	}

	#[test]
	fn lexer_handles_repository_sources_and_edge_cases() {
		for source in [
			include_str!("../../../stdlib/src/collections/list.nym"),
			include_str!("../../../stdlib/src/math/complex.nym"),
			include_str!("../../../examples/shapes/src/main.nym"),
			"0x 0o8 0b2 1_ 1.0e 1e+ 2f 3u _ λ λ2",
			"#[1, 2] #{1: 2} #() a...b a..=b a?.b a??b a::b",
			r#"'a' '\n' '\u1f600' "text $ text \n \u1f600 \${x} ${f("}", {1})}""#,
			"// comment\n/* block */ func f($255): int = 1_000",
		] {
			let expected = lex(source);
			assert!(
				expected.diagnostics.is_empty(),
				"{:?}",
				expected.diagnostics
			);
			assert!(!expected.tokens.is_empty(), "no tokens for {source:?}");
		}
	}

	#[test]
	fn malformed_source_preserves_recovery_and_incomplete_signals() {
		let unknown = lex("@bad-token `");
		assert!(unknown.tokens.is_empty());
		assert_eq!(unknown.diagnostics.len(), 1);
		assert_eq!(unknown.diagnostics[0].span, Span::new(11, 12));
		assert!(!unknown.incomplete);

		let comment = lex("/* unterminated");
		assert_eq!(
			comment
				.tokens
				.into_iter()
				.map(|token| token.0)
				.collect::<Vec<_>>(),
			vec![
				Token::Slash,
				Token::Star,
				Token::Identifier("unterminated".into())
			]
		);
		assert!(comment.diagnostics.is_empty());
		assert!(comment.incomplete);

		for source in ["'unterminated", r#""bad \q escape""#] {
			let result = lex(source);
			assert!(result.tokens.is_empty());
			assert_eq!(result.diagnostics.len(), 1);
			assert!(!result.incomplete);
		}

		for source in [r#""unterminated"#, r#""${{ 1 }""#, "#"] {
			let result = lex(source);
			assert!(result.tokens.is_empty());
			assert_eq!(result.diagnostics.len(), 1);
			assert!(result.incomplete);
		}
	}

	#[test]
	fn malformed_values_keep_recovery_tokens_and_diagnostics() {
		let closure = lex("$256");
		assert_eq!(closure.tokens[0].0, Token::AnonymousParam(Some(0)));
		assert_eq!(closure.diagnostics[0].code, "2");

		let integer = lex("18446744073709551616");
		assert_eq!(integer.tokens[0].0, Token::Int(u64::MAX));
		assert_eq!(integer.diagnostics[0].code, "3");

		let character = lex(r"'\u110000'");
		assert_eq!(character.tokens[0].0, Token::Char('\u{FFFD}'));
		assert_eq!(character.diagnostics[0].code, "1");
		assert_eq!(character.diagnostics[0].span, Span::new(1, 9));
	}
}
