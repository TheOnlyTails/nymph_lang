//! Shared source-position policy for semantic LSP requests.

use nymph_ast::token::Token;

/// Query an exact byte offset, then optionally retry inside the semantic token
/// immediately to its left when the intervening source is whitespace only.
///
/// The fallback is computed at Unicode scalar boundaries and is never offered
/// through comments, punctuation, operators, delimiters, or another token.
/// Core semantic queries remain strictly half-open; this is the sole LSP-only
/// cursor convenience shared by hover, definition, and completion.
pub(crate) fn query_with_whitespace_left_bias<T>(
	text: &str,
	offset: usize,
	mut query: impl FnMut(usize) -> Option<T>,
) -> Option<T> {
	let offset = offset.min(text.len());
	if let Some(result) = query(offset) {
		return Some(result);
	}
	if offset < text.len() && !text[offset..].starts_with(char::is_whitespace) {
		return None;
	}

	let whitespace_start = text[..offset]
		.char_indices()
		.rev()
		.find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index + ch.len_utf8()))?;
	if whitespace_start == offset
		|| !text[whitespace_start..offset]
			.chars()
			.all(char::is_whitespace)
	{
		return None;
	}

	let token = nymph_syntax::lex(text)
		.tokens
		.into_iter()
		.find(|token| token.1.end == whitespace_start && can_left_bias_from(&token.0))?;
	let candidate = text[..token.1.end].char_indices().next_back()?.0;
	query(candidate)
}

fn can_left_bias_from(token: &Token) -> bool {
	use Token::*;
	matches!(
		token,
		Int(_)
			| UInt(_)
			| Float(_)
			| Char(_)
			| Str(_)
			| True
			| False
			| Identifier(_)
			| AnonymousParam(_)
			| Public
			| Internal
			| Private
			| Import
			| With
			| Type
			| Struct
			| Enum
			| Let
			| External
			| Func
			| Interface
			| Impl
			| Namespace
			| For
			| If
			| Else
			| Match
			| Continue
			| Break
			| Return
			| This
			| Async
			| Await
			| IntType
			| UIntType
			| FloatType
			| BooleanType
			| CharType
			| StringType
			| VoidType
			| NeverType
			| SelfType
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn left_bias_is_limited_to_whitespace_after_a_semantic_token() {
		let cases = [
			("one space", "name ", 5, Some(3)),
			("spaces and newline", "name  \n  ", 9, Some(3)),
			("utf-8 token", "café ", 6, Some(3)),
			("dot", "name. ", 6, None),
			("comma", "name, ", 6, None),
			("colon", "name: ", 6, None),
			("semicolon", "name; ", 6, None),
			("operator", "name + ", 7, None),
			("paren", "name) ", 6, None),
			("bracket", "name] ", 6, None),
			("brace", "name} ", 6, None),
			("line comment", "name // note ", 13, None),
			("block comment", "name /* note */ ", 16, None),
			("adjacent token", "name other", 10, None),
		];

		for (name, text, offset, expected) in cases {
			let actual = query_with_whitespace_left_bias(text, offset, |candidate| {
				(candidate < offset).then_some(candidate)
			});
			assert_eq!(actual, expected, "case {name}");
		}
	}
}
