//! `textDocument/semanticTokens/full`: classify every token in a document
//! from the compiler's own lexer + parser, so the editor colours tokens by
//! what they actually *are* rather than by a TextMate grammar's best guess.
//! This is also the robust fix for a match arm's `->`, which a
//! punctuation-only grammar can't distinguish from a function-type arrow —
//! here it always lexes to [`Token::Arrow`] and always classifies as
//! `operator`.
//!
//! Three data sources, all best-effort (never panics, even over malformed
//! input — mirrors [`crate::definition`] and [`crate::document_symbols`]):
//!
//! - [`nymph_syntax::lex`] gives the flat token stream: keywords, operators
//!   (including `->`), delimiters, literals. Every non-identifier token
//!   classifies directly from its [`Token`] variant.
//! - [`nymph_syntax::parse_module`] gives the AST, walked twice into a
//!   `span-start -> (type, modifiers)` map ([`build_role_map`]): phase 1
//!   (unchanged) covers every declaration-site identifier — a function
//!   name, a struct field, a parameter, a plain local. Phase 2 covers *use*
//!   sites, so the same name colours consistently wherever it appears: an
//!   enum-variant reference/construction/pattern classifies as
//!   `enumMember` (not just at its declaration), a parameter/local-binder
//!   identifier resolves to `parameter`/`variable` via the same scope
//!   machinery [`crate::definition`] uses
//!   ([`nymph_sema::query::definition_at`]), and a call-argument/
//!   struct-field LABEL (`hello` in `f(hello = x)`) classifies as
//!   `property`, distinct from an ordinary variable.
//! - [`nymph_sema::check_module`] resolves which enum variant a
//!   construction/reference/pattern names precisely (disambiguating
//!   `None`/`Ok`-style bare names) — run fresh per request (mirrors
//!   `hover.rs`'s uncached path), tolerant of partial/erroring input. Where
//!   a checker resolution is unavailable, phase 2 falls back to a
//!   name-set match against the module's own declared enum variants,
//!   deferring to an in-scope binder of the same name. An identifier whose
//!   role isn't determined by any of this falls back to `variable` rather
//!   than risk a wrong classification.
//!
//! Comments are not tokens at all — the lexer discards them — so they are
//! recovered by scanning the gaps between consecutive lexed tokens for `//`
//! and `/* … */` runs. A string's `//` can never be mistaken for a comment
//! this way, because it sits inside the `Str` token's own span, never in a
//! gap.
//!
//! A [`Token::Str`] is not one opaque span: it is expanded
//! fragment-by-fragment ([`push_str_token`]), so plain text and escapes stay
//! `string` while each `${ … }` interpolation's own tokens — an identifier,
//! a keyword, an operator, a number, even a nested string — recurse through
//! the same classification as the top-level stream (via [`push_token`]) and
//! the same AST role map, rather than being swallowed into the literal.

use std::collections::{HashMap, HashSet};

use lsp_types::{
	SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
	SemanticTokensParams, SemanticTokensResult,
};
use nymph_ast::{
	Ident, Span, Spanned,
	decl::{
		Declaration, EnumVariant, FuncDeclaration, ImplMember, InterfaceElement, InterfaceMember,
		LetDeclaration, Module, StructField, StructImpl,
	},
	expr::{
		Expr, ExprKind, ListItem, ListPatternEntry, MapEntry, MapPatternEntry, Pattern, RangeKind,
		Statement, StringPart, StructPatternField,
	},
	token::{StrFragment, Token},
	ty::{GenericArg, GenericParam, Type},
};
use nymph_sema::Checked;

use crate::{compiler_state::AnalysisSnapshot, line_index::LineIndex};

// ── Legend (fixed index order — the classifier below must never reference
// an index outside this table) ──────────────────────────────────────────

const KEYWORD: u32 = 0;
const OPERATOR: u32 = 1;
const TYPE: u32 = 2;
const FUNCTION: u32 = 3;
const METHOD: u32 = 4;
const VARIABLE: u32 = 5;
const PARAMETER: u32 = 6;
const PROPERTY: u32 = 7;
const ENUM_MEMBER: u32 = 8;
const STRING: u32 = 9;
const NUMBER: u32 = 10;
const COMMENT: u32 = 11;
#[expect(
	dead_code,
	reason = "reserved legend slot: no AST node currently classifies as namespace besides a top-level `namespace` name, which is handled as `type`; kept for a future refinement (import path segments)"
)]
const NAMESPACE: u32 = 12;

const DECLARATION: u32 = 1 << 0;
const READONLY: u32 = 1 << 1;
#[expect(
	dead_code,
	reason = "reserved modifier bit: no builtin is tagged defaultLibrary yet"
)]
const DEFAULT_LIBRARY: u32 = 1 << 2;

/// The fixed token-type/modifier legend this server advertises and encodes
/// against. Must be registered verbatim in `server_capabilities()`.
#[must_use]
pub fn legend() -> SemanticTokensLegend {
	SemanticTokensLegend {
		token_types: vec![
			SemanticTokenType::KEYWORD,
			SemanticTokenType::OPERATOR,
			SemanticTokenType::TYPE,
			SemanticTokenType::FUNCTION,
			SemanticTokenType::METHOD,
			SemanticTokenType::VARIABLE,
			SemanticTokenType::PARAMETER,
			SemanticTokenType::PROPERTY,
			SemanticTokenType::ENUM_MEMBER,
			SemanticTokenType::STRING,
			SemanticTokenType::NUMBER,
			SemanticTokenType::COMMENT,
			SemanticTokenType::NAMESPACE,
		],
		token_modifiers: vec![
			SemanticTokenModifier::DECLARATION,
			SemanticTokenModifier::READONLY,
			SemanticTokenModifier::DEFAULT_LIBRARY,
		],
	}
}

/// Answer a `textDocument/semanticTokens/full` request: `None` when the
/// document isn't open, matching [`crate::definition::definition`] and
/// [`crate::document_symbols::document_symbols`].
#[must_use]
#[cfg(not(test))]
pub fn semantic_tokens_full(
	snapshot: &AnalysisSnapshot,
	params: &SemanticTokensParams,
) -> Option<SemanticTokensResult> {
	semantic_tokens_snapshot(snapshot, params)
}

pub(crate) fn semantic_tokens_snapshot(
	snapshot: &AnalysisSnapshot,
	params: &SemanticTokensParams,
) -> Option<SemanticTokensResult> {
	let _ = params;
	let text = snapshot.source.as_ref();
	let checked = nymph_sema::Checked {
		diags: Vec::new(),
		facts: snapshot.analysis.semantic.checked.as_ref().clone(),
	};
	let roles = build_role_map(&snapshot.analysis.semantic.module, &checked);
	Some(semantic_tokens_for_source(text, &roles))
}

fn semantic_tokens_for_source(text: &str, roles: &RoleMap) -> SemanticTokensResult {
	let index = LineIndex::new(text);
	let lexed = nymph_syntax::lex(text);

	let mut items: Vec<(Span, u32, u32)> = Vec::new();

	for spanned in &lexed.tokens {
		push_token(spanned, roles, &mut items);
	}

	for span in comment_spans(text, &lexed.tokens) {
		items.push((span, COMMENT, 0));
	}

	let mut pieces: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
	for (span, ty, mods) in items {
		for (line, col, len) in split_span_lines(text, &index, span) {
			if len > 0 {
				pieces.push((line, col, len, ty, mods));
			}
		}
	}

	SemanticTokensResult::Tokens(SemanticTokens {
		result_id: None,
		data: encode(pieces),
	})
}

