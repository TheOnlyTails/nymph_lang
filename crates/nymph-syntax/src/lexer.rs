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
use chumsky::{error::RichReason, prelude::*};
use nymph_ast::{
	Span, Spanned,
	expr::StringEscape,
	token::{StrFragment, Token},
};
use nymph_diagnostics::{Diagnostic, IntoDiagnostic};

type Err<'src> = extra::Err<Rich<'src, char, SimpleSpan, LexError>>;

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
	if let Some(mut tokens) = FastLexer::new(source).lex() {
		normalize_tokens(&mut tokens);
		return LexResult {
			tokens,
			diagnostics: Vec::new(),
			incomplete: false,
		};
	}

	lex_with_chumsky(source)
}

/// Preserve Chumsky's recovery and detailed diagnostics for malformed source.
fn lex_with_chumsky(source: &str) -> LexResult {
	let (output, errors) = lexer().parse(source).into_output_errors();
	let mut tokens = output.unwrap_or_default();
	normalize_tokens(&mut tokens);
	let incomplete = has_unterminated_block_comment(source)
		|| errors.iter().any(|error| {
			matches!(
				error.reason(),
				RichReason::ExpectedFound { found: None, .. }
			)
		});

	let diagnostics = errors
		.into_iter()
		.map(|err| {
			let span = *err.span();
			let err = match err.reason() {
				RichReason::ExpectedFound { expected, found } => &LexError::ExpectedFound {
					found: found.map(|it| it.into_inner()),
					expected: expected.iter().map(|it| it.to_string().into()).collect(),
				},
				RichReason::Custom(err) => err,
			};
			err.as_diagnostic(span)
		})
		.collect();

	LexResult {
		tokens,
		diagnostics,
		incomplete,
	}
}

/// Allocation-light lexer for well-formed source. Any malformed or ambiguous
/// token returns `None`, which sends the source through the diagnostic lexer.
struct FastLexer<'src> {
	source: &'src str,
	position: usize,
}

impl<'src> FastLexer<'src> {
	fn new(source: &'src str) -> Self {
		Self {
			source,
			position: 0,
		}
	}

	fn lex(mut self) -> Option<Vec<Spanned<Token>>> {
		self.tokens(false)
	}

	fn tokens(&mut self, interpolation: bool) -> Option<Vec<Spanned<Token>>> {
		let mut tokens = Vec::new();
		let mut brace_depth = 0;
		loop {
			self.skip_trivia()?;
			if self.position == self.source.len() {
				return (!interpolation).then_some(tokens);
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

	fn skip_trivia(&mut self) -> Option<()> {
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
				let end = self.source[self.position + 2..].find("*/")?;
				self.position += end + 4;
			} else {
				return Some(());
			}
		}
	}

	fn token(&mut self) -> Option<Spanned<Token>> {
		let start = self.position;
		let first = self.peek()?;
		let token = if first.is_ascii_digit() {
			self.number()?
		} else if first == '\'' {
			self.character()?
		} else if first == '"' {
			self.string()?
		} else if first == '$' {
			self.anonymous_param()?
		} else if first == '_' || unicode_ident::is_xid_start(first) {
			self.identifier()
		} else {
			self.operator()?
		};
		Some(Spanned(token, Span::new(start, self.position)))
	}

	fn number(&mut self) -> Option<Token> {
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
			return Some(Token::Float(
				parse_f64(&self.source[start..self.position]).into(),
			));
		}
		if self.has_exponent() {
			self.optional_exponent();
			return Some(Token::Float(
				parse_f64(&self.source[start..self.position]).into(),
			));
		}
		if self.peek().is_some_and(|c| matches!(c, 'f' | 'F')) {
			self.bump();
			return Some(Token::Float(
				parse_f64(&self.source[start..self.position - 1]).into(),
			));
		}
		if self.peek().is_some_and(|c| matches!(c, 'u' | 'U')) {
			self.bump();
		}
		int_decimal(&self.source[start..self.position])
	}

