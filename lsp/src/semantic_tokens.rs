/// Semantic token type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenType {
	Keyword,
	Type,
	Function,
	Variable,
	Parameter,
	Number,
	String,
	Comment,
	Operator,
	Interface,
	Member,
}

impl TokenType {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Keyword => "keyword",
			Self::Type => "type",
			Self::Function => "function",
			Self::Variable => "variable",
			Self::Parameter => "parameter",
			Self::Number => "number",
			Self::String => "string",
			Self::Comment => "comment",
			Self::Operator => "operator",
			Self::Interface => "interface",
			Self::Member => "member",
		}
	}
}

/// Token modifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenModifier {
	Declaration,
	Definition,
	Builtin,
	Mutable,
}

impl TokenModifier {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Declaration => "declaration",
			Self::Definition => "definition",
			Self::Builtin => "builtin",
			Self::Mutable => "mutable",
		}
	}
}

/// A semantic token with position and type information
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticToken {
	pub line: usize,
	pub start_char: usize,
	pub length: usize,
	pub token_type: TokenType,
	pub modifiers: Vec<TokenModifier>,
}

pub struct SemanticTokenizer {
	tokens: Vec<SemanticToken>,
}

impl SemanticTokenizer {
	pub fn new() -> Self {
		Self { tokens: Vec::new() }
	}

	/// Tokenize content and return semantic tokens
	pub fn tokenize(&mut self, content: &str) -> Vec<SemanticToken> {
		self.tokens.clear();

		// Basic keyword highlighting
		let keywords = [
			"let",
			"func",
			"if",
			"else",
			"struct",
			"interface",
			"return",
			"true",
			"false",
		];

		for (line_idx, line) in content.lines().enumerate() {
			for keyword in &keywords {
				let mut search_start = 0;
				while let Some(pos) = line[search_start..].find(keyword) {
					let absolute_pos = search_start + pos;

					// Verify it's a complete word
					let before_ok = absolute_pos == 0 || {
						let before_char = line.chars().nth(absolute_pos.saturating_sub(1));
						before_char.is_none() || !is_identifier_char(before_char.unwrap())
					};

					let after_ok = absolute_pos + keyword.len() >= line.len() || {
						let after_char = line.chars().nth(absolute_pos + keyword.len());
						after_char.is_none() || !is_identifier_char(after_char.unwrap())
					};

					if before_ok && after_ok {
						self.add_token(
							line_idx + 1,
							absolute_pos + 1,
							keyword.len(),
							TokenType::Keyword,
							vec![],
						);
					}

					search_start = absolute_pos + 1;
				}
			}
		}

		// Sort by position
		self.tokens.sort_by(|a, b| {
			if a.line != b.line {
				a.line.cmp(&b.line)
			} else {
				a.start_char.cmp(&b.start_char)
			}
		});

		self.tokens.clone()
	}

	/// Add a semantic token
	fn add_token(
		&mut self,
		line: usize,
		col: usize,
		length: usize,
		token_type: TokenType,
		modifiers: Vec<TokenModifier>,
	) {
		self.tokens.push(SemanticToken {
			line,
			start_char: col,
			length,
			token_type,
			modifiers,
		});
	}
}

fn is_identifier_char(ch: char) -> bool {
	ch.is_alphanumeric() || ch == '_'
}

impl Default for SemanticTokenizer {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_token_type_as_str() {
		assert_eq!(TokenType::Keyword.as_str(), "keyword");
		assert_eq!(TokenType::Function.as_str(), "function");
		assert_eq!(TokenType::Variable.as_str(), "variable");
	}

	#[test]
	fn test_token_modifier_as_str() {
		assert_eq!(TokenModifier::Declaration.as_str(), "declaration");
		assert_eq!(TokenModifier::Mutable.as_str(), "mutable");
	}
}