#[cfg(test)]
pub fn semantic_tokens_full(
	docs: &crate::document_store::DocumentStore,
	params: &SemanticTokensParams,
) -> Option<SemanticTokensResult> {
	let uri = &params.text_document.uri;
	let document = docs.get(uri)?;
	let mut owned_docs = docs.clone();
	let mut state = crate::compiler_state::CompilerState::new();
	state
		.open(
			&mut owned_docs,
			uri.clone(),
			document.text.clone(),
			document.version,
		)
		.ok()?;
	match state.analysis_for_uri(&owned_docs, uri) {
		Some(snapshot) => semantic_tokens_snapshot(&snapshot, params),
		None => Some(semantic_tokens_for_source(
			&document.text,
			&RoleMap::default(),
		)),
	}
}

/// Classify one lexed token and push its piece(s) into `items`. Identifiers
/// resolve through the AST-built `roles` map; a [`Token::Str`] is expanded
/// fragment-by-fragment (see [`push_str_token`]) rather than pushed as one
/// span, so an interpolated `${ … }` expression's own tokens get real
/// classification instead of being swallowed into `string`. Every other
/// token classifies directly via [`lexer_token_type`].
fn push_token(spanned: &Spanned<Token>, roles: &RoleMap, items: &mut Vec<(Span, u32, u32)>) {
	let span = spanned.1;
	match &spanned.0 {
		Token::Identifier(_) | Token::AnonymousParam(_) => {
			let (ty, mods) = roles.get(&span.start).copied().unwrap_or((VARIABLE, 0));
			items.push((span, ty, mods));
		}
		Token::Str(fragments) => push_str_token(span, fragments, roles, items),
		other => {
			if let Some(ty) = lexer_token_type(other) {
				items.push((span, ty, 0));
			}
		}
	}
}

/// Expand a string literal's fragments. Plain text, escapes, and the `${`/`}`
/// interpolation delimiters accumulate into contiguous `string` runs (so a
/// literal with no interpolation still yields exactly one `string` piece,
/// matching the pre-interpolation behaviour); each interpolated expression's
/// own tokens are classified individually via [`push_token`] (recursing for a
/// nested string literal), breaking the run.
fn push_str_token(
	span: Span,
	fragments: &[Spanned<StrFragment>],
	roles: &RoleMap,
	items: &mut Vec<(Span, u32, u32)>,
) {
	// Plain text/escape fragments need no action here: they simply extend
	// whatever `string` run is pending, which is flushed lazily — either when
	// an interpolation breaks it, or at the very end.
	let mut run_start = span.start;

	for frag in fragments {
		if let StrFragment::Interpolation(inner) = &frag.0 {
			let open_end = inner.first().map_or(frag.1.end, |t| t.1.start);
			if open_end > run_start {
				items.push((Span::new(run_start, open_end), STRING, 0));
			}

			for tok in inner {
				push_token(tok, roles, items);
			}

			run_start = inner.last().map_or(open_end, |t| t.1.end);
		}
	}

	if span.end > run_start {
		items.push((Span::new(run_start, span.end), STRING, 0));
	}
}

/// Direct token -> legend-index classification for every [`Token`] variant
/// except `Identifier`/`AnonymousParam` (resolved from the AST instead).
/// `None` means "skip" — structural delimiters and lexer-error placeholders
/// have no legend slot and are left to the TextMate grammar.
fn lexer_token_type(token: &Token) -> Option<u32> {
	use Token::*;
	Some(match token {
		Public | Internal | Private | Import | With | Type | Struct | Enum | Let | Mut | External
		| Func | Interface | Impl | Namespace | For | While | If | Else | Match | Continue | Break
		| Return | This | In | As | Is | Async | Await | True | False => KEYWORD,

		IntType | UIntType | FloatType | BooleanType | CharType | StringType | VoidType | NeverType
		| SelfType => TYPE,

		Arrow | DotDotDot | Question | DoubleQuestion | QuestionDot | Dot | PipeArrow | Bang | Plus
		| Minus | Star | Slash | Percent | StarStar | Amp | Pipe | Caret | Tilde | EqEq | BangEq
		| Lt | Gt | LtEq | GtEq | BangIn | BangIs | AmpAmp | PipePipe | Eq | PlusEq | MinusEq
		| StarEq | SlashEq | PercentEq | StarStarEq | AmpAmpEq | PipePipeEq | AmpEq | PipeEq
		| CaretEq | TildeEq | LtLtEq | GtGtEq | DotDot | DotDotEq | ColonColon => OPERATOR,

		Int(_) | UInt(_) | Float(_) => NUMBER,
		Str(_) | Char(_) => STRING,

		LParen | RParen | LBracket | RBracket | LBrace | RBrace | HashLParen | HashLBracket
		| HashLBrace | Comma | Semicolon | Colon | At | Underscore | Error => return None,

		Identifier(_) | AnonymousParam(_) => return None,
	})
}

/// Recover comment spans (the lexer discards them entirely) by scanning the
/// gaps between consecutive lexed tokens, plus the leading/trailing gaps
/// against the whole document.
fn comment_spans(text: &str, tokens: &[Spanned<Token>]) -> Vec<Span> {
	let mut spans = Vec::new();
	let mut prev_end = 0usize;
	for spanned in tokens {
		let span = spanned.1;
		if span.start > prev_end {
			scan_comments_into(text, prev_end, span.start, &mut spans);
		}
		prev_end = prev_end.max(span.end);
	}
	if prev_end < text.len() {
		scan_comments_into(text, prev_end, text.len(), &mut spans);
	}
	spans
}

/// Scan `text[start..end]` (known to hold only whitespace and comments) for
/// `//` and `/* … */` runs, pushing their spans in source order. An
/// unterminated block comment runs to `end` (matches the lexer's own
/// recovery).
fn scan_comments_into(text: &str, start: usize, end: usize, out: &mut Vec<Span>) {
	let gap = &text[start..end];
	let bytes = gap.as_bytes();
	let mut i = 0usize;
	while i < bytes.len() {
		if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
			let comment_start = start + i;
			let mut j = i;
			while j < bytes.len() && bytes[j] != b'\n' {
				j += 1;
			}
			out.push(Span::new(comment_start, start + j));
			i = j;
		} else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
			let comment_start = start + i;
			if let Some(rel) = gap[i + 2..].find("*/") {
				let comment_end = start + i + 2 + rel + 2;
				out.push(Span::new(comment_start, comment_end));
				i = i + 2 + rel + 2;
			} else {
				out.push(Span::new(comment_start, end));
				i = bytes.len();
			}
		} else {
			i += 1;
		}
	}
}

/// Split a byte [`Span`] into per-line `(line, utf16_start_char, utf16_len)`
/// pieces, as the LSP spec requires for tokens spanning multiple lines
/// (block comments; strings, since the lexer's text fragment admits a
/// literal newline). A single-line span yields exactly one piece.
fn split_span_lines(text: &str, index: &LineIndex, span: Span) -> Vec<(u32, u32, u32)> {
	let start_pos = index.position(text, span.start);
	let end_pos = index.position(text, span.end);
	if start_pos.line == end_pos.line {
		return vec![(
			start_pos.line,
			start_pos.character,
			end_pos.character - start_pos.character,
		)];
	}

	let mut pieces = Vec::new();
	let mut line = start_pos.line;
	let mut char_start = start_pos.character;
	let mut len: u32 = 0;
	for ch in text[span.start..span.end].chars() {
		if ch == '\n' {
			pieces.push((line, char_start, len));
			line += 1;
			char_start = 0;
			len = 0;
		} else {
			len += ch.len_utf16() as u32;
		}
	}
	pieces.push((line, char_start, len));
	pieces
}

