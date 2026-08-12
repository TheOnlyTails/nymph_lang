use std::path::{Path, PathBuf};

use nymph_compiler::{
	ast::{Span, Spanned, declaration::Module},
	config::load_compiler_project_config,
	db::{DiagnosticKind, Diagnostics, NymphDatabase, ProjectConfig, SourceFile},
	queries::{parse_file, typecheck_file},
	types::{Context, TypeChecker, error::TypeError},
};
use tower_lsp::lsp_types::{Position, Range};

const DEFAULT_OUTPUT_DIR: &str = "dist";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerDiagnostic {
	pub file_path: String,
	pub span: Span,
	pub message: String,
	pub source: String,
}

#[derive(Debug, Clone)]
struct LineIndex {
	line_starts: Vec<usize>,
}

impl LineIndex {
	fn new(content: &str) -> Self {
		let mut line_starts = vec![0];
		for (offset, ch) in content.char_indices() {
			if ch == '\n' {
				line_starts.push(offset + ch.len_utf8());
			}
		}
		Self { line_starts }
	}

	fn offset_to_position(&self, content: &str, offset: usize) -> Option<Position> {
		if offset > content.len() || !content.is_char_boundary(offset) {
			return None;
		}

		let line_index = match self.line_starts.binary_search(&offset) {
			Ok(index) => index,
			Err(index) => index.checked_sub(1)?,
		};
		let line_start = *self.line_starts.get(line_index)?;
		let line_slice = &content[line_start..offset];
		let utf16_col = line_slice.encode_utf16().count() as u32;

		Some(Position {
			line: line_index as u32,
			character: utf16_col,
		})
	}

	fn position_to_offset(&self, content: &str, position: Position) -> Option<usize> {
		let line_start = *self.line_starts.get(position.line as usize)?;
		let line_end = self
			.line_starts
			.get(position.line as usize + 1)
			.copied()
			.unwrap_or(content.len());
		let line = &content[line_start..line_end];

		let mut utf16_count = 0u32;
		for (byte_offset, ch) in line.char_indices() {
			if utf16_count == position.character {
				return Some(line_start + byte_offset);
			}
			utf16_count += ch.len_utf16() as u32;
			if utf16_count > position.character {
				return None;
			}
		}

		(utf16_count == position.character).then_some(line_end)
	}
}

/// Represents a document being edited in the language server.
#[derive(Debug, Clone)]
pub struct Document {
	pub uri: String,
	pub content: String,
	pub ast: Option<Spanned<Module>>,
	pub diagnostics: Vec<CompilerDiagnostic>,
	pub type_context: Option<Context>,
	pub type_errors: Vec<TypeError>,
	pub type_checker: Option<TypeChecker>,
	line_index: LineIndex,
}

impl Document {
	#[must_use]
	pub fn new(uri: String, content: String) -> Self {
		let mut doc = Self {
			uri,
			line_index: LineIndex::new(&content),
			content,
			ast: None,
			diagnostics: Vec::new(),
			type_context: None,
			type_errors: Vec::new(),
			type_checker: None,
		};
		let _ = doc.parse();
		doc
	}

	pub fn update(&mut self, content: String) {
		self.content = content;
		self.line_index = LineIndex::new(&self.content);
		self.ast = None;
		self.diagnostics.clear();
		self.type_context = None;
		self.type_errors.clear();
		self.type_checker = None;
		let _ = self.parse();
	}