	fn radix_number(&mut self, radix: u32, is_digit: impl Fn(char) -> bool + Copy) -> Option<Token> {
		let start = self.position;
		if !self.peek_at(2).is_some_and(is_digit) {
			self.position += 1;
			return int_decimal(&self.source[start..self.position]);
		}
		self.position += 2;
		self.digit_sequence(is_digit);
		if self.peek().is_some_and(|c| matches!(c, 'u' | 'U')) {
			self.bump();
		}
		int_radix(&self.source[start..self.position], radix)
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
			self.position += 1;
			match self.bump()? {
				'n' => '\n',
				'r' => '\r',
				't' => '\t',
				'\\' => '\\',
				'\'' => '\'',
				'u' | 'U' => self.unicode_escape()?,
				_ => return None,
			}
		} else {
			let value = self.bump()?;
			if matches!(value, '\\' | '\'') {
				return None;
			}
			value
		};
		if !self.starts_with("'") {
			return None;
		}
		self.position += 1;
		Some(Token::Char(value))
	}

	fn string(&mut self) -> Option<Token> {
		self.position += 1;
		let mut fragments = Vec::new();
		loop {
			let start = self.position;
			match self.peek()? {
				'"' => {
					self.position += 1;
					return Some(Token::Str(fragments));
				}
				'\\' => {
					self.position += 1;
					let escape = match self.bump()? {
						'n' => StringEscape::Newline,
						'r' => StringEscape::Carriage,
						't' => StringEscape::Tab,
						'\\' => StringEscape::Backslash,
						'"' => StringEscape::Quote,
						'u' | 'U' => StringEscape::Unicode(self.unicode_escape()?),
						'$' if self.starts_with("{") => {
							self.position += 1;
							StringEscape::Interpolation
						}
						_ => return None,
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

	fn unicode_escape(&mut self) -> Option<char> {
		let start = self.position;
		while self.position - start < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
			self.bump();
		}
		if self.position == start {
			return None;
		}
		u32::from_str_radix(&self.source[start..self.position], 16)
			.ok()
			.and_then(char::from_u32)
	}

	fn anonymous_param(&mut self) -> Option<Token> {
		self.position += 1;
		let start = self.position;
		while self.peek().is_some_and(|c| c.is_ascii_digit()) {
			self.bump();
		}
		let index = (start != self.position)
			.then(|| self.source[start..self.position].parse::<u8>())
			.transpose()
			.ok()?;
		Some(Token::AnonymousParam(index))
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
			_ => return None,
		};
		Some(token)
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

/// The token parser can recover a failed `/* ... */` as `/` and `*` operator
/// tokens. Keep that lexical recovery for diagnostics, but retain the fact
/// that the comment token itself is still open for incremental/REPL clients.
fn has_unterminated_block_comment(source: &str) -> bool {
	let bytes = source.as_bytes();
	let mut index = 0;
	let mut quote = None;
	while index < bytes.len() {
		if let Some(end) = quote {
			if bytes[index] == b'\\' {
				index += 2;
				continue;
			}
			if bytes[index] == end {
				quote = None;
			}
			index += 1;
			continue;
		}
		if matches!(bytes[index], b'\'' | b'"') {
			quote = Some(bytes[index]);
			index += 1;
			continue;
		}
		if bytes[index..].starts_with(b"//") {
			index += bytes[index..]
				.iter()
				.position(|byte| *byte == b'\n')
				.unwrap_or(bytes.len() - index);
			continue;
		}
		if bytes[index..].starts_with(b"/*") {
			let Some(close) = bytes[index + 2..]
				.windows(2)
				.position(|window| window == b"*/")
			else {
				return true;
			};
			index += close + 4;
			continue;
		}
		index += 1;
	}
	false
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

fn lexer<'src>() -> impl Parser<'src, &'src str, Vec<Spanned<Token>>, Err<'src>> + Clone {
	recursive(|token| {
		choice((
			number(),
			char_literal(),
			string_literal(token.clone()),
			anonymous_param(),
			keyword_or_ident(),
			operators(),
		))
		.padded_by(comment().repeated())
		.padded()
	})
	.repeated()
	.collect()
	.padded_by(comment().repeated())
	.padded()
}

fn comment<'src>() -> impl Parser<'src, &'src str, (), Err<'src>> + Clone {
	let line = just("//")
		.then(any().and_is(just('\n').not()).repeated())
		.ignored();
	let block = just("/*")
		.then(any().and_is(just("*/").not()).repeated())
		.then(just("*/"))
		.ignored();
	choice((line, block)).padded()
}

fn number<'src>() -> impl Parser<'src, &'src str, Spanned<Token>, Err<'src>> + Clone {
	let hex = regex(r"0[xX][0-9a-fA-F](_?[0-9a-fA-F])*[uU]?").map(|s: &str| int_radix(s, 16));
	let oct = regex(r"0[oO][0-7](_?[0-7])*[uU]?").map(|s: &str| int_radix(s, 8));
	let bin = regex(r"0[bB][01](_?[01])*[uU]?").map(|s: &str| int_radix(s, 2));
	let dec = regex(r"\d(_?\d)*[uU]?").map(int_decimal);
	let integer = choice((hex, oct, bin, dec)).validate(|token, e, emitter| {
		token.unwrap_or_else(|| {
			emitter.emit(Rich::custom(e.span(), LexError::IntegerLiteralOutOfRange));
			Token::Int(u64::MAX)
		})
	});

	let float_dot = regex(r"\d(_?\d)*\.\d(_?\d)*([eE][+-]?\d(_?\d)*)?")
		.map(|s: &str| Token::Float(parse_f64(s).into()));
	let float_exp =
		regex(r"\d(_?\d)*[eE][+-]?\d(_?\d)*").map(|s: &str| Token::Float(parse_f64(s).into()));
	let float_suffix =
		regex(r"\d(_?\d)*[fF]").map(|s: &str| Token::Float(parse_f64(&s[..s.len() - 1]).into()));

	// Floats are tried before integers so `1.5` is not read as `1` `.` `5`, and the
	// dotted form is tried first so `1e3` and `1f` still work.
	choice((float_dot, float_exp, float_suffix, integer)).map_with(|v, e| Spanned::new(v, e.span()))
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

fn char_literal<'src>() -> impl Parser<'src, &'src str, Spanned<Token>, Err<'src>> + Clone {
	let unicode = just('\\')
		.ignore_then(one_of("uU"))
		.ignore_then(text::digits(16).at_least(1).at_most(6).to_slice())
		.map(|s: &str| u32::from_str_radix(s, 16).ok().and_then(char::from_u32))
		.validate(|c, e, emitter| {
			c.unwrap_or_else(|| {
				emitter.emit(Rich::custom(e.span(), LexError::InvalidUnicodeCodePoint));
				'\u{FFFD}'
			})
		});

	let escape = choice((
		just(r"\n").to('\n'),
		just(r"\r").to('\r'),
		just(r"\t").to('\t'),
		just(r"\\").to('\\'),
		just(r"\'").to('\''),
	));

	choice((unicode, escape, none_of("\\'")))
		.delimited_by(just('\''), just('\''))
		.map(Token::Char)
		.map_with(|v, e| Spanned::new(v, e.span()))
}

fn string_literal<'src>(
	token: impl Parser<'src, &'src str, Spanned<Token>, Err<'src>> + Clone + 'src,
) -> impl Parser<'src, &'src str, Spanned<Token>, Err<'src>> + Clone {
	let unicode = regex(r"\\[uU][0-9a-fA-F]{1,6}")
		.map(|s: &str| {
			u32::from_str_radix(&s[2..], 16)
				.ok()
				.and_then(char::from_u32)
		})
		.validate(|c, e, emitter| {
			StrFragment::Escape(StringEscape::Unicode(c.unwrap_or_else(|| {
				emitter.emit(Rich::custom(e.span(), LexError::InvalidUnicodeCodePoint));
				'\u{FFFD}'
			})))
		});

	let escape = choice((
		just(r"\n").to(StringEscape::Newline),
		just(r"\r").to(StringEscape::Carriage),
		just(r"\t").to(StringEscape::Tab),
		just(r"\\").to(StringEscape::Backslash),
		just(r#"\""#).to(StringEscape::Quote),
		just(r"\${").to(StringEscape::Interpolation),
	))
	.map(StrFragment::Escape);

	// Reuse the normal token parser inside interpolation. Only brace-delimited token
	// groups are special here: consume each group recursively so its closing brace
	// cannot be mistaken for the interpolation's closing brace. Strings, chars, and
	// comments therefore retain exactly their ordinary lexing behavior, including
	// recursively nested string interpolation.
	let non_brace = token
		.clone()
		.filter(|token| !matches!(token.0, Token::LBrace | Token::HashLBrace | Token::RBrace));
	let balanced_braces = recursive(|balanced| {
		let opening = token
			.clone()
			.filter(|token| matches!(token.0, Token::LBrace | Token::HashLBrace));
		let closing = token.clone().filter(|token| token.0 == Token::RBrace);
		opening
			.then(
				choice((balanced, non_brace.clone().map(|token| vec![token])))
					.repeated()
					.collect::<Vec<Vec<Spanned<Token>>>>(),
			)
			.then(closing)
			.map(|((opening, groups), closing)| {
				let mut tokens = Vec::with_capacity(2 + groups.iter().map(Vec::len).sum::<usize>());
				tokens.push(opening);
				tokens.extend(groups.into_iter().flatten());
				tokens.push(closing);
				tokens
			})
	});
	let interpolation = choice((balanced_braces, non_brace.map(|token| vec![token])))
		.repeated()
		.collect::<Vec<Vec<Spanned<Token>>>>()
		.map(|groups| groups.into_iter().flatten().collect())
		.padded()
		.map(StrFragment::Interpolation)
		.delimited_by(just("${"), just('}'));

	let text = none_of("\"\\$")
		.or(just('$').then(none_of('{').rewind()).to('$'))
		.repeated()
		.at_least(1)
		.to_slice()
		.map(|s: &str| StrFragment::Text(s.into()));

	choice((unicode, escape, interpolation, text))
		.map_with(|v, e| Spanned::new(v, e.span()))
		.repeated()
		.collect::<Vec<_>>()
		.delimited_by(just('"'), just('"'))
		.map(Token::Str)
		.map_with(|v, e| Spanned::new(v, e.span()))
}

fn anonymous_param<'src>() -> impl Parser<'src, &'src str, Spanned<Token>, Err<'src>> + Clone {
	just('$')
		.ignore_then(text::digits(10).to_slice().or_not())
		.validate(|digits: Option<&str>, e, emitter| {
			let index = digits.map(|d| {
				d.parse::<u8>().unwrap_or_else(|_| {
					emitter.emit(Rich::custom(e.span(), LexError::ClosureIndexTooLarge));
					0
				})
			});
			Token::AnonymousParam(index)
		})
		.map_with(|v, e| Spanned::new(v, e.span()))
}

fn keyword_or_ident<'src>() -> impl Parser<'src, &'src str, Spanned<Token>, Err<'src>> + Clone {
	let keyword = choice([
		text::keyword("true").to(Token::True),
		text::keyword("false").to(Token::False),
		text::keyword("public").to(Token::Public),
		text::keyword("internal").to(Token::Internal),
		text::keyword("private").to(Token::Private),
		text::keyword("import").to(Token::Import),
		text::keyword("with").to(Token::With),
		text::keyword("async").to(Token::Async),
		text::keyword("await").to(Token::Await),
		text::keyword("type").to(Token::Type),
		text::keyword("struct").to(Token::Struct),
		text::keyword("enum").to(Token::Enum),
		text::keyword("let").to(Token::Let),
		text::keyword("external").to(Token::External),
		text::keyword("effect").to(Token::Effect),
		text::keyword("func").to(Token::Func),
		text::keyword("interface").to(Token::Interface),
		text::keyword("impl").to(Token::Impl),
		text::keyword("namespace").to(Token::Namespace),
		text::keyword("for").to(Token::For),
		text::keyword("loop").to(Token::Loop),
		text::keyword("if").to(Token::If),
		text::keyword("else").to(Token::Else),
		text::keyword("match").to(Token::Match),
		text::keyword("int").to(Token::IntType),
		text::keyword("uint").to(Token::UIntType),
		text::keyword("float").to(Token::FloatType),
		text::keyword("boolean").to(Token::BooleanType),
		text::keyword("char").to(Token::CharType),
		text::keyword("string").to(Token::StringType),
		text::keyword("void").to(Token::VoidType),
		text::keyword("never").to(Token::NeverType),
		text::keyword("self").to(Token::SelfType),
		text::keyword("as").to(Token::As),
		text::keyword("is").to(Token::Is),
		text::keyword("in").to(Token::In),
		text::keyword("return").to(Token::Return),
		text::keyword("break").to(Token::Break),
		text::keyword("continue").to(Token::Continue),
		text::keyword("echo").to(Token::Echo),
		text::keyword("this").to(Token::This),
	]);

	let ident = text::unicode::ident().map(|t: &str| match t {
		"_" => Token::Underscore,
		other => Token::Identifier(other.into()),
	});

	choice((keyword, ident)).map_with(|v, e| Spanned::new(v, e.span()))
}

fn operators<'src>() -> impl Parser<'src, &'src str, Spanned<Token>, Err<'src>> + Clone {
	// Grouped by leading character; within each group, longer operators come first so
	// e.g. `..=` is preferred over `..` over `.`.
	let dots = choice([
		just("..=").to(Token::DotDotEq),
		just("...").to(Token::DotDotDot),
		just("..").to(Token::DotDot),
		just(".").to(Token::Dot),
	]);
	let stars = choice([just("**").to(Token::StarStar), just("*").to(Token::Star)]);
	let angles = choice([
		just("<=").to(Token::LtEq),
		just("<").to(Token::Lt),
		just(">=").to(Token::GtEq),
		just(">").to(Token::Gt),
	]);
	let questions = choice([
		just("?.").to(Token::QuestionDot),
		just("??").to(Token::DoubleQuestion),
		just("?").to(Token::Question),
	]);
	let dashes = choice([just("->").to(Token::Arrow), just("-").to(Token::Minus)]);
	let pipes = choice([
		just("|>").to(Token::PipeArrow),
		just("||").to(Token::PipePipe),
		just("|").to(Token::Pipe),
	]);
	let amps = choice([just("&&").to(Token::AmpAmp), just("&").to(Token::Amp)]);
	let carets = just("^").to(Token::Caret);
	let tildes = just("~").to(Token::Tilde);
	let eqs = choice([just("==").to(Token::EqEq), just("=").to(Token::Eq)]);
	let bangs = choice([just("!=").to(Token::BangEq), just("!").to(Token::Bang)]);
	let pluses = just("+").to(Token::Plus);
	let slashes = just("/").to(Token::Slash);
	let percents = just("%").to(Token::Percent);
	let colons = choice([just("::").to(Token::ColonColon), just(":").to(Token::Colon)]);
	let hashes = choice([
		just("#(").to(Token::HashLParen),
		just("#[").to(Token::HashLBracket),
		just("#{").to(Token::HashLBrace),
	]);
	let singles = choice([
		just("(").to(Token::LParen),
		just(")").to(Token::RParen),
		just("[").to(Token::LBracket),
		just("]").to(Token::RBracket),
		just("{").to(Token::LBrace),
		just("}").to(Token::RBrace),
		just(",").to(Token::Comma),
		just(";").to(Token::Semicolon),
		just("@").to(Token::At),
	]);

	choice((
		dots, stars, angles, questions, dashes, pipes, amps, carets, tildes, eqs, bangs, pluses,
		slashes, percents, colons, hashes, singles,
	))
	.map_with(|v, e| Spanned::new(v, e.span()))
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
	fn fast_lexer_matches_chumsky() {
		for source in [
			include_str!("../../../stdlib/src/collections/list.nym"),
			include_str!("../../../stdlib/src/math/complex.nym"),
			include_str!("../../../examples/shapes/src/main.nym"),
			"0x 0o8 0b2 1_ 1.0e 1e+ 2f 3u _ λ λ2",
			"#[1, 2] #{1: 2} #() a...b a..=b a?.b a??b a::b",
			r#"'a' '\n' '\u1f600' "text $ text \n \u1f600 \${x} ${f("}", {1})}""#,
			"// comment\n/* block */ func f($255): int = 1_000",
		] {
			let mut fast = FastLexer::new(source)
				.lex()
				.unwrap_or_else(|| panic!("fast lexer rejected valid source: {source:?}"));
			normalize_tokens(&mut fast);
			let expected = lex_with_chumsky(source);
			assert!(
				expected.diagnostics.is_empty(),
				"{:?}",
				expected.diagnostics
			);
			assert_eq!(fast, expected.tokens, "token mismatch for {source:?}");
		}
	}

	#[test]
	fn malformed_source_uses_unchanged_chumsky_diagnostics() {
		for source in [
			"@bad-token `",
			"/* unterminated",
			"'unterminated",
			r#""unterminated"#,
			r#""${{ 1 }""#,
			"$256",
			"18446744073709551616",
			r"'\u110000'",
		] {
			assert!(FastLexer::new(source).lex().is_none());
			let actual = lex(source);
			let expected = lex_with_chumsky(source);
			assert_eq!(actual.tokens, expected.tokens);
			assert_eq!(actual.diagnostics, expected.diagnostics);
			assert_eq!(actual.incomplete, expected.incomplete);
		}
	}
}