/// Delta-encode absolute `(line, start_char, len, type_idx, mods)` tuples —
/// already sorted by `(line, start_char)` — into the wire's `SemanticToken`
/// vector.
fn encode(mut items: Vec<(u32, u32, u32, u32, u32)>) -> Vec<SemanticToken> {
	items.sort_by_key(|&(line, col, ..)| (line, col));

	let mut result = Vec::with_capacity(items.len());
	let mut prev_line = 0u32;
	let mut prev_char = 0u32;
	for (line, col, length, token_type, token_modifiers_bitset) in items {
		let delta_line = line - prev_line;
		let delta_start = if delta_line == 0 {
			col - prev_char
		} else {
			col
		};
		result.push(SemanticToken {
			delta_line,
			delta_start,
			length,
			token_type,
			token_modifiers_bitset,
		});
		prev_line = line;
		prev_char = col;
	}
	result
}

// ── AST role resolution ─────────────────────────────────────────────────

type RoleMap = HashMap<usize, (u32, u32)>;

/// Two-phase build: phase 1 (unchanged) maps every declaration-site (and
/// type-reference) identifier's byte-span start to its `(type, modifiers)`
/// legend classification; phase 2 resolves every *use* site — an enum
/// variant reference/construction/pattern, a parameter/local-binder
/// identifier use, and a call-argument/struct-field label — to a
/// classification consistent with its declaration, using `checked`'s
/// recorded variant resolutions plus the existing scope machinery
/// ([`nymph_sema::query::definition_at`]/[`nymph_sema::query::scope_names_at`]).
/// Phase 2 must run after phase 1 completes (not interleaved) so a
/// forward-referenced top-level declaration is already in the map by the
/// time any of its uses are resolved. Uses never overwrite an existing
/// entry — decl-site and use-site spans are always disjoint byte positions,
/// but `or_insert` keeps that invariant even if two use-walks ever visit the
/// same span twice.
fn build_role_map(module: &Module, checked: &Checked) -> RoleMap {
	let mut map = RoleMap::new();
	for decl in &module.members {
		walk_decl(decl, &mut map);
	}

	let variant_names = collect_enum_variant_names(module);
	let mut uses: Vec<(usize, (u32, u32))> = Vec::new();
	for decl in &module.members {
		walk_decl_uses(decl, module, checked, &variant_names, &map, &mut uses);
	}
	for (start, role) in uses {
		map.entry(start).or_insert(role);
	}

	map
}

fn walk_decl(decl: &Declaration, map: &mut RoleMap) {
	match decl {
		Declaration::Import { .. } => {}
		Declaration::Let { meta, value, .. } => {
			bind_let(meta, map);
			walk_type_opt(&meta.type_, map);
			walk_expr(value, map);
		}
		Declaration::ExternalLet(_, _, meta) => {
			bind_let(meta, map);
			walk_type_opt(&meta.type_, map);
		}
		Declaration::Func { meta, body, .. } => {
			bind_func(meta, FUNCTION, map);
			walk_expr(body, map);
		}
		Declaration::ExternalFunc(_, _, meta) => {
			bind_func(meta, FUNCTION, map);
		}
		Declaration::TypeAlias { meta, value, .. } => {
			map.insert(meta.name.1.start, (TYPE, DECLARATION));
			walk_generics(&meta.generics, map);
			walk_type(value, map);
		}
		Declaration::Struct {
			name,
			generics,
			fields,
			members,
			impls,
			..
		} => {
			map.insert(name.1.start, (TYPE, DECLARATION));
			walk_generics(generics, map);
			for f in fields {
				walk_struct_field(f, map);
			}
			for m in members {
				walk_impl_member(m, map);
			}
			for si in impls {
				walk_struct_impl(si, map);
			}
		}
		Declaration::Enum {
			name,
			generics,
			variants,
			members,
			impls,
			..
		} => {
			map.insert(name.1.start, (TYPE, DECLARATION));
			walk_generics(generics, map);
			for v in variants {
				walk_enum_variant(v, map);
			}
			for m in members {
				walk_impl_member(m, map);
			}
			for si in impls {
				walk_struct_impl(si, map);
			}
		}
		Declaration::Namespace { name, members, .. } => {
			map.insert(name.1.start, (TYPE, DECLARATION));
			for m in members {
				walk_impl_member(m, map);
			}
		}
		Declaration::Interface {
			name,
			generics,
			super_interfaces,
			members,
			..
		} => {
			map.insert(name.1.start, (TYPE, DECLARATION));
			walk_generics(generics, map);
			for si in super_interfaces {
				let (_, args) = &si.0;
				for g in args {
					walk_generic_arg(g, map);
				}
			}
			for m in members {
				walk_interface_member(m, map);
			}
		}
		Declaration::Impl {
			generics,
			type_,
			members,
			..
		} => {
			walk_generics(generics, map);
			walk_type(type_, map);
			for m in members {
				walk_impl_member(m, map);
			}
		}
		Declaration::ImplFor {
			generics,
			type_,
			for_interface,
			members,
			..
		} => {
			walk_generics(generics, map);
			walk_type(type_, map);
			for g in &for_interface.1 {
				walk_generic_arg(g, map);
			}
			for m in members {
				walk_impl_member(m, map);
			}
		}
	}
}

fn bind_let(meta: &LetDeclaration, map: &mut RoleMap) {
	if let Some(name) = meta.name.0.as_binding() {
		let mods = if meta.is_mutable() {
			DECLARATION
		} else {
			DECLARATION | READONLY
		};
		map.insert(name.1.start, (VARIABLE, mods));
	}
}

fn bind_func(meta: &FuncDeclaration, kind: u32, map: &mut RoleMap) {
	map.insert(meta.name.1.start, (kind, DECLARATION));
	walk_generics(&meta.generics, map);
	for p in &meta.params {
		if let Some(name) = p.0.name.0.as_binding() {
			map.insert(name.1.start, (PARAMETER, DECLARATION));
		}
		walk_type(&p.0.type_, map);
	}
	walk_type_opt(&meta.return_type, map);
}

fn walk_struct_field(f: &Spanned<StructField>, map: &mut RoleMap) {
	map.insert(f.0.name.1.start, (PROPERTY, DECLARATION));
	walk_type(&f.0.type_, map);
	if let Some(default) = &f.0.default {
		walk_expr(default, map);
	}
}

fn walk_enum_variant(v: &Spanned<EnumVariant>, map: &mut RoleMap) {
	map.insert(v.0.name.1.start, (ENUM_MEMBER, DECLARATION));
	for f in &v.0.fields {
		walk_struct_field(f, map);
	}
}

fn walk_impl_member(m: &Spanned<ImplMember>, map: &mut RoleMap) {
	match &m.0 {
		ImplMember::Let { meta, value, .. } => {
			bind_let(meta, map);
			walk_type_opt(&meta.type_, map);
			walk_expr(value, map);
		}
		ImplMember::ExternalLet(_, _, meta) => {
			bind_let(meta, map);
			walk_type_opt(&meta.type_, map);
		}
		ImplMember::Func { meta, body, .. } => {
			bind_func(meta, METHOD, map);
			walk_expr(body, map);
		}
		ImplMember::ExternalFunc(_, _, meta) => {
			bind_func(meta, METHOD, map);
		}
	}
}

fn walk_struct_impl(si: &Spanned<StructImpl>, map: &mut RoleMap) {
	walk_generics(&si.0.generics, map);
	let (_, args) = &si.0.interface;
	for g in args {
		walk_generic_arg(g, map);
	}
	for m in &si.0.members {
		walk_impl_member(m, map);
	}
}

