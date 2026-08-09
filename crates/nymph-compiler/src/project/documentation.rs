//! Deterministic, filesystem-independent project documentation extraction and rendering.

use std::{
	collections::{BTreeMap, HashMap},
	sync::Arc,
};

use nymph_ast::decl::Visibility;
use nymph_sema::{
	DefinitionId, DefinitionShapeKind, ExportedDefinition, ExportedImpl, GenericConstraint,
	GenericParameter, InterfaceType, MemberKind, MemberShape, ParameterShape,
};

use super::{CompilerSession, ModulePath, ProjectDiagnostic, ProjectId};

/// Documentation extraction options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocOptions {
	/// Include private top-level declarations. Public and internal declarations are always included.
	pub document_private_items: bool,
}

/// A semantic signature fragment. Named types retain their exact semantic identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocFragment {
	Text(String),
	Definition { label: String, target: DefinitionId },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocSignature(pub Vec<DocFragment>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocItem {
	pub definition: DefinitionId,
	pub name: String,
	pub kind: DefinitionShapeKind,
	pub private: bool,
	pub anchor: String,
	pub signature: DocSignature,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocImplementation {
	pub definition: DefinitionId,
	pub private: bool,
	pub anchor: String,
	pub signature: DocSignature,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocModule {
	pub path: ModulePath,
	pub url: String,
	pub items: Vec<DocItem>,
	pub implementations: Vec<DocImplementation>,
}

/// Checked documentation model for the project-module closure rooted at `entry`.
#[derive(Clone, Debug, PartialEq)]
pub struct DocProject {
	pub entry: ModulePath,
	pub modules: BTreeMap<ModulePath, DocModule>,
	/// Non-error diagnostics retained from the successful project check.
	pub diagnostics: Vec<ProjectDiagnostic>,
}

/// Relative output paths and complete file contents; publication belongs to the caller.
pub type StaticDocSite = BTreeMap<String, String>;

/// Load with the compiler's embedded standard library and extract project documentation.
pub fn document_project(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	options: DocOptions,
) -> Result<DocProject, Vec<ProjectDiagnostic>> {
	document_project_with_std(entry, load, &crate::embedded_std_provider, options)
}

/// Load through the canonical compiler project graph using a caller-provided std source provider.
pub fn document_project_with_std(
	entry: &str,
	load: &dyn Fn(&str) -> Option<String>,
	std_provider: &dyn Fn(&str) -> Option<String>,
	options: DocOptions,
) -> Result<DocProject, Vec<ProjectDiagnostic>> {
	let project = ProjectId::new(super::FACADE_PROJECT);
	let session = CompilerSession::from_source_loaders(project.clone(), entry, load, std_provider);
	let entry = ModulePath::new(entry).expect("project entry must be a canonical module path");
	extract(&session, project, entry, options).map_err(|d| d.iter().cloned().collect())
}

fn extract(
	session: &CompilerSession,
	project: ProjectId,
	entry: ModulePath,
	options: DocOptions,
) -> Result<DocProject, Arc<[ProjectDiagnostic]>> {
	let diagnostics = session.check_project(
		project.clone(),
		entry.clone(),
		nymph_sema::EntryMode::Library,
	);
	if diagnostics
		.iter()
		.any(|diagnostic| diagnostic.diag.is_error())
	{
		return Err(diagnostics);
	}
	let retained_diagnostics = diagnostics.iter().cloned().collect::<Vec<_>>();
	let mut modules = BTreeMap::new();
	for path in session.graph_order(
		project.clone(),
		entry.clone(),
		nymph_sema::EntryMode::Library,
	) {
		let interface = session
			.documentation_module_interface(
				project.clone(),
				entry.clone(),
				path.clone(),
				options.document_private_items,
			)
			.expect("clean reachable module is registered");
		let interface = match interface {
			Ok(interface) => interface,
			Err(error) => {
				return Err(
					vec![ProjectDiagnostic {
						module: path.as_str().to_string(),
						diag: nymph_diagnostics::Diagnostic::error(
							"INTERNAL-DOCUMENTATION-INTERFACE".into(),
							format!("internal documentation interface conversion failed: {error:?}"),
							nymph_ast::Span::new(0, 0),
						),
					}]
					.into(),
				);
			}
		};
		let private = interface
			.exports
			.iter()
			.chain(
				interface
					.support_definitions
					.iter()
					.map(|support| &support.definition),
			)
			.filter(|definition| definition.visibility == Some(Visibility::Private))
			.map(|definition| definition.id.clone())
			.collect::<std::collections::BTreeSet<_>>();
		let mut items = interface
			.exports
			.iter()
			.cloned()
			.map(|definition| item(definition, options.document_private_items))
			.collect::<Vec<_>>();
		let private_implementations = interface
			.implementations
			.iter()
			.filter(|implementation| implementation_is_private(implementation, &private))
			.map(|implementation| implementation.id.clone())
			.collect::<std::collections::BTreeSet<_>>();
		let mut implementations = interface
			.implementations
			.iter()
			.filter(|implementation| {
				options.document_private_items || !private_implementations.contains(&implementation.id)
			})
			.map(|implementation| DocImplementation {
				definition: implementation.id.clone(),
				private: private_implementations.contains(&implementation.id),
				anchor: String::new(),
				signature: implementation_signature(implementation, options.document_private_items),
			})
			.collect::<Vec<_>>();
		items.sort_by(|a, b| a.definition.cmp(&b.definition));
		implementations.sort_by(|a, b| a.definition.cmp(&b.definition));
		for implementation in &mut implementations {
			implementation.anchor = implementation_anchor(&implementation.definition);
		}
		modules.insert(
			path.clone(),
			DocModule {
				url: module_url(&path),
				path,
				items,
				implementations,
			},
		);
	}
	Ok(DocProject {
		entry,
		modules,
		diagnostics: retained_diagnostics,
	})
}

fn item(definition: ExportedDefinition, document_private_items: bool) -> DocItem {
	let private = definition.visibility == Some(Visibility::Private);
	let anchor = definition_anchor(&definition.id);
	let signature = signature(&definition, document_private_items);
	DocItem {
		definition: definition.id,
		name: definition.name.to_string(),
		kind: definition.kind,
		private,
		anchor,
		signature,
	}
}

fn signature(d: &ExportedDefinition, document_private_items: bool) -> DocSignature {
	let mut out = DocSignature::default();
	let generic_names = d
		.binders
		.iter()
		.map(|binder| (binder.id.clone(), binder.name.as_ref()))
		.collect::<HashMap<_, _>>();
	let keyword = match (d.kind, d.declaration_kind) {
		(DefinitionShapeKind::Function, Some(MemberKind::MutatingFunction)) => "mut func ",
		(DefinitionShapeKind::Function, Some(MemberKind::StaticFunction)) => "namespace func ",
		(DefinitionShapeKind::Function, Some(MemberKind::Function) | None) => "func ",
		(DefinitionShapeKind::Let, Some(MemberKind::MutableValue)) => "let mut ",
		(DefinitionShapeKind::Let, Some(MemberKind::StaticValue)) => "namespace let ",
		(DefinitionShapeKind::Let, Some(MemberKind::Value) | None) => "let ",
		(DefinitionShapeKind::TypeAlias, _) => "type ",
		(DefinitionShapeKind::Struct, _) => "struct ",
		(DefinitionShapeKind::Enum, _) => "enum ",
		(DefinitionShapeKind::Interface, _) => "interface ",
		(DefinitionShapeKind::Namespace, _) => "namespace ",
		(DefinitionShapeKind::Function, Some(_)) | (DefinitionShapeKind::Let, Some(_)) => {
			unreachable!("definition declaration modifier does not match its shape")
		}
	};
	visibility(&mut out, d.visibility);
	external(&mut out, d.external.as_ref());
	text(&mut out, keyword);
	text(&mut out, &d.name);
	binders(&mut out, &d.binders);
	if d.kind == DefinitionShapeKind::Interface && !d.super_interfaces.is_empty() {
		text(&mut out, ": ");
		for (index, interface) in d.super_interfaces.iter().enumerate() {
			if index > 0 {
				text(&mut out, ", ");
			}
			definition(&mut out, &interface.interface);
			generic_args(
				&mut out,
				&interface.positional,
				&interface.named,
				&generic_names,
			);
		}
	}
	if d.kind == DefinitionShapeKind::Function {
		parameters(&mut out, &d.parameters, &generic_names);
	}
	if let Some(ty) = d.return_type.as_ref().or(d.ty.as_ref()) {
		text(
			&mut out,
			if d.kind == DefinitionShapeKind::TypeAlias {
				" = "
			} else {
				": "
			},
		);
		ty_doc(&mut out, ty, &generic_names);
	}
	constraints(&mut out, &d.binders, &d.constraints, &generic_names);
	for field in d
		.fields
		.iter()
		.filter(|field| document_private_items || field.visibility != Some(Visibility::Private))
	{
		text(&mut out, "\n  ");
		visibility(&mut out, field.visibility);
		text(&mut out, &field.name);
		text(&mut out, ": ");
		ty_doc(&mut out, &field.ty, &generic_names);
	}
	for variant in &d.variants {
		text(&mut out, "\n  ");
		text(&mut out, &variant.name);
		let fields = variant
			.fields
			.iter()
			.filter(|field| document_private_items || field.visibility != Some(Visibility::Private))
			.collect::<Vec<_>>();
		if !fields.is_empty() {
			text(&mut out, "(");
			for (i, f) in fields.into_iter().enumerate() {
				if i > 0 {
					text(&mut out, ", ");
				}
				visibility(&mut out, f.visibility);
				text(&mut out, &f.name);
				text(&mut out, ": ");
				ty_doc(&mut out, &f.ty, &generic_names);
			}
			text(&mut out, ")");
		}
	}
	for member in d
		.members
		.iter()
		.filter(|member| document_private_items || member.visibility != Some(Visibility::Private))
	{
		member_doc(&mut out, member, &generic_names);
	}
	out
}

fn implementation_signature(
	implementation: &ExportedImpl,
	document_private_items: bool,
) -> DocSignature {
	let mut out = DocSignature::default();
	let generic_names = implementation
		.binders
		.iter()
		.map(|binder| (binder.id.clone(), binder.name.as_ref()))
		.collect::<HashMap<_, _>>();
	visibility(&mut out, implementation.visibility);
	text(&mut out, "impl");
	binders(&mut out, &implementation.binders);
	text(&mut out, " ");
	if implementation.mutable {
		text(&mut out, "mut ");
	}
	if let Some(interface) = &implementation.interface {
		definition(&mut out, interface);
		generic_args(
			&mut out,
			&[],
			&implementation.interface_arguments,
			&generic_names,
		);
		text(&mut out, " for ");
	}
	ty_doc(&mut out, &implementation.self_type, &generic_names);
	constraints(
		&mut out,
		&implementation.binders,
		&implementation.constraints,
		&generic_names,
	);
	for member in implementation
		.members
		.iter()
		.filter(|member| document_private_items || member.visibility != Some(Visibility::Private))
	{
		member_doc(&mut out, member, &generic_names);
	}
	out
}

fn type_references_private(
	ty: &InterfaceType,
	private_definitions: &std::collections::BTreeSet<DefinitionId>,
) -> bool {
	match ty {
		InterfaceType::Named {
			definition,
			positional,
			named,
		} => {
			private_definitions.contains(definition)
				|| positional
					.iter()
					.any(|ty| type_references_private(ty, private_definitions))
				|| named
					.iter()
					.any(|(_, ty)| type_references_private(ty, private_definitions))
		}
		InterfaceType::List(inner) | InterfaceType::Mutable(inner) => {
			type_references_private(inner, private_definitions)
		}
		InterfaceType::Tuple(items) | InterfaceType::Intersection(items) => items
			.iter()
			.any(|ty| type_references_private(ty, private_definitions)),
		InterfaceType::Map(key, value) => {
			type_references_private(key, private_definitions)
				|| type_references_private(value, private_definitions)
		}
		InterfaceType::Function {
			parameters,
			return_type,
		} => {
			parameters
				.iter()
				.any(|ty| type_references_private(ty, private_definitions))
				|| type_references_private(return_type, private_definitions)
		}
		InterfaceType::Int
		| InterfaceType::UInt
		| InterfaceType::Float
		| InterfaceType::Char
		| InterfaceType::String
		| InterfaceType::Boolean
		| InterfaceType::Void
		| InterfaceType::Never
		| InterfaceType::SelfType
		| InterfaceType::Generic(_) => false,
	}
}

fn implementation_is_private(
	implementation: &ExportedImpl,
	private_definitions: &std::collections::BTreeSet<DefinitionId>,
) -> bool {
	implementation.visibility == Some(Visibility::Private)
		|| implementation
			.interface
			.as_ref()
			.is_some_and(|interface| private_definitions.contains(interface))
		|| type_references_private(&implementation.self_type, private_definitions)
		|| implementation
			.interface_arguments
			.iter()
			.any(|(_, ty)| type_references_private(ty, private_definitions))
		|| implementation.constraints.iter().any(|constraint| {
			private_definitions.contains(&constraint.interface)
				|| constraint
					.positional
					.iter()
					.any(|ty| type_references_private(ty, private_definitions))
				|| constraint
					.named
					.iter()
					.any(|(_, ty)| type_references_private(ty, private_definitions))
		})
}

fn member_doc(
	out: &mut DocSignature,
	member: &MemberShape<InterfaceType>,
	parent_generic_names: &HashMap<nymph_sema::GenericParameterId, &str>,
) {
	let mut generic_names = parent_generic_names.clone();
	generic_names.extend(
		member
			.binders
			.iter()
			.map(|binder| (binder.id.clone(), binder.name.as_ref())),
	);
	text(out, "\n  ");
	visibility(out, member.visibility);
	external(out, member.external.as_ref());
	text(
		out,
		match member.kind {
			MemberKind::Value => "let ",
			MemberKind::MutableValue => "let mut ",
			MemberKind::Function => "func ",
			MemberKind::MutatingFunction => "mut func ",
			MemberKind::StaticValue => "namespace let ",
			MemberKind::StaticFunction => "namespace func ",
		},
	);
	text(out, &member.name);
	binders(out, &member.binders);
	if matches!(
		member.kind,
		MemberKind::Function | MemberKind::MutatingFunction | MemberKind::StaticFunction
	) {
		parameters(out, &member.parameters, &generic_names);
	}
	text(out, ": ");
	ty_doc(out, &member.return_type, &generic_names);
	constraints(out, &member.binders, &member.constraints, &generic_names);
}

fn binders(out: &mut DocSignature, values: &[GenericParameter]) {
	if !values.is_empty() {
		text(out, "<");
		for (i, v) in values.iter().enumerate() {
			if i > 0 {
				text(out, ", ");
			}
			text(out, &v.name);
		}
		text(out, ">");
	}
}
fn parameters(
	out: &mut DocSignature,
	values: &[ParameterShape<InterfaceType>],
	generic_names: &HashMap<nymph_sema::GenericParameterId, &str>,
) {
	text(out, "(");
	for (i, p) in values.iter().enumerate() {
		if i > 0 {
			text(out, ", ");
		}
		if p.spread {
			text(out, "...");
		}
		if p.mutable {
			text(out, "mut ");
		}
		if let Some(name) = &p.name {
			text(out, name);
			text(out, ": ");
		}
		ty_doc(out, &p.ty, generic_names);
	}
	text(out, ")");
}
fn constraints(
	out: &mut DocSignature,
	binders: &[GenericParameter],
	values: &[GenericConstraint],
	generic_names: &HashMap<nymph_sema::GenericParameterId, &str>,
) {
	if values.is_empty() {
		return;
	}
	text(out, " where ");
	for (i, c) in values.iter().enumerate() {
		if i > 0 {
			text(out, ", ");
		}
		let name = binders
			.iter()
			.find(|b| b.id == c.parameter)
			.map_or("?", |b| b.name.as_ref());
		text(out, name);
		text(out, ": ");
		definition(out, &c.interface);
		generic_args(out, &c.positional, &c.named, generic_names);
	}
}
fn generic_args(
	out: &mut DocSignature,
	positional: &[InterfaceType],
	named: &[(ecow::EcoString, InterfaceType)],
	generic_names: &HashMap<nymph_sema::GenericParameterId, &str>,
) {
	if positional.is_empty() && named.is_empty() {
		return;
	}
	text(out, "<");
	let mut first = true;
	for value in positional {
		if !first {
			text(out, ", ");
		}
		first = false;
		ty_doc(out, value, generic_names);
	}
	for (name, value) in named {
		if !first {
			text(out, ", ");
		}
		first = false;
		text(out, name);
		text(out, " = ");
		ty_doc(out, value, generic_names);
	}
	text(out, ">");
}

fn ty_doc(
	out: &mut DocSignature,
	ty: &InterfaceType,
	generic_names: &HashMap<nymph_sema::GenericParameterId, &str>,
) {
	match ty {
		InterfaceType::Int => text(out, "int"),
		InterfaceType::UInt => text(out, "uint"),
		InterfaceType::Float => text(out, "float"),
		InterfaceType::Char => text(out, "char"),
		InterfaceType::String => text(out, "string"),
		InterfaceType::Boolean => text(out, "boolean"),
		InterfaceType::Void => text(out, "void"),
		InterfaceType::Never => text(out, "never"),
		InterfaceType::SelfType => text(out, "self"),
		InterfaceType::List(v) => {
			text(out, "#[");
			ty_doc(out, v, generic_names);
			text(out, "]");
		}
		InterfaceType::Tuple(v) => {
			text(out, "#(");
			list_types(out, v, ", ", generic_names);
			text(out, ")");
		}
		InterfaceType::Map(k, v) => {
			text(out, "#{ ");
			ty_doc(out, k, generic_names);
			text(out, ": ");
			ty_doc(out, v, generic_names);
			text(out, " }");
		}
		InterfaceType::Function {
			parameters,
			return_type,
		} => {
			text(out, "(");
			list_types(out, parameters, ", ", generic_names);
			text(out, ") -> ");
			ty_doc(out, return_type, generic_names);
		}
		InterfaceType::Named {
			definition: d,
			positional,
			named,
		} => {
			definition(out, d);
			generic_args(out, positional, named, generic_names);
		}
		InterfaceType::Intersection(v) => list_types(out, v, " + ", generic_names),
		InterfaceType::Mutable(v) => {
			text(out, "mut ");
			ty_doc(out, v, generic_names);
		}
		InterfaceType::Generic(g) => text(
			out,
			generic_names.get(g).copied().unwrap_or("<unknown generic>"),
		),
	}
}
fn list_types(
	out: &mut DocSignature,
	values: &[InterfaceType],
	separator: &str,
	generic_names: &HashMap<nymph_sema::GenericParameterId, &str>,
) {
	for (i, v) in values.iter().enumerate() {
		if i > 0 {
			text(out, separator);
		}
		ty_doc(out, v, generic_names);
	}
}
fn definition(out: &mut DocSignature, target: &DefinitionId) {
	out.0.push(DocFragment::Definition {
		label: definition_name(target),
		target: target.clone(),
	});
}
fn text(out: &mut DocSignature, value: &str) {
	out.0.push(DocFragment::Text(value.to_string()));
}
fn visibility(out: &mut DocSignature, value: Option<Visibility>) {
	let keyword = match value {
		Some(Visibility::Public) => "public ",
		Some(Visibility::Internal) => "internal ",
		Some(Visibility::Private) => "private ",
		None => return,
	};
	text(out, keyword);
}
fn external(out: &mut DocSignature, value: Option<&nymph_sema::ExternalAbi>) {
	if let Some(value) = value {
		text(out, "external(");
		text(out, &value.marker);
		text(out, ") ");
	}
}
fn definition_name(id: &DefinitionId) -> String {
	match &id.key {
		nymph_sema::DeclarationKey::TopLevel { name, .. }
		| nymph_sema::DeclarationKey::Member { name, .. } => name.to_string(),
		_ => "definition".to_string(),
	}
}

fn module_url(path: &ModulePath) -> String {
	format!("modules/{}.html", encode_path(path.as_str()))
}
fn definition_anchor(id: &DefinitionId) -> String {
	format!("item-{}-{}", slug(&definition_name(id)), id.key.duplicate())
}
fn implementation_anchor(id: &DefinitionId) -> String {
	format!("implementation-{}", slug(&format!("{:?}", id.key)))
}
fn encode_path(path: &str) -> String {
	path.split('/').map(path_slug).collect::<Vec<_>>().join("/")
}
fn path_slug(value: &str) -> String {
	let mut encoded = String::new();
	for b in value.bytes() {
		if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' {
			encoded.push(char::from(b));
		} else {
			encoded.push_str(&format!("~{b:02X}"));
		}
	}
	if is_windows_reserved(&encoded) {
		format!("~R{encoded}")
	} else {
		encoded
	}
}
fn is_windows_reserved(value: &str) -> bool {
	let value = value.to_ascii_lowercase();
	matches!(value.as_str(), "con" | "prn" | "aux" | "nul")
		|| value
			.strip_prefix("com")
			.or_else(|| value.strip_prefix("lpt"))
			.is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}
fn slug(value: &str) -> String {
	let mut out = String::new();
	for b in value.bytes() {
		if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
			out.push(char::from(b));
		} else {
			out.push_str(&format!("~{b:02X}"));
		}
	}
	out
}

impl DocProject {
	/// Render a deterministic, self-contained static site in memory.
	#[must_use]
	pub fn render_html(&self) -> StaticDocSite {
		let mut files = BTreeMap::new();
		let mut index = page_start("Nymph documentation", "");
		index.push_str("<h1>Modules</h1><ul>");
		for module in self.modules.values() {
			index.push_str("<li><a href=\"");
			index.push_str(&escape(&module.url));
			index.push_str("\">");
			index.push_str(&escape(module.path.as_str()));
			index.push_str("</a></li>");
		}
		index.push_str("</ul>");
		page_end(&mut index);
		files.insert("index.html".into(), index);
		for module in self.modules.values() {
			let root = "../".repeat(module.url.matches('/').count());
			let mut html = page_start(module.path.as_str(), &root);
			html.push_str("<p><a href=\"");
			html.push_str(&root);
			html.push_str("index.html\">Modules</a></p><h1>");
			html.push_str(&escape(module.path.as_str()));
			html.push_str("</h1>");
			for item in &module.items {
				html.push_str("<section id=\"");
				html.push_str(&item.anchor);
				html.push_str("\"><h2>");
				html.push_str(&escape(&item.name));
				html.push_str("</h2><pre><code>");
				render_signature(&mut html, &item.signature, &self.modules, &root);
				html.push_str("</code></pre></section>");
			}
			for implementation in &module.implementations {
				html.push_str("<section id=\"");
				html.push_str(&implementation.anchor);
				html.push_str("\"><h2>Implementation</h2><pre><code>");
				render_signature(&mut html, &implementation.signature, &self.modules, &root);
				html.push_str("</code></pre></section>");
			}
			page_end(&mut html);
			files.insert(module.url.clone(), html);
		}
		files.insert("assets/style.css".into(), "body{font-family:system-ui,sans-serif;max-width:70rem;margin:2rem auto;padding:0 1rem}pre{background:#f4f4f4;padding:1rem;overflow:auto}code{white-space:pre-wrap}".into());
		files
	}
}
fn render_signature(
	out: &mut String,
	signature: &DocSignature,
	modules: &BTreeMap<ModulePath, DocModule>,
	root: &str,
) {
	for fragment in &signature.0 {
		match fragment {
			DocFragment::Text(v) => out.push_str(&escape(v)),
			DocFragment::Definition { label, target } => {
				let path = ModulePath::new(target.module.path.as_str()).ok();
				if let Some(module) = path
					.as_ref()
					.and_then(|path| modules.get(path))
					.filter(|module| module.items.iter().any(|item| item.definition == *target))
				{
					out.push_str("<a href=\"");
					out.push_str(&escape(&format!(
						"{root}{}#{}",
						module.url,
						definition_anchor(target)
					)));
					out.push_str("\">");
					out.push_str(&escape(label));
					out.push_str("</a>");
				} else {
					out.push_str(&escape(label));
				}
			}
		}
	}
}
fn page_start(title: &str, root: &str) -> String {
	format!(
		"<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title><link rel=\"stylesheet\" href=\"{root}assets/style.css\"></head><body>",
		escape(title)
	)
}
fn page_end(out: &mut String) {
	out.push_str("</body></html>\n");
}
fn escape(value: &str) -> String {
	let mut out = String::new();
	for c in value.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'\"' => out.push_str("&quot;"),
			'\'' => out.push_str("&#39;"),
			_ => out.push(c),
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use nymph_sema::{
		DeclarationCategory, DeclarationKey, ModuleIdentity, ModuleOrigin, StableIdBuilder,
	};

	#[test]
	fn doc_duplicate_semantic_ids_have_distinct_stable_anchors() {
		let identity = ModuleIdentity {
			origin: ModuleOrigin::Project("test".into()),
			project: "test".into(),
			path: "main".into(),
		};
		let mut ids = StableIdBuilder::new(identity);
		let first = ids.allocate(DeclarationKey::top_level(
			DeclarationCategory::Function,
			"same",
		));
		let second = ids.allocate(DeclarationKey::top_level(
			DeclarationCategory::Function,
			"same",
		));
		assert_eq!(definition_anchor(&first), "item-same-0");
		assert_eq!(definition_anchor(&second), "item-same-1");
	}
}