	pub fn parse(&mut self) -> Result<(), String> {
		let file_path = self.document_path();
		let file = SourceFile::new(
			&NymphDatabase::default(),
			file_path.to_string_lossy().to_string(),
			self.content.clone(),
		);
		let _ = file;

		let (ast, diagnostics) = self.run_compiler_queries(&file_path);
		self.ast = ast;
		self.diagnostics = diagnostics;

		if self.ast.is_none() {
			self.type_context = None;
			self.type_errors.clear();
			self.type_checker = None;
			return Err("Parse errors".to_string());
		}

		let mut type_checker = TypeChecker::new(Some(file_path));
		let base_ctx = Context::default();
		match type_checker.check_module(&self.ast.as_ref().expect("checked above").0, &base_ctx) {
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
	}

	fn run_compiler_queries(
		&self,
		file_path: &Path,
	) -> (Option<Spanned<Module>>, Vec<CompilerDiagnostic>) {
		let db = NymphDatabase::default();
		let file_path_str = file_path.to_string_lossy().to_string();
		let file = SourceFile::new(&db, file_path_str.clone(), self.content.clone());
		let parse_result = parse_file(&db, file);
		let mut diagnostics = parse_file::accumulated::<Diagnostics>(&db, file)
			.into_iter()
			.filter(|diag| diag.0.file_path.as_ref() == file_path_str.as_str())
			.map(|diag| CompilerDiagnostic {
				file_path: diag.0.file_path.to_string(),
				span: diag.0.span,
				message: diag.0.message.clone(),
				source: diagnostic_source(diag.0.kind),
			})
			.collect::<Vec<_>>();

		if parse_result.errors.is_empty() {
			let config = self.project_config(&db, file_path);
			let _ = typecheck_file(&db, file, config);
			diagnostics.extend(
				typecheck_file::accumulated::<Diagnostics>(&db, file, config)
					.into_iter()
					.filter(|diag| diag.0.file_path.as_ref() == file_path_str.as_str())
					.map(|diag| CompilerDiagnostic {
						file_path: diag.0.file_path.to_string(),
						span: diag.0.span,
						message: diag.0.message.clone(),
						source: diagnostic_source(diag.0.kind),
					}),
			);
		}

		(parse_result.module, diagnostics)
	}

	fn project_config(&self, db: &NymphDatabase, file_path: &Path) -> ProjectConfig {
		let project_root = TypeChecker::find_project_root(file_path).unwrap_or_else(|| {
			file_path
				.parent()
				.map(Path::to_path_buf)
				.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
		});

		load_compiler_project_config(db, project_root.clone(), PathBuf::from(DEFAULT_OUTPUT_DIR))
			.unwrap_or_else(|_| {
				ProjectConfig::new(db, project_root, PathBuf::from(DEFAULT_OUTPUT_DIR), true)
			})
	}

	#[must_use]
	pub fn document_path(&self) -> PathBuf {
		self
			.uri_to_path()
			.unwrap_or_else(|| PathBuf::from(self.uri.clone()))
	}

	pub fn uri_to_path(&self) -> Option<PathBuf> {
		if let Ok(url) = url::Url::parse(&self.uri)
			&& let Ok(path) = url.to_file_path()
		{
			return Some(path);
		}

		self.uri.strip_prefix("file://").map(PathBuf::from)
	}

	pub fn position_to_line_col(&self, position: usize) -> (usize, usize) {
		let mut line = 1;
		let mut col = 1;
		for (offset, ch) in self.content.char_indices() {
			if offset >= position {
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

	pub fn position_to_offset(&self, line: usize, character: usize) -> Option<usize> {
		let mut current_line = 1usize;
		let mut current_char = 0usize;
		for (offset, ch) in self.content.char_indices() {
			if current_line == line && current_char == character {
				return Some(offset);
			}
			if ch == '\n' {
				current_line += 1;
				current_char = 0;
			} else {
				current_char += 1;
			}
		}
		(current_line == line && current_char == character).then_some(self.content.len())
	}

	pub fn offset_to_lsp_position(&self, offset: usize) -> Option<Position> {
		self.line_index.offset_to_position(&self.content, offset)
	}

	pub fn span_to_lsp_range(&self, span: Span) -> Option<Range> {
		Some(Range {
			start: self.offset_to_lsp_position(span.start)?,
			end: self.offset_to_lsp_position(span.end)?,
		})
	}

	pub fn lsp_position_to_offset(&self, line: u32, character: u32) -> Option<usize> {
		self
			.line_index
			.position_to_offset(&self.content, Position { line, character })
	}

	pub fn apply_lsp_change(&mut self, range: &Range, text: &str) -> Result<(), String> {
		let start = self
			.lsp_position_to_offset(range.start.line, range.start.character)
			.ok_or_else(|| "invalid LSP change start position".to_string())?;
		let end = self
			.lsp_position_to_offset(range.end.line, range.end.character)
			.ok_or_else(|| "invalid LSP change end position".to_string())?;

		if start > end || !self.content.is_char_boundary(start) || !self.content.is_char_boundary(end) {
			return Err("invalid LSP edit byte range".to_string());
		}

		self.content.replace_range(start..end, text);
		self.line_index = LineIndex::new(&self.content);
		self.parse()
	}

	pub fn get_range(
		&self,
		start_line: usize,
		start_col: usize,
		end_line: usize,
		end_col: usize,
	) -> Option<String> {
		let start_offset = self.position_to_offset(start_line, start_col)?;
		let end_offset = self.position_to_offset(end_line, end_col)?;
		(start_offset <= end_offset).then(|| self.content[start_offset..end_offset].to_string())
	}

	pub fn load_from_path(path: &Path) -> std::io::Result<Self> {
		let content = std::fs::read_to_string(path)?;
		let uri = url::Url::from_file_path(path)
			.map_err(|()| std::io::Error::other("invalid file path for URI"))?
			.to_string();
		Ok(Self::new(uri, content))
	}
}

fn diagnostic_source(kind: DiagnosticKind) -> String {
	match kind {
		DiagnosticKind::ParseError => "nymph-parse".to_string(),
		DiagnosticKind::TypeError => "nymph-typecheck".to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_document_creation() {
		let doc = Document::new("file:///test.nym".to_string(), "let x = 5".to_string());
		assert_eq!(doc.uri, "file:///test.nym");
		assert_eq!(doc.content, "let x = 5");
	}

	#[test]
	fn test_position_to_line_col() {
		let doc = Document::new(
			"file:///test.nym".to_string(),
			"let x = 5\nlet y = 10".to_string(),
		);
		let (line, col) = doc.position_to_line_col(0);
		assert_eq!(line, 1);
		assert_eq!(col, 1);

		let (line, col) = doc.position_to_line_col(10);
		assert_eq!(line, 2);
		assert_eq!(col, 1);
	}

	#[test]
	fn test_lsp_position_conversion_uses_utf16_columns() {
		let doc = Document::new(
			"file:///test.nym".to_string(),
			"let 😀 = 1\nlet z = 2".to_string(),
		);

		let second_line_offset = doc
			.lsp_position_to_offset(1, 0)
			.expect("expected start of second line");
		assert_eq!(
			second_line_offset,
			doc.content.find("let z").expect("expected second line")
		);
	}

	#[test]
	fn test_apply_lsp_change_replaces_full_range() {
		let mut doc = Document::new(
			"file:///test.nym".to_string(),
			"let x = 5\nlet y = 10".to_string(),
		);

		let range = Range {
			start: Position {
				line: 0,
				character: 4,
			},
			end: Position {
				line: 1,
				character: 5,
			},
		};

		doc
			.apply_lsp_change(&range, "value")
			.expect("expected LSP edit to apply");

		assert_eq!(doc.content, "let value = 10");
	}

	#[test]
	fn test_parse_errors_are_structured_diagnostics() {
		let doc = Document::new("file:///test.nym".to_string(), "let =".to_string());

		assert!(!doc.diagnostics.is_empty(), "expected compiler diagnostics");
		assert_eq!(doc.diagnostics[0].source, "nymph-parse");
	}
}