fn walk_interface_member(m: &Spanned<InterfaceMember>, map: &mut RoleMap) {
	match &m.0 {
		InterfaceMember::Element(elem) => match &elem.0 {
			InterfaceElement::Let { meta, value } => {
				bind_let(meta, map);
				walk_type_opt(&meta.type_, map);
				if let Some(v) = value {
					walk_expr(v, map);
				}
			}
			InterfaceElement::Func { meta, body } => {
				bind_func(meta, METHOD, map);
				if let Some(b) = body {
					walk_expr(b, map);
				}
			}
		},
		InterfaceMember::Impl {
			interface,
			generics,
			members,
		} => {
			walk_generics(generics, map);
			let (_, args) = interface;
			for g in args {
				walk_generic_arg(g, map);
			}
			for m in members {
				walk_impl_member(m, map);
			}
		}
	}
}

fn walk_generics(generics: &[Spanned<GenericParam>], map: &mut RoleMap) {
	for g in generics {
		map.insert(g.0.name.1.start, (TYPE, DECLARATION));
		if let Some(c) = &g.0.constraint {
			walk_type(c, map);
		}
		if let Some(d) = &g.0.default {
			walk_type(d, map);
		}
	}
}

fn walk_generic_arg(arg: &Spanned<GenericArg>, map: &mut RoleMap) {
	walk_type(&arg.0.value, map);
}

fn walk_type_opt(opt: &Option<Spanned<Type>>, map: &mut RoleMap) {
	if let Some(t) = opt {
		walk_type(t, map);
	}
}

fn walk_type(ty: &Spanned<Type>, map: &mut RoleMap) {
	match &ty.0 {
		Type::Int
		| Type::UInt
		| Type::Float
		| Type::Char
		| Type::String
		| Type::Boolean
		| Type::Void
		| Type::Never
		| Type::SelfType
		| Type::Infer => {}
		Type::Intersection(a, b) => {
			walk_type(a, map);
			walk_type(b, map);
		}
		Type::List(a) => walk_type(a, map),
		Type::Tuple(items) => {
			for it in items {
				walk_type(it, map);
			}
		}
		Type::Map(k, v) => {
			walk_type(k, map);
			walk_type(v, map);
		}
		Type::Function {
			params,
			return_type,
		} => {
			for (_, t) in params {
				walk_type(t, map);
			}
			walk_type(return_type, map);
		}
		Type::Reference { name, generics } => {
			map.insert(name.1.start, (TYPE, 0));
			for g in generics {
				walk_generic_arg(g, map);
			}
		}
		Type::Grouped(a) | Type::Mut(a) => walk_type(a, map),
	}
}

fn walk_expr(expr: &Expr, map: &mut RoleMap) {
	match &expr.kind {
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::Identifier(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::This
		| ExprKind::Continue { .. } => {}
		ExprKind::String(parts) => {
			for part in parts {
				if let StringPart::InterpolatedExpr(e) = &part.0 {
					walk_expr(e, map);
				}
			}
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(e) | ListItem::Spread(e) => walk_expr(e, map),
				}
			}
		}
		ExprKind::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Entry(k, v) => {
						walk_expr(k, map);
						walk_expr(v, map);
					}
					MapEntry::Spread(e) => walk_expr(e, map),
				}
			}
		}
		ExprKind::Range(kind) => match kind {
			RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => walk_expr(e, map),
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				walk_expr(min, map);
				walk_expr(max, map);
			}
		},
		ExprKind::Call {
			func,
			generics,
			args,
		} => {
			walk_expr(func, map);
			for g in generics {
				walk_generic_arg(g, map);
			}
			for a in args {
				walk_expr(&a.0.value, map);
			}
		}
		ExprKind::MemberAccess { parent, .. } => walk_expr(parent, map),
		ExprKind::IndexAccess { parent, index, .. } => {
			walk_expr(parent, map);
			walk_expr(index, map);
		}
		ExprKind::Closure {
			params,
			generics,
			return_type,
			body,
			..
		} => {
			walk_generics(generics, map);
			for p in params {
				if let Some(name) = p.0.name.0.as_binding() {
					map.insert(name.1.start, (PARAMETER, DECLARATION));
				}
				walk_type_opt(&p.0.type_, map);
			}
			walk_type_opt(return_type, map);
			walk_expr(body, map);
		}
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => walk_expr(value, map),
		ExprKind::BinaryOp { lhs, rhs, .. } => {
			walk_expr(lhs, map);
			walk_expr(rhs, map);
		}
		ExprKind::TypeOp { lhs, rhs, .. } => {
			walk_expr(lhs, map);
			walk_type(rhs, map);
		}
		ExprKind::PatternOp { lhs, .. } => walk_expr(lhs, map),
		ExprKind::AssignOp { lhs, rhs, .. } => {
			walk_expr(lhs, map);
			walk_expr(rhs, map);
		}
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			if let Some(v) = value {
				walk_expr(v, map);
			}
		}
		ExprKind::While {
			condition, body, ..
		} => {
			walk_expr(condition, map);
			walk_expr(body, map);
		}
		ExprKind::For { iterable, body, .. } => {
			walk_expr(iterable, map);
			walk_expr(body, map);
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			walk_expr(condition, map);
			walk_expr(then, map);
			if let Some(o) = otherwise {
				walk_expr(o, map);
			}
		}
		ExprKind::Match { value, arms } => {
			walk_expr(value, map);
			for arm in arms {
				if let Some(g) = &arm.guard {
					walk_expr(g, map);
				}
				walk_expr(&arm.body, map);
			}
		}
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => walk_expr(e, map),
					Statement::Let { meta, value } => {
						bind_let(meta, map);
						walk_type_opt(&meta.type_, map);
						walk_expr(value, map);
					}
				}
			}
		}
		ExprKind::Grouped(e) => walk_expr(e, map),
	}
}

// ── AST use-site resolution (phase 2) ───────────────────────────────────
//
// Mirrors `walk_decl`/`walk_expr`'s traversal shape, but instead of binding
// declaration-site names it resolves *use* sites: an identifier that names
// an enum variant, a parameter/local binder, or a top-level function; a
// pattern that matches an enum variant; and a call-argument/struct-field
// label. Pushes `(span_start, (type, modifiers))` pairs into `out` rather
// than inserting into the (already-complete) decl `map`, so a forward
// reference always resolves — see `build_role_map`.

/// Every declared enum variant's name, module-wide (variants live only on a
/// top-level `Declaration::Enum` — never nested inside a namespace/impl
/// block), for the name-set fallback used when a checker resolution is
/// unavailable (a still-erroring subtree). Borrowed from `module`, so this
/// carries no allocation cost beyond the `HashSet` itself.
fn collect_enum_variant_names(module: &Module) -> HashSet<&str> {
	let mut names = HashSet::new();
	for decl in &module.members {
		if let Declaration::Enum { variants, .. } = decl {
			for v in variants {
				names.insert(v.0.name.0.as_str());
			}
		}
	}
	names
}

/// Whether `name` is both a known enum-variant name and NOT shadowed by an
/// in-scope local/parameter binder at its own position — the name-set
/// fallback's shadowing guard, used only when a checker resolution
/// ([`nymph_sema::Annotations::variant_of`]/`pattern_variant_of`) is
/// unavailable for this exact node/span.
fn is_unshadowed_variant(module: &Module, variant_names: &HashSet<&str>, name: &Ident) -> bool {
	variant_names.contains(name.0.as_str())
		&& !nymph_sema::query::scope_names_at_exact(module, name.1.start)
			.unwrap_or_default()
			.iter()
			.any(|n| n == name.0.as_str())
}

