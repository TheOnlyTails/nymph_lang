use std::{collections::BTreeMap, path::PathBuf};

use ecow::EcoString;

use crate::ast::Span;
use crate::ast::declaration::ImportRoot;
use crate::types::error::TypeError;
use crate::types::{GenericParamInfo, StructMember, Type};

#[salsa::input]
pub struct SourceFile {
	#[returns(ref)]
	pub path: String,
	#[returns(ref)]
	pub text: String,
}

#[salsa::input]
pub struct ProjectConfig {
	#[returns(ref)]
	pub root: PathBuf,
	#[returns(ref)]
	pub output_dir: PathBuf,
	pub implicit_prelude: bool,
}

#[salsa::interned]
pub struct Name<'db> {
	#[returns(ref)]
	pub text: String,
}

#[salsa::interned]
pub struct ModulePath<'db> {
	#[returns(ref)]
	pub segments: Vec<String>,
}

#[salsa::interned]
pub struct DefId<'db> {
	pub file: SourceFile,
	pub index: u32,
}

/// A stable, lifetime-free key for type definitions.
/// Used inside `Type` to link nominal types back to their definition site
/// without requiring a salsa `'db` lifetime on the `Type` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DefKey {
	pub file_path_hash: u64,
	pub index: u32,
}

impl DefKey {
	pub fn new(file_path: &str, index: u32) -> Self {
		use std::hash::{Hash, Hasher};
		let mut hasher = std::collections::hash_map::DefaultHasher::new();
		file_path.hash(&mut hasher);
		Self {
			file_path_hash: hasher.finish(),
			index,
		}
	}

	pub fn from_source_file(db: &dyn Db, file: SourceFile, index: u32) -> Self {
		Self::new(file.path(db), index)
	}
}

/// Shape data for a struct type definition (fields, members, impls, generics).
/// Computed lazily via salsa queries; not stored inline in `Type`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub struct StructShape {
	pub generics: Vec<GenericParamInfo>,
	pub fields: BTreeMap<EcoString, Type>,
	pub members: BTreeMap<EcoString, StructMember>,
	pub impls: BTreeMap<EcoString, Type>,
}

/// Shape data for an enum type definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub struct EnumShape {
	pub generics: Vec<GenericParamInfo>,
	pub variants: BTreeMap<EcoString, BTreeMap<EcoString, Type>>,
	pub members: BTreeMap<EcoString, StructMember>,
	pub impls: BTreeMap<EcoString, Type>,
}

/// Shape data for an interface type definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub struct InterfaceShape {
	pub generics: Vec<GenericParamInfo>,
	pub members: BTreeMap<EcoString, StructMember>,
	pub impls: BTreeMap<EcoString, Type>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub struct ImportSpec {
	pub root: ImportRoot,
	pub path: Vec<String>,
	pub idents: Option<Vec<ImportedIdent>>,
	pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::Update)]
pub struct ImportedIdent {
	pub name: String,
	pub alias: Option<String>,
	pub span: Span,
}

#[salsa::accumulator]
pub struct Diagnostics(pub Diagnostic);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
	pub file_path: EcoString,
	pub span: Span,
	pub message: String,
	pub kind: DiagnosticKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
	ParseError,
	TypeError,
}

#[salsa::accumulator]
pub struct TypeErrors(pub TypeError);

#[salsa::db]
pub trait Db: salsa::Database {}

#[salsa::db]
#[derive(Default, Clone)]
pub struct NymphDatabase {
	storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for NymphDatabase {}

#[salsa::db]
impl Db for NymphDatabase {}
