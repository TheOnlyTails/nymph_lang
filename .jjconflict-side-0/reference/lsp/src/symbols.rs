use crate::analyzer::SymbolKind;
use serde::{Deserialize, Serialize};

/// Information about a symbol in the document
#[derive(Debug, Clone)]
pub struct SymbolInfo {
	pub name: String,
	pub kind: SymbolKind,
	pub line: usize,
	pub column: usize,
	pub type_info: String,
}

/// LSP-compatible symbol information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInformation {
	pub name: String,
	pub kind: i32,
	pub location: Location,
	pub container_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
	pub uri: String,
	pub range: Range,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
	pub start: Position,
	pub end: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
	pub line: usize,
	pub character: usize,
}

/// LSP symbol kinds
pub mod lsp_symbol_kinds {
	pub const FILE: i32 = 1;
	pub const MODULE: i32 = 2;
	pub const NAMESPACE: i32 = 3;
	pub const PACKAGE: i32 = 4;
	pub const CLASS: i32 = 5;
	pub const METHOD: i32 = 6;
	pub const PROPERTY: i32 = 7;
	pub const FIELD: i32 = 8;
	pub const CONSTRUCTOR: i32 = 9;
	pub const ENUM: i32 = 10;
	pub const INTERFACE: i32 = 11;
	pub const FUNCTION: i32 = 12;
	pub const VARIABLE: i32 = 13;
	pub const CONSTANT: i32 = 14;
	pub const STRING: i32 = 15;
	pub const NUMBER: i32 = 16;
	pub const BOOLEAN: i32 = 17;
	pub const ARRAY: i32 = 18;
	pub const OBJECT: i32 = 19;
	pub const KEY: i32 = 20;
	pub const NULL: i32 = 21;
	pub const ENUM_MEMBER: i32 = 22;
	pub const STRUCT: i32 = 23;
	pub const EVENT: i32 = 24;
	pub const OPERATOR: i32 = 25;
	pub const TYPE_PARAMETER: i32 = 26;
}

#[must_use]
pub fn symbol_kind_to_lsp(kind: SymbolKind) -> i32 {
	match kind {
		SymbolKind::Function => lsp_symbol_kinds::FUNCTION,
		SymbolKind::Variable | SymbolKind::Parameter => lsp_symbol_kinds::VARIABLE,
		SymbolKind::Type | SymbolKind::Struct => lsp_symbol_kinds::STRUCT,
		SymbolKind::Interface => lsp_symbol_kinds::INTERFACE,
		SymbolKind::Field => lsp_symbol_kinds::FIELD,
		SymbolKind::Enum => lsp_symbol_kinds::ENUM,
		SymbolKind::Namespace | SymbolKind::Module => lsp_symbol_kinds::NAMESPACE,
	}
}

#[must_use]
pub fn symbol_kind_to_lsp_enum(kind: SymbolKind) -> tower_lsp::lsp_types::SymbolKind {
	use tower_lsp::lsp_types::SymbolKind as LspSymbolKind;
	match kind {
		SymbolKind::Function => LspSymbolKind::FUNCTION,
		SymbolKind::Variable | SymbolKind::Parameter => LspSymbolKind::VARIABLE,
		SymbolKind::Type | SymbolKind::Struct => LspSymbolKind::STRUCT,
		SymbolKind::Interface => LspSymbolKind::INTERFACE,
		SymbolKind::Field => LspSymbolKind::FIELD,
		SymbolKind::Enum => LspSymbolKind::ENUM,
		SymbolKind::Namespace | SymbolKind::Module => LspSymbolKind::NAMESPACE,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_symbol_kind_to_lsp() {
		assert_eq!(
			symbol_kind_to_lsp(SymbolKind::Function),
			lsp_symbol_kinds::FUNCTION
		);
		assert_eq!(
			symbol_kind_to_lsp(SymbolKind::Type),
			lsp_symbol_kinds::STRUCT
		);
		assert_eq!(
			symbol_kind_to_lsp(SymbolKind::Interface),
			lsp_symbol_kinds::INTERFACE
		);
	}
}