/// Resolve an `ExprKind::Identifier` use to its legend role, checker-backed
/// first: a variant reference ([`nymph_sema::Annotations::variant_of`]) wins
/// outright — this MUST run before any scope-based resolution, because a
/// bare variant name (`Square`) can otherwise mis-resolve against a
/// same-named nullary-variant *pattern* elsewhere, which the purely-AST
/// `definition_at` cannot itself distinguish from a real binder. A local
/// that shadows a variant name gets `variant_of == None` (the checker
/// already resolved it as the binder), so shadowing falls out for free.
/// Falls through to the name-set fallback, then to
/// [`nymph_sema::query::definition_at`] resolved against the decl `map`
/// (dropping every modifier — `roles.get` never needs a `declaration`/
/// `readonly` bit at a use site); `None` (→ the `variable` default) when
/// nothing resolves, matching a still-erroring or unresolvable identifier.
fn identifier_role(
	module: &Module,
	checked: &Checked,
	decls: &RoleMap,
	variant_names: &HashSet<&str>,
	expr: &Expr,
	name: &Ident,
) -> Option<(u32, u32)> {
	if checked.annotations.variant_of(expr.id).is_some() {
		return Some((ENUM_MEMBER, 0));
	}
	if is_unshadowed_variant(module, variant_names, name) {
		return Some((ENUM_MEMBER, 0));
	}
	if let Some(decl_span) = nymph_sema::query::definition_at(module, expr.span.start)
		&& let Some(&(ty, _)) = decls.get(&decl_span.start)
		&& matches!(ty, PARAMETER | VARIABLE | FUNCTION | METHOD)
	{
		return Some((ty, 0));
	}
	None
}

fn walk_decl_uses(
	decl: &Declaration,
	module: &Module,
	checked: &Checked,
	variant_names: &HashSet<&str>,
	decls: &RoleMap,
	out: &mut Vec<(usize, (u32, u32))>,
) {
	match decl {
		Declaration::Import { .. }
		| Declaration::ExternalLet(..)
		| Declaration::ExternalFunc(..)
		| Declaration::TypeAlias { .. } => {}
		Declaration::Let { value, .. } | Declaration::Func { body: value, .. } => {
			walk_expr_uses(value, module, checked, variant_names, decls, out);
		}
		Declaration::Struct {
			fields,
			members,
			impls,
			..
		} => {
			for f in fields {
				if let Some(default) = &f.0.default {
					walk_expr_uses(default, module, checked, variant_names, decls, out);
				}
			}
			for m in members {
				walk_impl_member_uses(&m.0, module, checked, variant_names, decls, out);
			}
			for si in impls {
				for m in &si.0.members {
					walk_impl_member_uses(&m.0, module, checked, variant_names, decls, out);
				}
			}
		}
		Declaration::Enum { members, impls, .. } => {
			for m in members {
				walk_impl_member_uses(&m.0, module, checked, variant_names, decls, out);
			}
			for si in impls {
				for m in &si.0.members {
					walk_impl_member_uses(&m.0, module, checked, variant_names, decls, out);
				}
			}
		}
		Declaration::Namespace { members, .. } => {
			for m in members {
				walk_impl_member_uses(&m.0, module, checked, variant_names, decls, out);
			}
		}
		Declaration::Interface { members, .. } => {
			for m in members {
				walk_interface_member_uses(&m.0, module, checked, variant_names, decls, out);
			}
		}
		Declaration::Impl { members, .. } | Declaration::ImplFor { members, .. } => {
			for m in members {
				walk_impl_member_uses(&m.0, module, checked, variant_names, decls, out);
			}
		}
	}
}

fn walk_impl_member_uses(
	member: &ImplMember,
	module: &Module,
	checked: &Checked,
	variant_names: &HashSet<&str>,
	decls: &RoleMap,
	out: &mut Vec<(usize, (u32, u32))>,
) {
	match member {
		ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => {}
		ImplMember::Let { value, .. } | ImplMember::Func { body: value, .. } => {
			walk_expr_uses(value, module, checked, variant_names, decls, out);
		}
	}
}

fn walk_interface_member_uses(
	member: &InterfaceMember,
	module: &Module,
	checked: &Checked,
	variant_names: &HashSet<&str>,
	decls: &RoleMap,
	out: &mut Vec<(usize, (u32, u32))>,
) {
	match member {
		InterfaceMember::Element(elem) => match &elem.0 {
			InterfaceElement::Let { value, .. } => {
				if let Some(v) = value {
					walk_expr_uses(v, module, checked, variant_names, decls, out);
				}
			}
			InterfaceElement::Func { body, .. } => {
				if let Some(b) = body {
					walk_expr_uses(b, module, checked, variant_names, decls, out);
				}
			}
		},
		InterfaceMember::Impl { members, .. } => {
			for m in members {
				walk_impl_member_uses(&m.0, module, checked, variant_names, decls, out);
			}
		}
	}
}

