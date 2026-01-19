use std::path::PathBuf;

use ariadne::{Config, Source};
use ecow::EcoString;
use nymph_compiler::ast::{Spanned, declaration::Module};
use nymph_compiler::parse;
use nymph_compiler::types::error::TypeError;
use nymph_compiler::types::{Context, TypeChecker};

/// Represents a document being edited in the language server
#[derive(Debug, Clone)]
pub struct Document {
	/// The URI of the document
	pub uri: String,
	/// The current content of the document
	pub content: String,
	/// The parsed AST
	pub ast: Option<Spanned<Module>>,
	/// Parse errors if any
	pub parse_errors: Vec<String>,
	/// The typed context from type checking
	pub type_context: Option<Context>,
	/// Type errors if any
	pub type_errors: Vec<TypeError>,
	/// The type checker (for import resolution)
	pub type_checker: Option<TypeChecker>,
}

impl Document {
	/// Create a new document
	pub fn new(uri: String, content: String) -> Self {
		let mut doc = Self {
			uri,
			content,
			ast: None,
			parse_errors: Vec::new(),
			type_context: None,
			type_errors: Vec::new(),
			type_checker: None,
		};
		let _ = doc.parse();
		doc
	}

	/// Update the document content and reparse
	pub fn update(&mut self, content: String) {
		self.content = content;
		self.parse_errors.clear();
		self.type_context = None;
		self.type_errors.clear();
		self.type_checker = None;
		let _ = self.parse();
	}

	/// Parse the current content and run type checking
	pub fn parse(&mut self) -> std::result::Result<(), String> {
		let filename = EcoString::from(self.uri.clone());
		let (ast, errors) = parse(filename.clone(), &self.content);

		if !errors.is_empty() {
			self.parse_errors = errors
				.into_iter()
				.map(|e| {
					let mut buf = Vec::new();
					let report = e
						.with_config(
							Config::new()
								.with_color(false)
								.with_compact(true)
								.with_tab_width(2),
						)
						.finish();

					let path = self.uri_to_path().unwrap();
					let path = path.to_str().unwrap();

					report
						.write_for_stdout((path.into(), Source::from(&self.content)), &mut buf)
						.unwrap();
					String::from_utf8(buf).unwrap()
				})
				.collect();
			self.ast = None;
			self.type_context = None;
			self.type_errors.clear();
			Err("Parse errors".to_string())
		} else if let Some(spanned_module) = ast {
			self.ast = Some(spanned_module.clone());
			self.parse_errors.clear();

			// Try to extract file path from URI for import resolution
			let file_path = self.uri_to_path();
			let mut type_checker = if let Some(path) = file_path {
				TypeChecker::new(Some(path))
			} else {
				TypeChecker::default()
			};
			let base_ctx = Context::default();
			match type_checker.check_module(spanned_module.inner(), &base_ctx) {
				Ok(ctx) => {
					self.type_context = Some(ctx);
					self.type_errors.clear();
				}
				Err(err) => {
					self.type_context = Some(base_ctx);
					self.type_errors = vec![err];
				}
			}
			self.type_checker = Some(type_checker);

			Ok(())
		} else {
			self.ast = None;
			self.type_context = None;
			self.type_errors.clear();
			self.parse_errors.push("No AST generated".to_string());
			Err("No AST generated".to_string())
		}
	}

	/// Convert the document URI to a file path
	fn uri_to_path(&self) -> Option<PathBuf> {
		// Try using the url crate first
		if let Ok(url) = url::Url::parse(&self.uri)
			&& let Ok(path) = url.to_file_path()
		{
			return Some(path);
		}

		// Fallback: manually strip file:// prefix
		// On Unix, URIs look like file:///path/to/file (3 slashes)
		// On Windows, URIs look like file:///C:/path/to/file
		if let Some(path_str) = self.uri.strip_prefix("file://") {
			return Some(PathBuf::from(path_str));
		}

		None
	}

	/// Get the line and column from a position
	pub fn position_to_line_col(&self, position: usize) -> (usize, usize) {
		let mut line = 1;
		let mut col = 1;
		for (i, ch) in self.content.chars().enumerate() {
			if i >= position {
				break;
			}
			if ch == '\n' {
				line += 1;
				col = 1;
			} else {
				col += 1;
			}
		}
		(line, col)
	}

	/// Convert a position to byte offset
	pub fn position_to_offset(&self, line: usize, character: usize) -> Option<usize> {
		let mut current_line = 1;
		let mut current_char = 0;
		for (i, ch) in self.content.chars().enumerate() {
			if current_line == line && current_char == character {
				return Some(i);
			}
			if ch == '\n' {
				current_line += 1;
				current_char = 0;
			} else {
				current_char += 1;
			}
		}
		if current_line == line && current_char == character {
			Some(self.content.len())
		} else {
			None
		}
	}

	/// Get a range of text
	pub fn get_range(
		&self,
		start_line: usize,
		start_col: usize,
		end_line: usize,
		end_col: usize,
	) -> Option<String> {
		let start_offset = self.position_to_offset(start_line, start_col)?;
		let end_offset = self.position_to_offset(end_line, end_col)?;
		if start_offset <= end_offset {
			Some(self.content[start_offset..end_offset].to_string())
		} else {
			None
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_document_creation() {
		let doc = Document::new("file:///test.nymph".to_string(), "let x = 5".to_string());
		assert_eq!(doc.uri, "file:///test.nymph");
		assert_eq!(doc.content, "let x = 5");
	}

	#[test]
	fn test_position_to_line_col() {
		let doc = Document::new(
			"file:///test.nymph".to_string(),
			"let x = 5\nlet y = 10".to_string(),
		);
		let (line, col) = doc.position_to_line_col(0);
		assert_eq!(line, 1);
		assert_eq!(col, 1);

		let (line, col) = doc.position_to_line_col(10);
		assert_eq!(line, 2);
		assert_eq!(col, 1);
	}
}