fn walk_expr_uses(
	expr: &Expr,
	module: &Module,
	checked: &Checked,
	variant_names: &HashSet<&str>,
	decls: &RoleMap,
	out: &mut Vec<(usize, (u32, u32))>,
) {
	match &expr.kind {
		ExprKind::Int(_)
		| ExprKind::UInt(_)
		| ExprKind::Float(_)
		| ExprKind::Char(_)
		| ExprKind::Boolean(_)
		| ExprKind::AnonymousParam(_)
		| ExprKind::This
		| ExprKind::Continue { .. } => {}
		ExprKind::Identifier(name) => {
			if let Some(role) = identifier_role(module, checked, decls, variant_names, expr, name) {
				out.push((expr.span.start, role));
			}
		}
		ExprKind::String(parts) => {
			for part in parts {
				if let StringPart::InterpolatedExpr(e) = &part.0 {
					walk_expr_uses(e, module, checked, variant_names, decls, out);
				}
			}
		}
		ExprKind::List(items) | ExprKind::Tuple(items) => {
			for item in items {
				match &item.0 {
					ListItem::Expr(e) | ListItem::Spread(e) => {
						walk_expr_uses(e, module, checked, variant_names, decls, out);
					}
				}
			}
		}
		ExprKind::Map(entries) => {
			for entry in entries {
				match &entry.0 {
					MapEntry::Entry(k, v) => {
						walk_expr_uses(k, module, checked, variant_names, decls, out);
						walk_expr_uses(v, module, checked, variant_names, decls, out);
					}
					MapEntry::Spread(e) => walk_expr_uses(e, module, checked, variant_names, decls, out),
				}
			}
		}
		ExprKind::Range(kind) => match kind {
			RangeKind::From(e) | RangeKind::To(e) | RangeKind::ToInclusive(e) => {
				walk_expr_uses(e, module, checked, variant_names, decls, out);
			}
			RangeKind::Exclusive { min, max } | RangeKind::Inclusive { min, max } => {
				walk_expr_uses(min, module, checked, variant_names, decls, out);
				walk_expr_uses(max, module, checked, variant_names, decls, out);
			}
		},
		ExprKind::Call { func, args, .. } => {
			// A construction (`Ok(value = x)`, `Circle(radius = n)`): the
			// checker resolves the variant against the CALL's own NodeId
			// (not `func`'s), matching `lower_hir`'s `variant_new(expr.id)`.
			// When resolved, color the func sub-expression's own name span
			// and skip walking `func` further (it's fully classified);
			// otherwise try the narrow name-set fallback, then fall back to
			// treating `func` as an ordinary use (a real function call).
			let resolved = checked.annotations.variant_of(expr.id).is_some();
			if resolved {
				match &func.kind {
					ExprKind::Identifier(_) => out.push((func.span.start, (ENUM_MEMBER, 0))),
					ExprKind::MemberAccess { member, .. } => {
						out.push((member.1.start, (ENUM_MEMBER, 0)));
					}
					_ => walk_expr_uses(func, module, checked, variant_names, decls, out),
				}
			} else if let ExprKind::Identifier(name) = &func.kind
				&& is_unshadowed_variant(module, variant_names, name)
			{
				out.push((func.span.start, (ENUM_MEMBER, 0)));
			} else {
				walk_expr_uses(func, module, checked, variant_names, decls, out);
			}
			for a in args {
				if let Some(label) = &a.0.name {
					out.push((label.1.start, (PROPERTY, 0)));
				}
				walk_expr_uses(&a.0.value, module, checked, variant_names, decls, out);
			}
		}
		ExprKind::MemberAccess { parent, member, .. } => {
			// A qualified nullary reference (`Result.Ok`): the checker
			// resolves the variant against the MemberAccess node's own id.
			if checked.annotations.variant_of(expr.id).is_some() {
				out.push((member.1.start, (ENUM_MEMBER, 0)));
			} else if let Some(decl_span) = nymph_sema::query::definition_at(module, member.1.start)
				&& let Some(&(ty, _)) = decls.get(&decl_span.start)
				&& ty == METHOD
			{
				// A method access (`this.method`, or any other receiver
				// `definition_at` can resolve): mirrors `identifier_role`'s
				// decl-lookup so the use classifies the same as the decl
				// (`method`) rather than falling through to the `variable`
				// default `push_token` applies when nothing here matches.
				out.push((member.1.start, (METHOD, 0)));
			}
			walk_expr_uses(parent, module, checked, variant_names, decls, out);
		}
		ExprKind::IndexAccess { parent, index, .. } => {
			walk_expr_uses(parent, module, checked, variant_names, decls, out);
			walk_expr_uses(index, module, checked, variant_names, decls, out);
		}
		ExprKind::Closure { body, .. } => {
			walk_expr_uses(body, module, checked, variant_names, decls, out);
		}
		ExprKind::PrefixOp { value, .. } | ExprKind::PostfixOp { value, .. } => {
			walk_expr_uses(value, module, checked, variant_names, decls, out);
		}
		ExprKind::BinaryOp { lhs, rhs, .. } | ExprKind::AssignOp { lhs, rhs, .. } => {
			walk_expr_uses(lhs, module, checked, variant_names, decls, out);
			walk_expr_uses(rhs, module, checked, variant_names, decls, out);
		}
		ExprKind::TypeOp { lhs, .. } => {
			walk_expr_uses(lhs, module, checked, variant_names, decls, out);
		}
		ExprKind::PatternOp { lhs, rhs, .. } => {
			walk_expr_uses(lhs, module, checked, variant_names, decls, out);
			walk_pattern_uses(rhs, module, checked, variant_names, out);
		}
		ExprKind::Return { value, .. } | ExprKind::Break { value, .. } => {
			if let Some(v) = value {
				walk_expr_uses(v, module, checked, variant_names, decls, out);
			}
		}
		ExprKind::While {
			condition, body, ..
		} => {
			walk_expr_uses(condition, module, checked, variant_names, decls, out);
			walk_expr_uses(body, module, checked, variant_names, decls, out);
		}
		ExprKind::For {
			variable,
			iterable,
			body,
			..
		} => {
			walk_expr_uses(iterable, module, checked, variant_names, decls, out);
			walk_pattern_uses(variable, module, checked, variant_names, out);
			walk_expr_uses(body, module, checked, variant_names, decls, out);
		}
		ExprKind::If {
			condition,
			then,
			otherwise,
		} => {
			walk_expr_uses(condition, module, checked, variant_names, decls, out);
			walk_expr_uses(then, module, checked, variant_names, decls, out);
			if let Some(o) = otherwise {
				walk_expr_uses(o, module, checked, variant_names, decls, out);
			}
		}
		ExprKind::Match { value, arms } => {
			walk_expr_uses(value, module, checked, variant_names, decls, out);
			for arm in arms {
				walk_pattern_uses(&arm.pattern, module, checked, variant_names, out);
				if let Some(g) = &arm.guard {
					walk_expr_uses(g, module, checked, variant_names, decls, out);
				}
				walk_expr_uses(&arm.body, module, checked, variant_names, decls, out);
			}
		}
		ExprKind::Block { body, .. } => {
			for stmt in body {
				match &stmt.0 {
					Statement::Expr(e) => walk_expr_uses(e, module, checked, variant_names, decls, out),
					Statement::Let { value, .. } => {
						walk_expr_uses(value, module, checked, variant_names, decls, out);
					}
				}
			}
		}
		ExprKind::Grouped(e) => walk_expr_uses(e, module, checked, variant_names, decls, out),
	}
}

/// Resolve a pattern's own variant-name span(s) and field/element labels,
/// recursing into every sub-pattern (mirroring `pattern_bindings`'s
/// recursion shape in `nymph_sema::query`). Checker-backed first
/// ([`nymph_sema::Annotations::pattern_variant_of`], span-keyed on the whole
/// `Spanned<Pattern>`); the name-set fallback only applies to the two
/// syntactic shapes a variant pattern can actually take — a `Struct` path's
/// last segment (`Circle(radius = v)`) or a bare `Binding` with a
/// `Placeholder` inner (a nullary variant like `Square`/`None`, which parses
/// identically to a plain binder — the `Placeholder` inner plus the name-set
/// membership is what disambiguates it here). The recursion into
/// sub-patterns/labels always runs, independent of whether this pattern
/// itself resolved as a variant.
fn walk_pattern_uses(
	pattern: &Spanned<Pattern>,
	module: &Module,
	checked: &Checked,
	variant_names: &HashSet<&str>,
	out: &mut Vec<(usize, (u32, u32))>,
) {
	if checked.annotations.pattern_variant_of(pattern.1).is_some() {
		match &pattern.0 {
			Pattern::Struct { path, .. } => {
				if let Some(last) = path.last() {
					out.push((last.1.start, (ENUM_MEMBER, 0)));
				}
			}
			Pattern::Binding { name, .. } => out.push((name.1.start, (ENUM_MEMBER, 0))),
			_ => {}
		}
	} else {
		match &pattern.0 {
			Pattern::Struct { path, .. } => {
				if let Some(last) = path.last()
					&& is_unshadowed_variant(module, variant_names, last)
				{
					out.push((last.1.start, (ENUM_MEMBER, 0)));
				}
			}
			Pattern::Binding { name, inner }
				if matches!(inner.0, Pattern::Placeholder)
					&& is_unshadowed_variant(module, variant_names, name) =>
			{
				out.push((name.1.start, (ENUM_MEMBER, 0)));
			}
			_ => {}
		}
	}

	match &pattern.0 {
		Pattern::Binding { inner, .. } => walk_pattern_uses(inner, module, checked, variant_names, out),
		Pattern::List(items) | Pattern::Tuple(items) => {
			for item in items {
				if let ListPatternEntry::Item(p) = &item.0 {
					walk_pattern_uses(p, module, checked, variant_names, out);
				}
			}
		}
		Pattern::Map(entries) => {
			for entry in entries {
				if let MapPatternEntry::Entry(k, v) = &entry.0 {
					walk_pattern_uses(k, module, checked, variant_names, out);
					walk_pattern_uses(v, module, checked, variant_names, out);
				}
			}
		}
		Pattern::Struct { fields, .. } => {
			for field in fields {
				match &field.0 {
					StructPatternField::Value { name, value } => {
						out.push((name.1.start, (PROPERTY, 0)));
						walk_pattern_uses(value, module, checked, variant_names, out);
					}
					StructPatternField::Positional(value) => {
						walk_pattern_uses(value, module, checked, variant_names, out);
					}
					StructPatternField::Named(name) => out.push((name.1.start, (PROPERTY, 0))),
					StructPatternField::Rest => {}
				}
			}
		}
		Pattern::Union(a, b) => {
			walk_pattern_uses(a, module, checked, variant_names, out);
			walk_pattern_uses(b, module, checked, variant_names, out);
		}
		Pattern::Grouped(inner) => walk_pattern_uses(inner, module, checked, variant_names, out),
		_ => {}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::document_store::DocumentStore;
	use lsp_types::{PartialResultParams, TextDocumentIdentifier, Uri, WorkDoneProgressParams};

	fn docs_with(uri: &Uri, text: &str) -> DocumentStore {
		let mut docs = DocumentStore::default();
		docs.open(uri.clone(), text.to_string(), 1);
		docs
	}

	fn params(uri: &Uri) -> SemanticTokensParams {
		SemanticTokensParams {
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: PartialResultParams::default(),
			text_document: TextDocumentIdentifier { uri: uri.clone() },
		}
	}

	/// One decoded token: absolute `(line, col, len)` plus its legend names.
	#[derive(Debug, Clone, PartialEq)]
	struct Decoded {
		line: u32,
		col: u32,
		len: u32,
		type_name: &'static str,
		modifiers: Vec<&'static str>,
	}

	const TYPE_NAMES: [&str; 13] = [
		"keyword",
		"operator",
		"type",
		"function",
		"method",
		"variable",
		"parameter",
		"property",
		"enumMember",
		"string",
		"number",
		"comment",
		"namespace",
	];
	const MODIFIER_NAMES: [&str; 3] = ["declaration", "readonly", "defaultLibrary"];

	fn decode(data: &[SemanticToken]) -> Vec<Decoded> {
		let mut line = 0u32;
		let mut col = 0u32;
		let mut out = Vec::with_capacity(data.len());
		for tok in data {
			if tok.delta_line == 0 {
				col += tok.delta_start;
			} else {
				line += tok.delta_line;
				col = tok.delta_start;
			}
			let modifiers = MODIFIER_NAMES
				.iter()
				.enumerate()
				.filter(|(bit, _)| tok.token_modifiers_bitset & (1 << bit) != 0)
				.map(|(_, name)| *name)
				.collect();
			out.push(Decoded {
				line,
				col,
				len: tok.length,
				type_name: TYPE_NAMES[tok.token_type as usize],
				modifiers,
			});
		}
		out
	}

	fn tokens_for(text: &str) -> Vec<Decoded> {
		let uri: Uri = "file:///semtok.nym".parse().unwrap();
		let docs = docs_with(&uri, text);
		let result = semantic_tokens_full(&docs, &params(&uri)).expect("document is open");
		let SemanticTokensResult::Tokens(tokens) = result else {
			panic!("expected the Tokens arm");
		};
		decode(&tokens.data)
	}

	fn find(decoded: &[Decoded], line: u32, col: u32) -> &Decoded {
		decoded
			.iter()
			.find(|d| d.line == line && d.col == col)
			.unwrap_or_else(|| panic!("no token at {line}:{col} in {decoded:?}"))
	}

	fn assert_sorted_and_non_overlapping(decoded: &[Decoded]) {
		for w in decoded.windows(2) {
			let (a, b) = (&w[0], &w[1]);
			assert!(
				(a.line, a.col) <= (b.line, b.col),
				"tokens out of order: {a:?} then {b:?}"
			);
			if a.line == b.line {
				assert!(
					a.col + a.len <= b.col,
					"overlapping tokens on the same line: {a:?} then {b:?}"
				);
			}
		}
	}

	#[test]
	fn classifies_a_multi_declaration_file() {
		let text = "\
struct Point(x: int)
enum Color { Red }
func f(p: Point): int = match (p) { _ -> 1 } // c
";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		// `struct` keyword.
		assert_eq!(find(&decoded, 0, 0).type_name, "keyword");
		// `Point` struct name, declaration site.
		let point = find(&decoded, 0, 7);
		assert_eq!(point.type_name, "type");
		assert!(point.modifiers.contains(&"declaration"));
		// `x` field, declaration site.
		let x = find(&decoded, 0, 13);
		assert_eq!(x.type_name, "property");
		assert!(x.modifiers.contains(&"declaration"));
		// `int` builtin type keyword.
		assert_eq!(find(&decoded, 0, 16).type_name, "type");

		// `enum` keyword.
		assert_eq!(find(&decoded, 1, 0).type_name, "keyword");
		// `Color` enum name, declaration site.
		let color = find(&decoded, 1, 5);
		assert_eq!(color.type_name, "type");
		assert!(color.modifiers.contains(&"declaration"));
		// `Red` variant, declaration site.
		let red = find(&decoded, 1, 13);
		assert_eq!(red.type_name, "enumMember");
		assert!(red.modifiers.contains(&"declaration"));

		// `func` keyword.
		assert_eq!(find(&decoded, 2, 0).type_name, "keyword");
		// `f` function name, declaration site.
		let f = find(&decoded, 2, 5);
		assert_eq!(f.type_name, "function");
		assert!(f.modifiers.contains(&"declaration"));
		// `p` parameter, declaration site.
		let p = find(&decoded, 2, 7);
		assert_eq!(p.type_name, "parameter");
		assert!(p.modifiers.contains(&"declaration"));
		// `Point` referenced as a type.
		assert_eq!(find(&decoded, 2, 10).type_name, "type");
		// `match` keyword.
		assert_eq!(find(&decoded, 2, 24).type_name, "keyword");
		// the match arm's `->` — the arrow the bug report is about.
		assert_eq!(find(&decoded, 2, 38).type_name, "operator");
		// the trailing `// c` line comment.
		assert_eq!(find(&decoded, 2, 45).type_name, "comment");
	}

	#[test]
	fn classifies_a_string_and_a_number_literal() {
		// Nymph statements are newline-separated, not semicolon-separated.
		let text = "func main(): void = {\n  let s = \"hi\"\n  let n = 42\n}";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		let s = find(&decoded, 1, 10);
		assert_eq!(s.type_name, "string");
		assert_eq!(s.len, 4); // `"hi"`

		let n = find(&decoded, 2, 10);
		assert_eq!(n.type_name, "number");

		// `let` keyword appears twice.
		assert_eq!(find(&decoded, 1, 2).type_name, "keyword");
		assert_eq!(find(&decoded, 2, 2).type_name, "keyword");
	}

	#[test]
	fn an_interpolated_identifier_is_classified_as_a_variable_not_swallowed_into_the_string() {
		let text = "func main(): void = {\n  let name = \"x\"\n  print(\"Hello, ${name}!\")\n}";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		// The interpolated `name` gets its own real classification …
		let name = find(&decoded, 2, 18);
		assert_eq!(name.type_name, "variable");
		assert_eq!(name.len, 4);

		// … distinct from the surrounding string text, which is still `string`
		// (and no single piece spans the whole literal any more).
		let opening = find(&decoded, 2, 8);
		assert_eq!(opening.type_name, "string");
		assert!(
			opening.len < 17,
			"the opening string piece must not swallow the whole `\"Hello, ${{name}}!\"` literal: {opening:?}"
		);
		let closing = find(&decoded, 2, 22);
		assert_eq!(closing.type_name, "string");
	}

	#[test]
	fn an_interpolated_expression_classifies_its_operator_and_number() {
		let text = "func main(): void = {\n  let x = 1\n  print(\"n = ${x + 1}\")\n}";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		// `x` — the interpolated identifier.
		let x = find(&decoded, 2, 15);
		assert_eq!(x.type_name, "variable");

		// `+` — the interpolated operator.
		assert_eq!(find(&decoded, 2, 17).type_name, "operator");

		// `1` — the interpolated number literal.
		assert_eq!(find(&decoded, 2, 19).type_name, "number");
	}

	#[test]
	fn an_interpolated_keyword_literal_classifies_as_a_keyword() {
		let text = "func main(): void = {\n  print(\"${true}\")\n}";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		let literal = find(&decoded, 1, 11);
		assert_eq!(literal.type_name, "keyword");
		assert_eq!(literal.len, 4); // `true`
	}

	#[test]
	fn a_let_binding_is_variable_with_readonly_unless_mut() {
		let text = "func main(): int = { let x = 1\n  let mut y = 2\n  x + y }";
		let decoded = tokens_for(text);

		let x = find(&decoded, 0, 25);
		assert_eq!(x.type_name, "variable");
		assert!(x.modifiers.contains(&"declaration"));
		assert!(x.modifiers.contains(&"readonly"));

		let y = find(&decoded, 1, 10);
		assert_eq!(y.type_name, "variable");
		assert!(y.modifiers.contains(&"declaration"));
		assert!(
			!y.modifiers.contains(&"readonly"),
			"a `let mut` binding must not carry `readonly`"
		);
	}

	#[test]
	fn a_multi_line_block_comment_emits_one_token_per_covered_line() {
		let text = "/* line one\nline two\nline three */\nfunc main(): void = {}";
		let decoded = tokens_for(text);
		let comments: Vec<&Decoded> = decoded
			.iter()
			.filter(|d| d.type_name == "comment")
			.collect();
		assert_eq!(comments.len(), 3, "got {decoded:?}");
		assert_eq!(comments[0].line, 0);
		assert_eq!(comments[1].line, 1);
		assert_eq!(comments[2].line, 2);
	}

	#[test]
	fn returns_none_for_an_unopened_document() {
		let uri: Uri = "file:///semtok_missing.nym".parse().unwrap();
		let docs = DocumentStore::default();
		assert!(semantic_tokens_full(&docs, &params(&uri)).is_none());
	}

	#[test]
	fn survives_a_syntactically_broken_buffer() {
		let uri: Uri = "file:///semtok_broken.nym".parse().unwrap();
		let text = "func add(a: int, b: int): int = a + b\nstruct Broken {";
		let docs = docs_with(&uri, text);
		let result = semantic_tokens_full(&docs, &params(&uri));
		assert!(
			result.is_some(),
			"expected tokens even over a broken buffer"
		);
	}

	#[test]
	fn variant_and_param_and_label_uses_classify_consistently_with_their_declarations() {
		// A Result-like enum: a variant name should colour `enumMember`
		// everywhere (declaration, match/for pattern, construction,
		// qualified/bare reference), a parameter should colour `parameter`
		// at every use in its body, and a call-argument/struct-field label
		// should colour `property`, distinct from an ordinary variable.
		let text = "enum Shape { Circle(radius: int), Square }\n\
			func f(r: Shape, n: int): Shape = match (r) { Circle(radius = v) -> Circle(radius = n), Square -> Square }\n";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		// Enum declaration: variant names are `enumMember`.
		assert_eq!(find(&decoded, 0, 13).type_name, "enumMember"); // Circle
		assert_eq!(find(&decoded, 0, 34).type_name, "enumMember"); // Square

		// The `r` parameter, used as the match scrutinee.
		assert_eq!(find(&decoded, 1, 41).type_name, "parameter");

		// `Circle` in the match-arm PATTERN.
		assert_eq!(find(&decoded, 1, 46).type_name, "enumMember");
		// `radius` — the pattern's field LABEL.
		assert_eq!(find(&decoded, 1, 53).type_name, "property");
		// `v` — the pattern's own binder use (a plain local, not a variant).
		assert_eq!(find(&decoded, 1, 62).type_name, "variable");

		// The arrow stays `operator`.
		assert_eq!(find(&decoded, 1, 65).type_name, "operator");

		// `Circle` in the CONSTRUCTION.
		assert_eq!(find(&decoded, 1, 68).type_name, "enumMember");
		// `radius` — the construction's field LABEL.
		assert_eq!(find(&decoded, 1, 75).type_name, "property");
		// `n` — the `n: int` parameter, used inside the construction.
		assert_eq!(find(&decoded, 1, 84).type_name, "parameter");

		// `Square` in the second arm's PATTERN (a bare nullary variant).
		assert_eq!(find(&decoded, 1, 88).type_name, "enumMember");
		// `Square` as the arm's bare REFERENCE value.
		assert_eq!(find(&decoded, 1, 98).type_name, "enumMember");
	}

	#[test]
	fn a_let_binder_use_stays_a_plain_variable() {
		let text = "func main(): int = { let x = 1\n  x }";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		// The body's `x` is a use of the `let` binder, not a parameter or
		// enum variant — it must stay `variable`, matching its declaration.
		let x_use = find(&decoded, 1, 2);
		assert_eq!(x_use.type_name, "variable");
		assert!(
			x_use.modifiers.is_empty(),
			"a use site carries no declaration/readonly modifiers: {x_use:?}"
		);
	}

	#[test]
	fn a_plain_call_argument_label_classifies_as_property() {
		let text = "func f(hello: int): int = hello\nfunc main(): int = f(hello = 1)";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		// `hello` the label in `f(hello = 1)`, distinct from an ordinary
		// variable/parameter use.
		let label = find(&decoded, 1, 21);
		assert_eq!(label.type_name, "property");

		// The call target `f` itself still resolves to `function`.
		let callee = find(&decoded, 1, 19);
		assert_eq!(callee.type_name, "function");
	}

	/// The `(line, col)` of byte `offset` in `text` — ASCII-only test inputs,
	/// so byte offset and UTF-16 column coincide.
	fn line_col(text: &str, offset: usize) -> (u32, u32) {
		let prefix = &text[..offset];
		let line = prefix.matches('\n').count() as u32;
		let col = match prefix.rfind('\n') {
			Some(nl) => (offset - nl - 1) as u32,
			None => offset as u32,
		};
		(line, col)
	}

	#[test]
	fn a_this_method_calls_name_classifies_as_method_matching_its_decl() {
		// BUG 2b: `get` in `this.get()` must classify the same as its own
		// `func get` declaration (`method`), not fall through to the
		// `variable` default `push_token` applies to an unclassified
		// identifier token.
		let text =
			"struct Point(x: int) {\n  func get(): int = this.x\n  func run(): int = this.get()\n}";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		let decl_offset = text.find("func get").unwrap() + "func ".len();
		let (decl_line, decl_col) = line_col(text, decl_offset);
		assert_eq!(find(&decoded, decl_line, decl_col).type_name, "method");

		let use_offset = text.rfind("this.get()").unwrap() + "this.".len();
		let (use_line, use_col) = line_col(text, use_offset);
		assert_eq!(find(&decoded, use_line, use_col).type_name, "method");
	}

	#[test]
	fn a_this_field_accesss_name_stays_the_variable_default() {
		// `this.x` names a FIELD, not a method — `definition_at` cannot
		// resolve it to a method decl, so it must NOT classify as `method`.
		let text = "struct Point(x: int) {\n  func get(): int = this.x\n}";
		let decoded = tokens_for(text);
		assert_sorted_and_non_overlapping(&decoded);

		let use_offset = text.rfind("this.x").unwrap() + "this.".len();
		let (use_line, use_col) = line_col(text, use_offset);
		assert_eq!(find(&decoded, use_line, use_col).type_name, "variable");
	}

	#[test]
	fn legend_indices_match_the_type_and_modifier_name_tables() {
		let legend = legend();
		assert_eq!(legend.token_types.len(), TYPE_NAMES.len());
		assert_eq!(legend.token_modifiers.len(), MODIFIER_NAMES.len());
	}
}
