use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};

use ecow::EcoString;
use nymph_ast::{
	OriginId, Span, Spanned, SyntaxContext,
	decl::{
		Declaration, FuncDeclaration, FuncParam, ImplMember, ImportRoot, Module, StructField,
		StructImpl,
	},
	expr::{
		CallArg, ClosureParam, Expr, ExprKind, ListItem, MapEntry, Pattern, RangeKind, Statement,
		StringPart, StructPatternField, TokenLiteralPiece,
	},
	ops::{BinaryOperator, PrefixOperator},
	token::{StrFragment, Token},
	ty::Type,
};
use nymph_diagnostics::{Diagnostic, Label};

const STEP_LIMIT: u64 = 100_000;
const VALUE_LIMIT: u64 = 100_000;
const CALL_DEPTH_LIMIT: u32 = 24;
const OUTPUT_TOKEN_LIMIT: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExpandedModule {
	pub tree: Module,
	pub tokens: Vec<Spanned<Token>>,
	pub diagnostics: Vec<Diagnostic>,
	pub origins: Vec<ExpansionOrigin>,
}

struct ExpandedItems {
	declarations: Vec<Declaration>,
	tokens: Vec<Spanned<Token>>,
}

type ParsedDeclarations = (Vec<Declaration>, Vec<Vec<Spanned<Token>>>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExpansionOrigin {
	pub id: OriginId,
	pub invocation: Span,
	pub definition: Span,
	pub parent: OriginId,
}

pub(crate) fn add_expansion_trace(
	mut diagnostic: Diagnostic,
	origins: &[ExpansionOrigin],
) -> Diagnostic {
	if diagnostic
		.notes
		.iter()
		.any(|note| note.starts_with("expanded at "))
	{
		return diagnostic;
	}
	let mut origin = diagnostic.span.origin;
	let mut seen = BTreeSet::new();
	while origin != OriginId::SOURCE && seen.insert(origin) {
		let Some(record) = origins.iter().find(|record| record.id == origin) else {
			break;
		};
		diagnostic = diagnostic
			.with_label(Label::new(
				record.invocation,
				"macro expansion was invoked here",
			))
			.with_label(Label::new(record.definition, "macro was defined here"))
			.with_note(format!(
				"expanded at {}..{} from macro definition at {}..{}",
				record.invocation.start,
				record.invocation.end,
				record.definition.start,
				record.definition.end,
			));
		origin = record.parent;
	}
	if !seen.is_empty() && diagnostic.help.is_none() {
		diagnostic = diagnostic.with_help(
			"fix the generated syntax at the labeled macro definition, or change the invocation arguments so it emits valid Nymph",
		);
	}
	diagnostic
}

pub(crate) fn render_tokens(tokens: &[Spanned<Token>], source: &str) -> String {
	let mut output = String::new();
	let mut previous: Option<(Span, bool)> = None;
	for Spanned(token, span) in tokens {
		let source_text = source
			.get(span.start..span.end)
			.filter(|text| {
				let lexed = nymph_syntax::lex(text);
				lexed.diagnostics.is_empty() && lexed.tokens.len() == 1 && lexed.tokens[0].0 == *token
			})
			.map(str::to_owned);
		let from_source = source_text.is_some();
		if let Some((previous_span, previous_from_source)) = previous {
			let trivia = (previous_from_source && from_source && previous_span.end <= span.start)
				.then(|| source.get(previous_span.end..span.start))
				.flatten()
				.filter(|gap| nymph_syntax::lex(gap).tokens.is_empty());
			if let Some(trivia) = trivia {
				output.push_str(trivia);
			} else {
				output.push(' ');
			}
		}
		if let Some(source_text) = source_text {
			output.push_str(&source_text);
		} else {
			output.push_str(&token_text(token));
		}
		previous = Some((*span, from_source));
	}
	output
}

fn token_text(token: &Token) -> EcoString {
	use Token::*;
	match token {
		Int(value) => value.to_string().into(),
		UInt(value) => format!("{value}u").into(),
		Float(value) => {
			let mut text = value.to_string();
			if !text.contains(['.', 'e', 'E']) {
				text.push_str(".0");
			}
			text.into()
		}
		Char(value) => format!("'{}'", escape_char(*value)).into(),
		Str(fragments) => {
			let mut text = String::from("\"");
			for fragment in fragments {
				match &fragment.0 {
					StrFragment::Text(value) => push_escaped_string_text(&mut text, value),
					StrFragment::Escape(value) => text.push_str(&value.to_string()),
					StrFragment::Interpolation(tokens) => {
						text.push_str("${");
						text.push_str(&render_tokens(tokens, ""));
						text.push('}');
					}
				}
			}
			text.push('"');
			text.into()
		}
		Identifier(value) => value.clone(),
		AnonymousParam(index) => index.map_or_else(|| "$".into(), |index| format!("${index}").into()),
		True => "true".into(),
		False => "false".into(),
		Public => "public".into(),
		Internal => "internal".into(),
		Private => "private".into(),
		Import => "import".into(),
		With => "with".into(),
		Type => "type".into(),
		Struct => "struct".into(),
		Enum => "enum".into(),
		Let => "let".into(),
		External => "external".into(),
		Effect => "effect".into(),
		Const => "const".into(),
		Func => "func".into(),
		Interface => "interface".into(),
		Impl => "impl".into(),
		Namespace => "namespace".into(),
		For => "for".into(),
		Loop => "loop".into(),
		If => "if".into(),
		Else => "else".into(),
		Match => "match".into(),
		Continue => "continue".into(),
		Break => "break".into(),
		Echo => "echo".into(),
		This => "this".into(),
		In => "in".into(),
		As => "as".into(),
		Is => "is".into(),
		Async => "async".into(),
		Await => "await".into(),
		IntType => "int".into(),
		UIntType => "uint".into(),
		FloatType => "float".into(),
		BooleanType => "boolean".into(),
		CharType => "char".into(),
		StringType => "string".into(),
		VoidType => "void".into(),
		NeverType => "never".into(),
		SelfType => "Self".into(),
		LParen => "(".into(),
		RParen => ")".into(),
		LBracket => "[".into(),
		RBracket => "]".into(),
		LBrace => "{".into(),
		RBrace => "}".into(),
		HashLParen => "#(".into(),
		HashLBracket => "#[".into(),
		HashLBrace => "#{".into(),
		Arrow => "->".into(),
		DotDotDot => "...".into(),
		Question => "?".into(),
		DoubleQuestion => "??".into(),
		QuestionDot => "?.".into(),
		Dot => ".".into(),
		At => "@".into(),
		Hash => "#".into(),
		Dollar => "$".into(),
		Backslash => "\\".into(),
		Backtick => "`".into(),
		Comma => ",".into(),
		Semicolon => ";".into(),
		Colon => ":".into(),
		ColonColon => "::".into(),
		Underscore => "_".into(),
		PipeArrow => "|>".into(),
		Bang => "!".into(),
		Plus => "+".into(),
		Minus => "-".into(),
		Star => "*".into(),
		Slash => "/".into(),
		Percent => "%".into(),
		StarStar => "**".into(),
		Amp => "&".into(),
		Pipe => "|".into(),
		Caret => "^".into(),
		Tilde => "~".into(),
		EqEq => "==".into(),
		BangEq => "!=".into(),
		Lt => "<".into(),
		Gt => ">".into(),
		LtEq => "<=".into(),
		GtEq => ">=".into(),
		BangIn => "!in".into(),
		BangIs => "!is".into(),
		AmpAmp => "&&".into(),
		PipePipe => "||".into(),
		Eq => "=".into(),
		DotDot => "..".into(),
		DotDotEq => "..=".into(),
		Error => "_".into(),
	}
}

fn push_escaped_string_text(output: &mut String, value: &str) {
	let mut chars = value.chars().peekable();
	while let Some(character) = chars.next() {
		match character {
			'\\' => output.push_str("\\\\"),
			'\n' => output.push_str("\\n"),
			'\r' => output.push_str("\\r"),
			'\t' => output.push_str("\\t"),
			'"' => output.push_str("\\\""),
			'$' if chars.peek() == Some(&'{') => output.push_str("\\$"),
			character => output.push(character),
		}
	}
}

fn escape_char(value: char) -> EcoString {
	match value {
		'\\' => "\\\\".into(),
		'\n' => "\\n".into(),
		'\r' => "\\r".into(),
		'\t' => "\\t".into(),
		'\'' => "\\'".into(),
		value => value.to_string().into(),
	}
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
	Int(i128),
	UInt(u64),
	Float(f64),
	Char(char),
	Boolean(bool),
	String(EcoString),
	List(Vec<Value>),
	Tuple(Vec<Value>),
	Map(Vec<(Value, Value)>),
	Closure {
		params: Vec<Spanned<ClosureParam>>,
		body: Box<Expr>,
		env: Env,
	},
	Tokens(Vec<Spanned<Token>>),
	Name(EcoString, Span),
	Record {
		owner: Option<EcoString>,
		name: EcoString,
		fields: BTreeMap<EcoString, Value>,
		tokens: Vec<Spanned<Token>>,
	},
	Variant {
		name: EcoString,
		value: Box<Value>,
		tokens: Vec<Spanned<Token>>,
	},
	Break(Option<Box<Value>>, Option<EcoString>),
	Continue(Vec<(EcoString, Value)>, Option<EcoString>),
	Void,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeclarationKind {
	Import,
	Let,
	Function,
	Struct,
	Enum,
	Interface,
	Implementation,
	Namespace,
	Effect,
	TypeAlias,
	External,
}

#[derive(Default)]
struct Budget {
	steps: u64,
	values: u64,
	depth: u32,
}

type Env = BTreeMap<EcoString, Value>;

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub(crate) struct ConstImportSource {
	pub(crate) source: EcoString,
	pub(crate) local: EcoString,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub(crate) struct ConstImport {
	pub(crate) target: EcoString,
	pub(crate) namespace: EcoString,
	pub(crate) names: Vec<ConstImportSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub(crate) struct ConstModuleSource {
	pub(crate) key: EcoString,
	pub(crate) path: EcoString,
	pub(crate) source: Arc<str>,
	pub(crate) imports: Vec<ConstImport>,
}

#[derive(Clone)]
struct ConstFunction {
	owner: EcoString,
	path: EcoString,
	meta: FuncDeclaration,
	body: Expr,
}

#[derive(Clone)]
struct ConstMethod {
	owner: EcoString,
	path: EcoString,
	interface: Option<EcoString>,
	meta: FuncDeclaration,
	body: Expr,
}

#[derive(Clone)]
struct AttachedTarget {
	kind: DeclarationKind,
	tokens: Vec<Spanned<Token>>,
	declaration: Box<Declaration>,
}

enum AttachedTargetType {
	Declaration,
	Narrow,
}

struct Evaluator<'a> {
	module: &'a Module,
	root: EcoString,
	owner: EcoString,
	functions: BTreeMap<(EcoString, EcoString), ConstFunction>,
	methods: BTreeMap<(EcoString, EcoString), Vec<ConstMethod>>,
	const_lets: BTreeMap<(EcoString, EcoString), Expr>,
	public_consts: BTreeSet<(EcoString, EcoString)>,
	imports: BTreeMap<EcoString, Vec<ConstImport>>,
	meta_type_names: BTreeSet<EcoString>,
	evaluated_lets: BTreeMap<(EcoString, EcoString), Value>,
	active_lets: BTreeSet<(EcoString, EcoString)>,
	budget: Budget,
	trace: Vec<(EcoString, Span)>,
	fresh_names: u64,
	current_origin: OriginId,
	current_definition: u64,
	origins: BTreeMap<OriginId, ExpansionOrigin>,
	deferred_diagnostics: Vec<Diagnostic>,
}

#[cfg(test)]
pub(crate) fn expand_module(module: &Module) -> ExpandedModule {
	expand_module_with_imports(module, module.path.clone(), &[])
}

#[cfg(test)]
pub(crate) fn expand_module_with_imports(
	module: &Module,
	root: EcoString,
	imports: &[ConstModuleSource],
) -> ExpandedModule {
	let evaluator = Evaluator::new(module, root, imports);
	expand_parsed_module(evaluator, module, &[], &[], Vec::new())
}

pub(crate) fn expand_source_with_imports(
	source: &str,
	path: EcoString,
	root: EcoString,
	imports: &[ConstModuleSource],
) -> ExpandedModule {
	let lexed = nymph_syntax::lex(source);
	let raw = nymph_syntax::parse_module(source, path.clone());
	let mut evaluator = Evaluator::new(&raw.tree, root, imports);
	let mut tokens = lexed.tokens;
	let mut diagnostics = lexed.diagnostics;
	let mut next_node_id = next_node_id(&raw.tree);
	if let Err(diagnostic) = evaluator.expand_source_tokens(&mut tokens, &mut next_node_id) {
		diagnostics.push(diagnostic);
		diagnostics.append(&mut evaluator.deferred_diagnostics);
		return ExpandedModule {
			tree: raw.tree.clone(),
			tokens,
			diagnostics,
			origins: evaluator.origins.into_values().collect(),
		};
	}
	let eoi = Span::new(source.len(), source.len());
	let ranged = nymph_syntax::parse_module_tokens_from_with_ranges(&tokens, eoi, path, next_node_id);
	diagnostics.extend(ranged.parsed.diagnostics.into_iter().map(|diagnostic| {
		if diagnostic.span.origin == OriginId::SOURCE {
			diagnostic
		} else {
			diagnostic.with_note("while parsing compile-time expansion output")
		}
	}));
	expand_parsed_module(
		evaluator,
		&ranged.parsed.tree,
		&tokens,
		&ranged.declaration_ranges,
		diagnostics,
	)
}

fn expand_parsed_module(
	mut evaluator: Evaluator<'_>,
	module: &Module,
	tokens: &[Spanned<Token>],
	declaration_ranges: &[std::ops::Range<usize>],
	mut diagnostics: Vec<Diagnostic>,
) -> ExpandedModule {
	let mut members = Vec::new();
	let mut expanded_tokens = Vec::new();
	let mut next_node_id = next_node_id(module);

	for (index, declaration) in module.members.iter().enumerate() {
		let declaration_tokens = declaration_ranges
			.get(index)
			.map_or(&[][..], |range| &tokens[range.clone()]);
		if let Declaration::Let { meta, .. } = declaration
			&& meta.is_const
			&& let Some(name) = meta.name.0.as_binding()
		{
			evaluator.begin_root();
			if let Err(diagnostic) = evaluator.eval_const_let(&name.0, meta.name.1) {
				diagnostics.push(diagnostic);
				diagnostics.append(&mut evaluator.deferred_diagnostics);
				continue;
			}
		}
		evaluator.begin_root();
		match evaluator.expand_declaration(declaration, declaration_tokens, &mut next_node_id) {
			Ok(mut expanded) => {
				members.append(&mut expanded.declarations);
				expanded_tokens.append(&mut expanded.tokens);
			}
			Err(diagnostic) => diagnostics.push(diagnostic),
		}
		diagnostics.append(&mut evaluator.deferred_diagnostics);
	}

	ExpandedModule {
		tree: Module {
			members,
			path: module.path.clone(),
		},
		tokens: expanded_tokens,
		diagnostics,
		origins: evaluator.origins.into_values().collect(),
	}
}

#[allow(clippy::result_large_err)]
impl<'a> Evaluator<'a> {
	fn new(module: &'a Module, root: EcoString, imported_modules: &[ConstModuleSource]) -> Self {
		let mut functions = BTreeMap::new();
		let mut methods = BTreeMap::new();
		let mut const_lets = BTreeMap::new();
		let mut public_consts = BTreeSet::new();
		collect_const_declarations(
			module,
			&root,
			&module.path,
			&mut functions,
			&mut methods,
			&mut const_lets,
			&mut public_consts,
		);
		let mut imports = BTreeMap::new();
		for imported in imported_modules {
			let parsed = nymph_syntax::parse_module(&imported.source, imported.path.clone());
			collect_const_declarations(
				&parsed.tree,
				&imported.key,
				&imported.path,
				&mut functions,
				&mut methods,
				&mut const_lets,
				&mut public_consts,
			);
			imports.insert(imported.key.clone(), imported.imports.clone());
		}
		let meta_type_names = imported_meta_type_names(module);
		Self {
			module,
			root: root.clone(),
			owner: root,
			functions,
			methods,
			const_lets,
			public_consts,
			imports,
			meta_type_names,
			evaluated_lets: BTreeMap::new(),
			active_lets: BTreeSet::new(),
			budget: Budget::default(),
			trace: Vec::new(),
			fresh_names: 0,
			current_origin: OriginId::SOURCE,
			current_definition: 0,
			origins: BTreeMap::new(),
			deferred_diagnostics: Vec::new(),
		}
	}

	fn begin_root(&mut self) {
		self.budget = Budget::default();
		self.trace.clear();
		self.owner = self.root.clone();
		self.deferred_diagnostics.clear();
	}

	fn begin_attached_call(&mut self, parent: OriginId) {
		self.budget = Budget::default();
		self.trace.clear();
		self.owner = self.root.clone();
		self.current_origin = parent;
		self.current_definition = 0;
		self.active_lets.clear();
	}

	fn runtime_capable_function(&self, meta: &FuncDeclaration) -> bool {
		meta
			.params
			.iter()
			.all(|parameter| self.runtime_capable_type(&parameter.0.type_.0))
			&& meta
				.return_type
				.as_ref()
				.is_none_or(|return_type| self.runtime_capable_type(&return_type.0))
	}

	fn runtime_capable_let(&self, meta: &nymph_ast::decl::LetDeclaration) -> bool {
		meta
			.type_
			.as_ref()
			.is_none_or(|ty| self.runtime_capable_type(&ty.0))
	}

	fn runtime_capable_implementation(
		&self,
		interface: &(nymph_ast::Ident, Vec<Spanned<nymph_ast::ty::GenericArg>>),
		members: &[Spanned<ImplMember>],
	) -> bool {
		interface.1.iter().all(|argument| {
			argument
				.0
				.value
				.as_type()
				.is_none_or(|ty| self.runtime_capable_type(&ty.0))
		}) && members.iter().all(|member| match &member.0 {
			ImplMember::Func { meta, .. } | ImplMember::ExternalFunc(_, _, meta) => {
				self.runtime_capable_function(meta)
			}
			ImplMember::Let { meta, .. } | ImplMember::ExternalLet(_, _, meta) => {
				self.runtime_capable_let(meta)
			}
		})
	}

	fn runtime_capable_type(&self, ty: &Type) -> bool {
		match ty {
			Type::Reference { name, generics } => {
				let prefix = name.0.split('.').next().unwrap_or(name.0.as_str());
				!name.0.starts_with("meta.")
					&& !name.0.starts_with("std.meta.")
					&& !self.meta_type_names.contains(prefix)
					&& !self.meta_type_names.contains(&name.0)
					&& generics.iter().all(|argument| {
						argument
							.0
							.value
							.as_type()
							.is_none_or(|ty| self.runtime_capable_type(&ty.0))
					})
			}
			Type::List(inner) | Type::Grouped(inner) => self.runtime_capable_type(&inner.0),
			Type::Tuple(items) => items.iter().all(|item| self.runtime_capable_type(&item.0)),
			Type::Map(key, value) | Type::Intersection(key, value) => {
				self.runtime_capable_type(&key.0) && self.runtime_capable_type(&value.0)
			}
			Type::Function {
				params,
				return_type,
				..
			} => {
				params
					.iter()
					.all(|(_, ty)| self.runtime_capable_type(&ty.0))
					&& self.runtime_capable_type(&return_type.0)
			}
			_ => true,
		}
	}

	fn resolve_const_path(&self, path: &str) -> Option<(EcoString, EcoString)> {
		let (prefix, name) = path
			.split_once('.')
			.map_or((None, path), |(prefix, name)| (Some(prefix), name));
		if prefix.is_none() {
			let local = (self.owner.clone(), name.into());
			if self.functions.contains_key(&local) || self.const_lets.contains_key(&local) {
				return Some(local);
			}
		}
		for import in self.imports.get(&self.owner).into_iter().flatten() {
			if prefix == Some(import.namespace.as_str()) {
				let key = (import.target.clone(), name.into());
				return self.public_consts.contains(&key).then_some(key);
			}
			if prefix.is_none()
				&& let Some(binding) = import.names.iter().find(|binding| binding.local == name)
			{
				let key = (import.target.clone(), binding.source.clone());
				return self.public_consts.contains(&key).then_some(key);
			}
		}
		None
	}

	fn resolve_const_name(&self, name: &EcoString) -> Option<(EcoString, EcoString)> {
		self.resolve_const_path(name)
	}

	fn eval_const_let(&mut self, name: &EcoString, span: Span) -> Result<Value, Diagnostic> {
		let key = self
			.resolve_const_name(name)
			.unwrap_or_else(|| (self.owner.clone(), name.clone()));
		if let Some(value) = self.evaluated_lets.get(&key) {
			return Ok(value.clone());
		}
		let Some(initializer) = self.const_lets.get(&key).cloned() else {
			return Err(self.error(span, format!("unknown const binding `{name}`")));
		};
		if !self.active_lets.insert(key.clone()) {
			return Err(self.error(span, format!("const binding cycle through `{name}`")));
		}
		let previous_owner = std::mem::replace(&mut self.owner, key.0.clone());
		let previous_definition = self.current_definition;
		self.current_definition =
			stable_hash(&[key.0.as_bytes(), name.as_bytes(), &span.start.to_le_bytes()]);
		let value = self.eval(&initializer, &mut Env::new());
		self.current_definition = previous_definition;
		self.owner = previous_owner;
		self.active_lets.remove(&key);
		let value = value?;
		self.evaluated_lets.insert(key, value.clone());
		Ok(value)
	}

	fn expand_declaration(
		&mut self,
		declaration: &Declaration,
		declaration_tokens: &[Spanned<Token>],
		next_node_id: &mut u32,
	) -> Result<ExpandedItems, Diagnostic> {
		match declaration {
			Declaration::Expansion(expression) => {
				let previous_origin = self.current_origin;
				self.current_origin =
					self.intern_origin(expression.span, expression.span, expression.span.origin);
				let tokens = self
					.eval(expression, &mut Env::new())
					.and_then(|value| self.require_tokens(value, expression.span));
				self.current_origin = previous_origin;
				let tokens = tokens?;
				let (declarations, token_groups) =
					self.parse_declarations(tokens, expression.span, next_node_id)?;
				self.expand_generated(declarations, token_groups, next_node_id)
			}
			Declaration::Attached {
				macros,
				target_tokens,
				target,
			} => self.expand_attached(macros, target_tokens, target, next_node_id),
			Declaration::Import {
				idents: Some(idents),
				..
			} => {
				let mut retained = idents.clone();
				for import in self.imports.get(&self.owner).into_iter().flatten() {
					retained.retain(|(source, alias)| {
						let local = alias.as_ref().unwrap_or(source);
						let Some(binding) = import.names.iter().find(|binding| binding.local == local.0) else {
							return true;
						};
						let key = (import.target.clone(), binding.source.clone());
						!self.functions.contains_key(&key) && !self.const_lets.contains_key(&key)
					});
				}
				if retained.is_empty() {
					Ok(ExpandedItems {
						declarations: Vec::new(),
						tokens: Vec::new(),
					})
				} else {
					let mut import = declaration.clone();
					let Declaration::Import { idents, .. } = &mut import else {
						unreachable!()
					};
					*idents = Some(retained);
					let tokens = if import == *declaration {
						declaration_tokens.to_vec()
					} else {
						import_tokens(&import)
					};
					Ok(ExpandedItems {
						declarations: vec![import],
						tokens,
					})
				}
			}
			Declaration::Func { meta, .. } if !self.runtime_capable_function(meta) => {
				if meta.is_const {
					Ok(ExpandedItems {
						declarations: Vec::new(),
						tokens: Vec::new(),
					})
				} else {
					Err(self.error(meta.name.1, "std/meta values are compile-time-only"))
				}
			}
			Declaration::Let { meta, .. } if !self.runtime_capable_let(meta) => {
				if meta.is_const {
					Ok(ExpandedItems {
						declarations: Vec::new(),
						tokens: Vec::new(),
					})
				} else {
					Err(self.error(meta.name.1, "std/meta values are compile-time-only"))
				}
			}
			Declaration::ImplFor {
				for_interface,
				members,
				..
			} if !self.runtime_capable_implementation(for_interface, members) => Ok(ExpandedItems {
				declarations: Vec::new(),
				tokens: Vec::new(),
			}),
			other => Ok(ExpandedItems {
				declarations: vec![other.clone()],
				tokens: declaration_tokens.to_vec(),
			}),
		}
	}

	fn expand_attached(
		&mut self,
		macros: &[Expr],
		target_tokens: &[Spanned<Token>],
		target: &Declaration,
		next_node_id: &mut u32,
	) -> Result<ExpandedItems, Diagnostic> {
		let kind = declaration_kind(target).ok_or_else(|| {
			self.error(
				macros.first().map_or(Span::new(0, 0), |call| call.span),
				"attached macros require one concrete declaration",
			)
		})?;
		let original = AttachedTarget {
			kind,
			tokens: target_tokens.to_vec(),
			declaration: Box::new(target.clone()),
		};
		let mut expanded = self.expand_declaration(target, target_tokens, next_node_id)?;
		let parent_origin = declaration_span(target).origin;

		for call in macros {
			self.begin_attached_call(parent_origin);
			let diagnostic_start = self.deferred_diagnostics.len();
			let result = self.eval_call(call, &mut Env::new(), Some(original.clone()));
			let result = match result {
				Ok(value) => {
					if let Some(origin) = self
						.origins
						.values()
						.find(|origin| origin.invocation == call.span && origin.parent == parent_origin)
						.map(|origin| origin.id)
					{
						self.current_origin = origin;
					}
					self
						.require_tokens(value, call.span)
						.and_then(|tokens| self.parse_declarations(tokens, call.span, next_node_id))
						.and_then(|(declarations, token_groups)| {
							self.expand_generated(declarations, token_groups, next_node_id)
						})
				}
				Err(diagnostic) => Err(diagnostic),
			};
			match result {
				Ok(mut generated) => {
					expanded.declarations.append(&mut generated.declarations);
					expanded.tokens.append(&mut generated.tokens);
				}
				Err(diagnostic) => self
					.deferred_diagnostics
					.insert(diagnostic_start, diagnostic),
			}
		}

		Ok(expanded)
	}

	fn expand_generated(
		&mut self,
		declarations: Vec<Declaration>,
		token_groups: Vec<Vec<Spanned<Token>>>,
		next_node_id: &mut u32,
	) -> Result<ExpandedItems, Diagnostic> {
		let mut expanded = ExpandedItems {
			declarations: Vec::new(),
			tokens: Vec::new(),
		};
		for (declaration, tokens) in declarations.into_iter().zip(token_groups) {
			self.step(declaration_span(&declaration))?;
			let mut item = self.expand_declaration(&declaration, &tokens, next_node_id)?;
			expanded.declarations.append(&mut item.declarations);
			expanded.tokens.append(&mut item.tokens);
		}
		Ok(expanded)
	}

	fn parse_declarations(
		&mut self,
		mut tokens: Vec<Spanned<Token>>,
		span: Span,
		next_node_id: &mut u32,
	) -> Result<ParsedDeclarations, Diagnostic> {
		self.expand_source_tokens(&mut tokens, next_node_id)?;
		if tokens.len() > OUTPUT_TOKEN_LIMIT {
			return Err(self.error(
				span,
				format!("compile-time output exceeded the {OUTPUT_TOKEN_LIMIT} token limit"),
			));
		}
		let end = tokens.last().map_or(span.end, |token| token.1.end);
		let ranged = nymph_syntax::parse_module_tokens_from_with_ranges(
			&tokens,
			Span::new(end, end)
				.with_context(SyntaxContext::CallSite(self.current_origin))
				.with_origin(self.current_origin),
			self.module.path.clone(),
			*next_node_id,
		);
		let mut diagnostics = ranged
			.parsed
			.diagnostics
			.into_iter()
			.map(|diagnostic| diagnostic.with_note("while parsing compile-time expansion output"));
		if let Some(diagnostic) = diagnostics.next() {
			self.deferred_diagnostics.extend(diagnostics);
			return Err(diagnostic);
		}
		*next_node_id =
			max_node_id(&ranged.parsed.tree).map_or(*next_node_id, |id| id.saturating_add(1));
		let token_groups = ranged
			.declaration_ranges
			.into_iter()
			.map(|range| tokens[range].to_vec())
			.collect();
		Ok((ranged.parsed.tree.members, token_groups))
	}

	fn expand_source_tokens(
		&mut self,
		tokens: &mut Vec<Spanned<Token>>,
		next_node_id: &mut u32,
	) -> Result<(), Diagnostic> {
		let mut index = 0;
		while index + 1 < tokens.len() {
			if tokens[index].0 == Token::Backslash && tokens[index + 1].0 == Token::LParen {
				index = matching_rparen(tokens, index + 1).map_or(tokens.len(), |end| end + 1);
				continue;
			}
			if tokens[index].0 != Token::Dollar || tokens[index + 1].0 != Token::LParen {
				index += 1;
				continue;
			}
			let Some(end) = matching_rparen(tokens, index + 1) else {
				return Err(self.error(tokens[index].1, "unterminated source expansion"));
			};
			let span = tokens[index].1.to(tokens[end].1);
			let parsed = nymph_syntax::parse_expression_tokens_from(
				&tokens[index + 2..end],
				tokens[end].1,
				*next_node_id,
			);
			let mut diagnostics = parsed
				.diagnostics
				.into_iter()
				.map(|diagnostic| diagnostic.with_note("while parsing compile-time source expansion"));
			if let Some(diagnostic) = diagnostics.next() {
				self.deferred_diagnostics.extend(diagnostics);
				return Err(diagnostic);
			}
			*next_node_id =
				max_expr_node_id(&parsed.tree).map_or(*next_node_id, |id| id.saturating_add(1));
			let value = self.eval(&parsed.tree, &mut Env::new())?;
			let replacement = self.require_tokens(value, span)?;
			tokens.splice(index..=end, replacement);
			if tokens.len() > OUTPUT_TOKEN_LIMIT {
				return Err(self.error(span, "compile-time token output limit exceeded"));
			}
		}
		Ok(())
	}

	fn eval(&mut self, expression: &Expr, env: &mut Env) -> Result<Value, Diagnostic> {
		self.step(expression.span)?;
		let value = match &expression.kind {
			ExprKind::Int(value) => Value::Int(value.0 as i128),
			ExprKind::UInt(value) => Value::UInt(value.0),
			ExprKind::Float(value) => Value::Float(value.0.into_inner()),
			ExprKind::Char(value) => Value::Char(value.0),
			ExprKind::Boolean(value) => Value::Boolean(value.0),
			ExprKind::String(parts) => {
				let mut result = EcoString::new();
				for part in parts {
					match &part.0 {
						StringPart::Text(text) => result.push_str(text),
						StringPart::EscapeSequence(escape) => {
							let Some(character) = escape.to_char() else {
								return Err(self.error(part.1, "invalid string escape in const evaluation"));
							};
							result.push(character);
						}
						StringPart::InterpolatedExpr(value) => {
							result.push_str(&self.eval(value, env)?.display());
						}
					}
				}
				Value::String(result)
			}
			ExprKind::Identifier(name) => {
				if let Some(value) = env.get(&name.0) {
					value.clone()
				} else if self.resolve_const_name(&name.0).is_some() {
					self.eval_const_let(&name.0, name.1)?
				} else {
					return Err(self.error(name.1, format!("`{}` is not a const value", name.0)));
				}
			}
			ExprKind::This => env.get("this").cloned().ok_or_else(|| {
				self.error(
					expression.span,
					"`this` is unavailable in this const context",
				)
			})?,
			ExprKind::TokenLiteral(literal) => Value::Tokens(self.eval_token_literal(literal, env)?),
			ExprKind::Expansion(value) | ExprKind::Grouped(value) => self.eval(value, env)?,
			ExprKind::List(items) => Value::List(self.eval_items(items, env)?),
			ExprKind::Tuple(items) => Value::Tuple(self.eval_items(items, env)?),
			ExprKind::Map(entries) => {
				let mut values = Vec::new();
				for entry in entries {
					match &entry.0 {
						MapEntry::Entry(key, value) => {
							values.push((self.eval(key, env)?, self.eval(value, env)?));
						}
						MapEntry::Spread(value) => {
							let Value::Map(spread) = self.eval(value, env)? else {
								return Err(self.error(entry.1, "const map spread requires a map"));
							};
							values.extend(spread);
						}
					}
				}
				Value::Map(values)
			}
			ExprKind::Range(range) => Value::List(self.eval_range(range, env, expression.span)?),
			ExprKind::IndexAccess { parent, index, .. } => {
				let parent = self.eval(parent, env)?;
				let index = self.eval(index, env)?;
				match (parent, index) {
					(Value::List(values) | Value::Tuple(values), index) => {
						let index = value_uint(&index)
							.and_then(|index| usize::try_from(index).ok())
							.ok_or_else(|| self.error(expression.span, "const collection index must be uint"))?;
						values.get(index).cloned().ok_or_else(|| {
							self.error(expression.span, "const collection index is out of bounds")
						})?
					}
					(Value::String(value), index) => {
						let index = value_uint(&index)
							.and_then(|index| usize::try_from(index).ok())
							.ok_or_else(|| self.error(expression.span, "const string index must be uint"))?;
						Value::Char(
							value.chars().nth(index).ok_or_else(|| {
								self.error(expression.span, "const string index is out of bounds")
							})?,
						)
					}
					(Value::Map(values), key) => values
						.into_iter()
						.rev()
						.find_map(|(candidate, value)| (candidate == key).then_some(value))
						.ok_or_else(|| self.error(expression.span, "const map key is absent"))?,
					_ => return Err(self.error(expression.span, "value is not const-indexable")),
				}
			}
			ExprKind::Closure { params, body, .. } => Value::Closure {
				params: params.clone(),
				body: body.clone(),
				env: env.clone(),
			},
			ExprKind::Call { .. } => self.eval_call(expression, env, None)?,
			ExprKind::MemberAccess { parent, member, .. } => {
				if let Some(path) = expression_path(expression)
					&& path.contains("Token.")
					&& let Some(token) = meta_unit_token(&member.0)
				{
					return Ok(Value::Variant {
						name: member.0.clone(),
						value: Box::new(Value::Void),
						tokens: vec![Spanned(token, member.1)],
					});
				}
				if let Some(path) = expression_path(expression) {
					let is_unit_variant = matches!(
						path.as_str(),
						"meta.SyntaxContext.Source"
							| "SyntaxContext.Source"
							| "meta.Visibility.Public"
							| "Visibility.Public"
							| "meta.Visibility.Internal"
							| "Visibility.Internal"
							| "meta.Visibility.Private"
							| "Visibility.Private"
							| "meta.ImportRoot.Project"
							| "ImportRoot.Project"
							| "meta.ImportRoot.Current"
							| "ImportRoot.Current"
							| "meta.ImportRoot.Parent"
							| "ImportRoot.Parent"
					) || path.ends_with(".None");
					if is_unit_variant {
						return Ok(meta_variant(&member.0, Value::Void));
					}
					if !path.starts_with("meta.") && member.0.chars().next().is_some_and(char::is_uppercase) {
						let owner = path
							.rsplit_once('.')
							.map_or(path.as_str(), |(owner, _)| owner)
							.rsplit('.')
							.next()
							.unwrap_or(path.as_str());
						return Ok(meta_variant(
							&member.0,
							Value::Record {
								owner: Some(self.owner.clone()),
								name: owner.into(),
								fields: BTreeMap::new(),
								tokens: Vec::new(),
							},
						));
					}
				}
				let parent = self.eval(parent, env)?;
				let Value::Record { fields, .. } = parent else {
					return Err(self.error(
						expression.span,
						"const member access requires a meta record",
					));
				};
				fields
					.get(&member.0)
					.cloned()
					.ok_or_else(|| self.error(member.1, format!("meta value has no `{}` field", member.0)))?
			}
			ExprKind::PrefixOp { op, value } => {
				let value = self.eval(value, env)?;
				self.eval_prefix(*op, value, expression.span)?
			}
			ExprKind::BinaryOp { lhs, op, rhs } => {
				let lhs = self.eval(lhs, env)?;
				if *op == BinaryOperator::BoolAnd && lhs == Value::Boolean(false) {
					Value::Boolean(false)
				} else if *op == BinaryOperator::BoolOr && lhs == Value::Boolean(true) {
					Value::Boolean(true)
				} else {
					let rhs = self.eval(rhs, env)?;
					self.eval_binary(lhs, *op, rhs, expression.span)?
				}
			}
			ExprKind::If {
				condition,
				then,
				otherwise,
			} => match self.eval(condition, env)? {
				Value::Boolean(true) => self.eval(then, env)?,
				Value::Boolean(false) => match otherwise {
					Some(otherwise) => self.eval(otherwise, env)?,
					None => Value::Void,
				},
				_ => return Err(self.error(condition.span, "const condition must be boolean")),
			},
			ExprKind::Match { value, arms } => {
				let value = self.eval(value, env)?;
				let mut matched = None;
				for arm in arms {
					let mut arm_env = env.clone();
					if !self.bind_pattern(&arm.pattern.0, &value, &mut arm_env)? {
						continue;
					}
					let guard_matches = match &arm.guard {
						None => true,
						Some(guard) => match self.eval(guard, &mut arm_env)? {
							Value::Boolean(value) => value,
							_ => return Err(self.error(guard.span, "const match guard must be boolean")),
						},
					};
					if guard_matches {
						matched = Some(self.eval(&arm.body, &mut arm_env)?);
						break;
					}
				}
				matched.ok_or_else(|| self.error(expression.span, "const match has no matching arm"))?
			}
			ExprKind::Break { value, label } => Value::Break(
				value
					.as_ref()
					.map(|value| self.eval(value, env).map(Box::new))
					.transpose()?,
				label.as_ref().map(|label| label.0.clone()),
			),
			ExprKind::Continue {
				label,
				replacements,
			} => {
				let mut values = Vec::new();
				for replacement in replacements {
					values.push((
						replacement.name.0.clone(),
						self.eval(&replacement.value, env)?,
					));
				}
				Value::Continue(values, label.as_ref().map(|label| label.0.clone()))
			}
			ExprKind::For {
				variable,
				iterable,
				body,
				label,
			} => {
				let iterable = self.eval(iterable, env)?;
				let values = match iterable {
					Value::List(values) | Value::Tuple(values) => values,
					Value::String(value) => value.chars().map(Value::Char).collect(),
					_ => {
						return Err(self.error(
							expression.span,
							"const for-loop requires a finite collection",
						));
					}
				};
				let loop_label = label.as_ref().map(|label| &label.0);
				let mut result = Value::Void;
				for value in values {
					let mut iteration = env.clone();
					if !self.bind_pattern(&variable.0, &value, &mut iteration)? {
						return Err(self.error(variable.1, "const for-loop pattern did not match"));
					}
					match self.eval(body, &mut iteration)? {
						Value::Break(value, target) if target.as_ref() == loop_label => {
							result = meta_variant("Some", value.map_or(Value::Void, |value| *value));
							break;
						}
						Value::Continue(_, target) if target.as_ref() == loop_label => {}
						flow @ (Value::Break(..) | Value::Continue(..)) => return Ok(flow),
						_ => {}
					}
				}
				result
			}
			ExprKind::StateLoop {
				bindings,
				body,
				label,
			} => {
				let mut state = env.clone();
				for binding in bindings {
					let value = self.eval(&binding.value, &mut state)?;
					let Some(name) = binding.meta.name.0.as_binding() else {
						return Err(self.error(
							binding.meta.name.1,
							"const loop state must use named bindings",
						));
					};
					state.insert(name.0.clone(), value);
				}
				let loop_label = label.as_ref().map(|label| &label.0);
				loop {
					match self.eval(body, &mut state)? {
						Value::Break(value, target) if target.as_ref() == loop_label => {
							break value.map_or(Value::Void, |value| *value);
						}
						Value::Continue(replacements, target) if target.as_ref() == loop_label => {
							for (name, value) in replacements {
								if !state.contains_key(&name) {
									return Err(self.error(expression.span, format!("unknown loop state `{name}`")));
								}
								state.insert(name, value);
							}
						}
						flow @ (Value::Break(..) | Value::Continue(..)) => return Ok(flow),
						_ => {}
					}
				}
			}
			ExprKind::Block { body, .. } => {
				let mut local = env.clone();
				let mut result = Value::Void;
				for statement in body {
					match &statement.0 {
						Statement::Expr(value) => {
							result = self.eval(value, &mut local)?;
							if matches!(result, Value::Break(..) | Value::Continue(..)) {
								break;
							}
						}
						Statement::Let { meta, value } => {
							let value = self.eval(value, &mut local)?;
							let Some(name) = meta.name.0.as_binding() else {
								return Err(self.error(
									meta.name.1,
									"const evaluator currently requires a named binding",
								));
							};
							local.insert(name.0.clone(), value);
							result = Value::Void;
						}
					}
				}
				result
			}
			_ => {
				return Err(self.error(
					expression.span,
					"this pure Nymph expression is not supported by const evaluation yet",
				));
			}
		};
		self.allocate(expression.span)?;
		Ok(value)
	}

	fn eval_call(
		&mut self,
		call: &Expr,
		env: &mut Env,
		target: Option<AttachedTarget>,
	) -> Result<Value, Diagnostic> {
		let ExprKind::Call { func, args, .. } = &call.kind else {
			return Err(self.error(call.span, "an attached macro must use normal call syntax"));
		};
		if let Some(path) = expression_path(func)
			&& path.ends_with(".parse")
			&& let Some(value) = self.eval_meta_parse(&path, args, env, call.span)?
		{
			return Ok(value);
		}
		if let Some(path) = expression_path(func)
			&& let Some(value) = self.eval_meta_constructor(&path, args, env, call.span)?
		{
			return Ok(value);
		}
		if let ExprKind::MemberAccess { parent, member, .. } = &func.kind
			&& member.0 == "tokens"
			&& args.is_empty()
		{
			return match self.eval(parent, env)? {
				Value::Record { tokens, .. } | Value::Variant { tokens, .. } => Ok(Value::Tokens(tokens)),
				_ => Err(self.error(parent.span, "`.tokens()` requires a std/meta construct")),
			};
		}
		if let Some(path) = expression_path(func)
			&& matches!(
				path.as_str(),
				"meta.Name.fresh" | "meta.Name.exposed" | "Name.fresh" | "Name.exposed"
			) {
			if target.is_some() || args.len() != 1 {
				return Err(self.error(call.span, "name construction expects one string argument"));
			}
			let value = self.eval(args[0].0.value(), env)?;
			let Value::String(name) = value else {
				return Err(self.error(args[0].1, "name construction expects a string"));
			};
			let context = if path.ends_with(".fresh") {
				let index = self.fresh_names;
				self.fresh_names += 1;
				SyntaxContext::Fresh {
					origin: self.current_origin,
					index: index as u32,
				}
			} else {
				SyntaxContext::Exposed(self.current_origin)
			};
			return Ok(Value::Name(
				name,
				call
					.span
					.with_context(context)
					.with_origin(self.current_origin),
			));
		}
		let is_named_const =
			expression_path(func).is_some_and(|path| self.resolve_const_path(&path).is_some());
		if !is_named_const && let ExprKind::MemberAccess { parent, member, .. } = &func.kind {
			let receiver = self.eval(parent, env)?;
			let mut values = Vec::new();
			for argument in args {
				match &argument.0 {
					CallArg::Value { value, .. } => values.push(self.eval(value, env)?),
					CallArg::Spread { value } => {
						let spread = self.eval(value, env)?;
						values.extend(self.iterable_values(spread, value.span)?);
					}
				}
			}
			if let Some(value) = self.eval_method(receiver, &member.0, None, values, call.span)? {
				return Ok(value);
			}
		}
		if let ExprKind::Identifier(name) = &func.kind
			&& let Some(Value::Closure {
				params,
				body,
				env: closure_env,
			}) = env.get(&name.0).cloned()
		{
			let mut values = Vec::new();
			for argument in args {
				match &argument.0 {
					CallArg::Value { value, .. } => values.push(self.eval(value, env)?),
					CallArg::Spread { value } => match self.eval(value, env)? {
						Value::List(spread) | Value::Tuple(spread) => values.extend(spread),
						_ => return Err(self.error(argument.1, "const call spread requires a collection")),
					},
				}
			}
			let mut closure_env = closure_env;
			let mut values = values.into_iter();
			for parameter in &params {
				let value = if parameter.0.spread {
					Value::List(values.by_ref().collect())
				} else {
					values
						.next()
						.ok_or_else(|| self.error(call.span, "not enough const closure arguments"))?
				};
				if !self.bind_pattern(&parameter.0.name.0, &value, &mut closure_env)? {
					return Err(self.error(parameter.1, "const closure argument pattern did not match"));
				}
			}
			if values.next().is_some() {
				return Err(self.error(call.span, "too many const closure arguments"));
			}
			return self.eval(&body, &mut closure_env);
		}
		let Some(path) = expression_path(func) else {
			return Err(self.error(
				func.span,
				"const calls currently require a directly named function",
			));
		};
		let Some(key) = self.resolve_const_path(&path) else {
			return Err(self.error(func.span, format!("`{path}` is not a const function")));
		};
		let Some(function) = self.functions.get(&key).cloned() else {
			return Err(self.error(func.span, format!("`{path}` is not a const function")));
		};
		let ConstFunction {
			owner,
			path: definition_path,
			meta,
			body,
		} = function;
		let target_type = target
			.as_ref()
			.map(|target| self.check_attached_target(&meta, target, call.span))
			.transpose()?;

		let mut values = Vec::new();
		if let Some(target) = target {
			let target_type = target_type.expect("target type checked");
			values.push(match (target_type, target) {
				(
					AttachedTargetType::Declaration,
					AttachedTarget {
						kind,
						tokens,
						declaration,
					},
				) => self.meta_declaration(kind, tokens, &declaration, true),
				(
					AttachedTargetType::Narrow,
					AttachedTarget {
						kind,
						tokens,
						declaration,
					},
				) => self.meta_declaration(kind, tokens, &declaration, false),
			});
		}
		for argument in args {
			match &argument.0 {
				CallArg::Value { value, .. } => values.push(self.eval(value, env)?),
				CallArg::Spread { value } => match self.eval(value, env)? {
					Value::List(items) | Value::Tuple(items) => values.extend(items),
					_ => return Err(self.error(value.span, "a spread const argument must be a collection")),
				},
			}
		}
		if values.len() != meta.params.len() {
			return Err(self.error(
				call.span,
				format!(
					"const function `{}` expects {} arguments but received {}",
					meta.name.0,
					meta.params.len(),
					values.len()
				),
			));
		}

		self.budget.depth += 1;
		if self.budget.depth > CALL_DEPTH_LIMIT {
			return Err(self.limit(call.span, "call depth", CALL_DEPTH_LIMIT));
		}
		let parent_origin = self.current_origin;
		let parent_definition = self.current_definition;
		let parent_owner = std::mem::replace(&mut self.owner, owner);
		let definition = meta.name.1;
		let origin = self.intern_origin(call.span, definition, parent_origin);
		self.current_origin = origin;
		self.current_definition = definition_context(&definition_path, &meta);
		self.trace.push((path.into(), call.span));
		let mut function_env = Env::new();
		for (parameter, value) in meta.params.iter().zip(values) {
			let Some(name) = parameter.0.name.0.as_binding() else {
				return Err(self.error(
					parameter.1,
					"const parameters currently require named bindings",
				));
			};
			function_env.insert(name.0.clone(), value);
		}
		let result = self.eval(&body, &mut function_env);
		self.trace.pop();
		self.owner = parent_owner;
		self.current_origin = parent_origin;
		self.current_definition = parent_definition;
		self.budget.depth -= 1;
		result
	}

	fn eval_meta_parse(
		&mut self,
		path: &str,
		args: &[Spanned<CallArg>],
		env: &mut Env,
		span: Span,
	) -> Result<Option<Value>, Diagnostic> {
		let Some(type_name) = path
			.strip_suffix(".parse")
			.and_then(|path| path.rsplit('.').next())
		else {
			return Ok(None);
		};
		if !matches!(
			type_name,
			"Tokens"
				| "Declaration"
				| "Function"
				| "Let"
				| "Struct"
				| "Enum"
				| "Interface"
				| "Implementation"
				| "Namespace"
				| "Import"
				| "Effect"
				| "TypeAlias"
				| "ExternalDeclaration"
				| "Expression"
				| "Statement"
				| "Pattern"
				| "Type"
				| "Parameter"
				| "Field"
				| "Variant"
				| "MatchArm"
		) {
			return Ok(None);
		}
		if args.len() != 1 {
			return Err(self.error(span, "std/meta parse expects one token value"));
		}
		let value = self.eval(args[0].0.value(), env)?;
		let tokens = self.require_tokens(value, args[0].1)?;
		if type_name == "Tokens" {
			return Ok(Some(Value::Tokens(tokens)));
		}
		if type_name == "Expression" {
			let end = tokens.last().map_or(span.end, |token| token.1.end);
			let parsed = nymph_syntax::parse_expression_tokens_from(
				&tokens,
				Span::new(end, end).with_origin(self.current_origin),
				0,
			);
			if let Some(diagnostic) = parsed.diagnostics.into_iter().next() {
				return Err(diagnostic.with_note("while parsing `meta.Expression`"));
			}
			return Ok(Some(meta_syntax("Expression", parsed.tree.span, &tokens)));
		}
		if let Some((prefix, suffix)) = match type_name {
			"Type" => Some(("type __Meta = ", "")),
			"Pattern" => Some(("let ", " = 0")),
			"Statement" => Some(("func __meta(): void = { ", " }")),
			"Parameter" => Some(("func __meta(", ") = #()")),
			"Field" => Some(("struct __Meta(", ")")),
			"Variant" => Some(("enum __Meta { ", " }")),
			"MatchArm" => Some(("func __meta(value: int): int = match (value) { ", " }")),
			_ => None,
		} {
			let parsed = parse_wrapped_meta_syntax(prefix, &tokens, suffix, span, &self.module.path);
			if let Some(diagnostic) = parsed.diagnostics.into_iter().next() {
				return Err(diagnostic.with_note(format!("while parsing `meta.{type_name}`")));
			}
			return Ok(Some(meta_syntax(
				type_name,
				declaration_tokens_span(&tokens),
				&tokens,
			)));
		}
		let end = tokens.last().map_or(span.end, |token| token.1.end);
		let parsed = nymph_syntax::parse_module_tokens(
			&tokens,
			Span::new(end, end).with_origin(self.current_origin),
			self.module.path.clone(),
		);
		if let Some(diagnostic) = parsed.diagnostics.into_iter().next() {
			return Err(diagnostic.with_note(format!("while parsing `meta.{type_name}`")));
		}
		if parsed.tree.members.len() != 1 {
			return Err(self.error(
				span,
				"std/meta declaration parse requires exactly one declaration",
			));
		}
		let declaration = &parsed.tree.members[0];
		let Some(kind) = declaration_kind(declaration) else {
			return Err(self.error(span, "std/meta parse requires a concrete declaration"));
		};
		let expected = if type_name == "Declaration" {
			"Declaration"
		} else {
			type_name
		};
		if expected != "Declaration" && expected != format!("{kind:?}") {
			return Err(self.error(
				span,
				format!("`meta.{type_name}.parse` received a {kind:?} declaration"),
			));
		}
		Ok(Some(self.meta_declaration(
			kind,
			tokens,
			declaration,
			expected == "Declaration",
		)))
	}

	fn eval_meta_constructor(
		&mut self,
		path: &str,
		args: &[Spanned<CallArg>],
		env: &mut Env,
		span: Span,
	) -> Result<Option<Value>, Diagnostic> {
		let name = path.rsplit('.').next().unwrap_or(path);
		if matches!(name, "Ok" | "Error" | "Some") {
			let field = if name == "Error" { "error" } else { "value" };
			let fields = self.eval_constructor_fields(args, &[field], env, span)?;
			let value = fields
				.get(field)
				.cloned()
				.expect("constructor fields were checked");
			return Ok(Some(Value::Variant {
				name: name.into(),
				value: Box::new(value),
				tokens: Vec::new(),
			}));
		}
		if path.contains("SyntaxContext.") {
			let fields = match name {
				"Definition" => self.eval_constructor_fields(args, &["id"], env, span)?,
				"CallSite" | "Exposed" => self.eval_constructor_fields(args, &["origin"], env, span)?,
				"Fresh" => self.eval_constructor_fields(args, &["origin", "index"], env, span)?,
				_ => return Ok(None),
			};
			let value = match name {
				"Definition" => fields.get("id").cloned().expect("field checked"),
				"CallSite" | "Exposed" => fields.get("origin").cloned().expect("field checked"),
				"Fresh" => Value::Tuple(vec![
					fields.get("origin").cloned().expect("field checked"),
					fields.get("index").cloned().expect("field checked"),
				]),
				_ => unreachable!(),
			};
			return Ok(Some(meta_variant(name, value)));
		}
		if path.contains("Declaration.") && !path.contains("ExternalDeclaration.") {
			let fields = self.eval_constructor_fields(args, &["value"], env, span)?;
			let value = fields.get("value").cloned().expect("field checked");
			if name == "External"
				&& let Value::Variant { tokens, .. } = &value
			{
				return Ok(Some(Value::Variant {
					name: "External".into(),
					value: Box::new(value.clone()),
					tokens: tokens.clone(),
				}));
			}
			let Value::Record {
				name: record_name,
				tokens,
				..
			} = &value
			else {
				return Err(self.error(span, "a `meta.Declaration` variant requires a meta record"));
			};
			if record_name.as_str() != name
				&& !(name == "External" && record_name == "ExternalDeclaration")
			{
				return Err(self.error(
					span,
					format!("`meta.Declaration.{name}` cannot wrap `meta.{record_name}`"),
				));
			}
			let tokens = tokens.clone();
			return Ok(Some(Value::Variant {
				name: name.into(),
				value: Box::new(value),
				tokens,
			}));
		}
		if path.contains("ExternalDeclaration.") {
			let fields = self.eval_constructor_fields(args, &["value"], env, span)?;
			let value = fields.get("value").cloned().expect("field checked");
			let Value::Record {
				name: record_name,
				tokens,
				..
			} = &value
			else {
				return Err(self.error(
					span,
					"an external declaration variant requires a meta record",
				));
			};
			let expected = if name == "Function" {
				"ExternalFunction"
			} else {
				"ExternalLet"
			};
			if record_name != expected {
				return Err(self.error(span, format!("expected `meta.{expected}`")));
			}
			let tokens = tokens.clone();
			return Ok(Some(Value::Variant {
				name: name.into(),
				value: Box::new(value),
				tokens,
			}));
		}
		if matches!(path, "meta.ImportRoot.Package" | "ImportRoot.Package") {
			let fields = self.eval_constructor_fields(args, &["name"], env, span)?;
			return Ok(Some(Value::Variant {
				name: "Package".into(),
				value: Box::new(fields.get("name").cloned().expect("field checked")),
				tokens: Vec::new(),
			}));
		}
		if matches!(path, "meta.Diagnostic" | "Diagnostic") {
			let fields = self.eval_constructor_fields(args, &["message", "span"], env, span)?;
			return Ok(Some(Value::Record {
				owner: None,
				name: "Diagnostic".into(),
				fields,
				tokens: Vec::new(),
			}));
		}
		if matches!(path, "meta.Name" | "Name") {
			let fields = self.eval_constructor_fields(args, &["text", "span"], env, span)?;
			let Some(Value::String(text)) = fields.get("text") else {
				return Err(self.error(span, "`meta.Name.text` must be a string"));
			};
			let Some(name_span) = fields.get("span").and_then(value_span) else {
				return Err(self.error(span, "`meta.Name.span` must be a `meta.Span`"));
			};
			return Ok(Some(Value::Name(text.clone(), name_span)));
		}
		if matches!(path, "meta.Span" | "Span") {
			let fields =
				self.eval_constructor_fields(args, &["start", "end", "context", "origin"], env, span)?;
			let Some(value) = value_span(&Value::Record {
				owner: None,
				name: "Span".into(),
				fields: fields.clone(),
				tokens: Vec::new(),
			}) else {
				return Err(self.error(span, "invalid `meta.Span` fields"));
			};
			return Ok(Some(meta_span(value)));
		}
		if matches!(path, "meta.Tokens" | "Tokens") {
			let fields = self.eval_constructor_fields(args, &["items"], env, span)?;
			let Some(Value::List(items)) = fields.get("items") else {
				return Err(self.error(span, "`meta.Tokens.items` must be a list"));
			};
			let mut tokens = Vec::new();
			for item in items {
				tokens.extend(self.token_convert(item.clone(), span)?);
			}
			return Ok(Some(Value::Tokens(tokens)));
		}
		if path.contains("Token.") {
			let token_name = path.rsplit('.').next().unwrap_or(path);
			let fields = self.eval_constructor_fields(args, &["value"], env, span)?;
			let value = fields.get("value").expect("constructor field checked");
			let token = match (token_name, value) {
				("Int", Value::UInt(value)) => Token::Int(*value),
				("Int", Value::Int(value)) if *value >= 0 => Token::Int(*value as u64),
				("UInt", Value::UInt(value)) => Token::UInt(*value),
				("Float", Value::Float(value)) => Token::Float((*value).into()),
				("Char", Value::Char(value)) => Token::Char(*value),
				("String", Value::String(value)) => {
					Token::Str(vec![Spanned(StrFragment::Text(value.clone()), span)])
				}
				("Identifier", Value::String(value)) => Token::Identifier(value.clone()),
				("AnonymousParam", Value::Variant { name, .. }) if name == "None" => {
					Token::AnonymousParam(None)
				}
				("AnonymousParam", Value::Variant { name, value, .. }) if name == "Some" => {
					Token::AnonymousParam(Some(
						value_uint(value)
							.and_then(|value| value.try_into().ok())
							.ok_or_else(|| self.error(span, "anonymous parameter index exceeds 255"))?,
					))
				}
				_ => return Err(self.error(span, format!("invalid `meta.Token.{token_name}` value"))),
			};
			return Ok(Some(Value::Variant {
				name: token_name.into(),
				value: Box::new(value.clone()),
				tokens: vec![Spanned(token, span)],
			}));
		}
		if matches!(
			path,
			"meta.Expression"
				| "Expression"
				| "meta.Statement"
				| "Statement"
				| "meta.Pattern"
				| "Pattern"
				| "meta.Type"
				| "Type"
		) {
			let fields = self.eval_constructor_fields(args, &["tokens", "span"], env, span)?;
			let Some(Value::Tokens(tokens)) = fields.get("tokens") else {
				return Err(self.error(span, "std/meta syntax tokens must be `meta.Tokens`"));
			};
			let tokens = tokens.clone();
			return Ok(Some(Value::Record {
				owner: None,
				name: name.into(),
				fields,
				tokens,
			}));
		}
		let record_fields: Option<&[&str]> = match name {
			"Parameter" => Some(&["name", "pattern", "type_", "spread", "span"]),
			"Field" => Some(&["name", "type_", "default", "span"]),
			"Variant" => Some(&["name", "fields", "span"]),
			"ImportName" => Some(&["source", "alias"]),
			"Let" => Some(&[
				"visibility",
				"name",
				"pattern",
				"type_",
				"value",
				"is_const",
				"span",
			]),
			"Struct" => Some(&["visibility", "name", "fields", "members", "span"]),
			"Enum" => Some(&["visibility", "name", "variants", "members", "span"]),
			"Interface" => Some(&["visibility", "name", "members", "span"]),
			"Implementation" => Some(&[
				"visibility",
				"target",
				"implemented_interface",
				"members",
				"span",
			]),
			"Import" => Some(&["root", "path", "alias", "names", "span"]),
			"ExternalFunction" => Some(&[
				"visibility",
				"name",
				"external_name",
				"is_async",
				"parameters",
				"return_type",
				"span",
			]),
			"ExternalLet" => Some(&[
				"visibility",
				"name",
				"external_name",
				"pattern",
				"type_",
				"span",
			]),
			"Namespace" => Some(&["visibility", "name", "members", "span"]),
			"Effect" => Some(&["visibility", "name", "span"]),
			"TypeAlias" => Some(&["visibility", "name", "value", "span"]),
			_ => None,
		};
		if let Some(record_fields) = record_fields {
			let fields = self.eval_constructor_fields(args, record_fields, env, span)?;
			let tokens = self.meta_record_tokens(name, &fields, span)?;
			return Ok(Some(Value::Record {
				owner: None,
				name: name.into(),
				fields,
				tokens,
			}));
		}
		if matches!(path, "meta.Function" | "Function") {
			let fields = self.eval_constructor_fields(
				args,
				&[
					"visibility",
					"name",
					"is_const",
					"is_async",
					"parameters",
					"return_type",
					"body",
					"span",
				],
				env,
				span,
			)?;
			let tokens = self.function_record_tokens(&fields, span)?;
			return Ok(Some(Value::Record {
				owner: None,
				name: "Function".into(),
				fields,
				tokens,
			}));
		}
		if !path.starts_with("meta.") && name.chars().next().is_some_and(char::is_uppercase) {
			let mut fields = BTreeMap::new();
			for (index, argument) in args.iter().enumerate() {
				let CallArg::Value { name, value } = &argument.0 else {
					return Err(self.error(argument.1, "constructor fields cannot be spread"));
				};
				let field = name
					.as_ref()
					.map_or_else(|| format!("_{index}").into(), |name| name.0.clone());
				fields.insert(field, self.eval(value, env)?);
			}
			if args.len() == 1
				&& !fields.contains_key("value")
				&& let Some(value) = fields.remove("_0")
			{
				fields.insert("value".into(), value);
			}
			let record_name = path
				.rsplit_once('.')
				.map_or(name, |(owner, _)| owner.rsplit('.').next().unwrap_or(owner));
			let record = Value::Record {
				owner: Some(self.owner.clone()),
				name: record_name.into(),
				fields,
				tokens: Vec::new(),
			};
			return Ok(Some(if path.contains('.') {
				meta_variant(name, record)
			} else {
				record
			}));
		}
		Ok(None)
	}

	fn meta_record_tokens(
		&mut self,
		name: &str,
		fields: &BTreeMap<EcoString, Value>,
		fallback: Span,
	) -> Result<Vec<Spanned<Token>>, Diagnostic> {
		let span = fields.get("span").and_then(value_span).unwrap_or(fallback);
		let fixed = |token| Spanned(token, span);
		let trace = self.trace.clone();
		let required = |field: &str| {
			fields.get(field).cloned().ok_or_else(|| {
				meta_error_with_trace(span, format!("`meta.{name}.{field}` is required"), &trace).with_help(
					format!("supply the `{field}` field when constructing `meta.{name}`"),
				)
			})
		};
		let comma_list = |this: &mut Self,
		                  values: &[Value],
		                  tokens: &mut Vec<Spanned<Token>>|
		 -> Result<(), Diagnostic> {
			for (index, value) in values.iter().enumerate() {
				if index != 0 {
					tokens.push(fixed(Token::Comma));
				}
				tokens.extend(this.token_convert(value.clone(), span)?);
			}
			Ok(())
		};
		let mut tokens = Vec::new();
		match name {
			"Parameter" => {
				if matches!(fields.get("spread"), Some(Value::Boolean(true))) {
					tokens.push(fixed(Token::DotDotDot));
				}
				tokens.extend(self.token_convert(required("pattern")?, span)?);
				tokens.push(fixed(Token::Colon));
				tokens.extend(self.token_convert(required("type_")?, span)?);
			}
			"Field" => {
				tokens.extend(self.token_convert(required("name")?, span)?);
				tokens.push(fixed(Token::Colon));
				tokens.extend(self.token_convert(required("type_")?, span)?);
				if let Some(default) = option_value(fields.get("default")) {
					tokens.push(fixed(Token::Eq));
					tokens.extend(self.token_convert(default.clone(), span)?);
				}
			}
			"Variant" => {
				tokens.extend(self.token_convert(required("name")?, span)?);
				let Some(Value::List(values)) = fields.get("fields") else {
					return Err(self.error(span, "`meta.Variant.fields` must be a list"));
				};
				if !values.is_empty() {
					tokens.push(fixed(Token::LParen));
					comma_list(self, values, &mut tokens)?;
					tokens.push(fixed(Token::RParen));
				}
			}
			"ImportName" => {
				tokens.extend(self.token_convert(required("source")?, span)?);
				if let Some(alias) = option_value(fields.get("alias")) {
					tokens.push(fixed(Token::As));
					tokens.extend(self.token_convert(alias.clone(), span)?);
				}
			}
			"Let" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				if matches!(fields.get("is_const"), Some(Value::Boolean(true))) {
					tokens.push(fixed(Token::Const));
				}
				tokens.push(fixed(Token::Let));
				tokens.extend(self.token_convert(required("pattern")?, span)?);
				if let Some(type_) = option_value(fields.get("type_")) {
					tokens.push(fixed(Token::Colon));
					tokens.extend(self.token_convert(type_.clone(), span)?);
				}
				tokens.push(fixed(Token::Eq));
				tokens.extend(self.token_convert(required("value")?, span)?);
			}
			"Struct" | "Enum" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(if name == "Struct" {
					Token::Struct
				} else {
					Token::Enum
				}));
				tokens.extend(self.token_convert(required("name")?, span)?);
				let list_field = if name == "Struct" {
					"fields"
				} else {
					"variants"
				};
				let Some(Value::List(values)) = fields.get(list_field) else {
					return Err(self.error(span, format!("`meta.{name}.{list_field}` must be a list")));
				};
				if name == "Struct" {
					tokens.push(fixed(Token::LParen));
					comma_list(self, values, &mut tokens)?;
					tokens.push(fixed(Token::RParen));
				} else {
					tokens.push(fixed(Token::LBrace));
					comma_list(self, values, &mut tokens)?;
				}
				let Some(Value::List(members)) = fields.get("members") else {
					return Err(self.error(span, format!("`meta.{name}.members` must be a list")));
				};
				if !members.is_empty() {
					if name == "Struct" {
						tokens.push(fixed(Token::LBrace));
					}
					for member in members {
						tokens.extend(self.token_convert(member.clone(), span)?);
					}
					if name == "Struct" {
						tokens.push(fixed(Token::RBrace));
					}
				}
				if name == "Enum" {
					tokens.push(fixed(Token::RBrace));
				}
			}
			"Namespace" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(Token::Namespace));
				tokens.extend(self.token_convert(required("name")?, span)?);
				tokens.push(fixed(Token::LBrace));
				let Some(Value::List(members)) = fields.get("members") else {
					return Err(self.error(span, "`meta.Namespace.members` must be a list"));
				};
				for member in members {
					tokens.extend(self.token_convert(member.clone(), span)?);
				}
				tokens.push(fixed(Token::RBrace));
			}
			"Interface" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(Token::Interface));
				tokens.extend(self.token_convert(required("name")?, span)?);
				tokens.push(fixed(Token::LBrace));
				let Some(Value::List(members)) = fields.get("members") else {
					return Err(self.error(span, "`meta.Interface.members` must be a list"));
				};
				for member in members {
					tokens.extend(self.token_convert(member.clone(), span)?);
				}
				tokens.push(fixed(Token::RBrace));
			}
			"Implementation" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(Token::Impl));
				if let Some(interface) = option_value(fields.get("implemented_interface")) {
					tokens.extend(self.token_convert(interface.clone(), span)?);
					tokens.push(fixed(Token::For));
				}
				tokens.extend(self.token_convert(required("target")?, span)?);
				tokens.push(fixed(Token::LBrace));
				let Some(Value::List(members)) = fields.get("members") else {
					return Err(self.error(span, "`meta.Implementation.members` must be a list"));
				};
				for member in members {
					tokens.extend(self.token_convert(member.clone(), span)?);
				}
				tokens.push(fixed(Token::RBrace));
			}
			"Import" => {
				tokens.push(fixed(Token::Import));
				match fields.get("root") {
					Some(Value::Variant { name, value, .. }) if name == "Package" => {
						tokens.extend(self.token_convert(value.as_ref().clone(), span)?);
					}
					Some(Value::Variant { name, .. }) if name == "Project" => {
						tokens.push(fixed(Token::At));
					}
					Some(Value::Variant { name, .. }) if name == "Current" => {
						tokens.push(fixed(Token::Dot));
					}
					Some(Value::Variant { name, .. }) if name == "Parent" => {
						tokens.push(fixed(Token::DotDot));
					}
					_ => return Err(self.error(span, "invalid `meta.Import.root`")),
				}
				tokens.push(fixed(Token::Slash));
				let Some(Value::List(path)) = fields.get("path") else {
					return Err(self.error(span, "`meta.Import.path` must be a list"));
				};
				for (index, part) in path.iter().enumerate() {
					if index != 0 {
						tokens.push(fixed(Token::Slash));
					}
					tokens.extend(self.token_convert(part.clone(), span)?);
				}
				if let Some(alias) = option_value(fields.get("alias")) {
					tokens.push(fixed(Token::As));
					tokens.extend(self.token_convert(alias.clone(), span)?);
				}
				let Some(Value::List(names)) = fields.get("names") else {
					return Err(self.error(span, "`meta.Import.names` must be a list"));
				};
				if !names.is_empty() {
					tokens.push(fixed(Token::With));
					tokens.push(fixed(Token::LParen));
					comma_list(self, names, &mut tokens)?;
					tokens.push(fixed(Token::RParen));
				}
			}
			"ExternalFunction" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(Token::External));
				tokens.push(fixed(Token::LParen));
				tokens.extend(self.token_convert(required("external_name")?, span)?);
				tokens.push(fixed(Token::RParen));
				if matches!(fields.get("is_async"), Some(Value::Boolean(true))) {
					tokens.push(fixed(Token::Async));
				}
				tokens.push(fixed(Token::Func));
				tokens.extend(self.token_convert(required("name")?, span)?);
				tokens.push(fixed(Token::LParen));
				let Some(Value::List(parameters)) = fields.get("parameters") else {
					return Err(self.error(span, "`meta.ExternalFunction.parameters` must be a list"));
				};
				comma_list(self, parameters, &mut tokens)?;
				tokens.push(fixed(Token::RParen));
				if let Some(return_type) = option_value(fields.get("return_type")) {
					tokens.push(fixed(Token::Colon));
					tokens.extend(self.token_convert(return_type.clone(), span)?);
				}
			}
			"ExternalLet" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(Token::External));
				tokens.push(fixed(Token::LParen));
				tokens.extend(self.token_convert(required("external_name")?, span)?);
				tokens.push(fixed(Token::RParen));
				tokens.push(fixed(Token::Let));
				tokens.extend(self.token_convert(required("pattern")?, span)?);
				if let Some(type_) = option_value(fields.get("type_")) {
					tokens.push(fixed(Token::Colon));
					tokens.extend(self.token_convert(type_.clone(), span)?);
				}
			}
			"Effect" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(Token::Effect));
				tokens.extend(self.token_convert(required("name")?, span)?);
			}
			"TypeAlias" => {
				self.push_meta_visibility(fields, &mut tokens, span, name)?;
				tokens.push(fixed(Token::Type));
				tokens.extend(self.token_convert(required("name")?, span)?);
				tokens.push(fixed(Token::Eq));
				tokens.extend(self.token_convert(required("value")?, span)?);
			}
			_ => return Err(self.error(span, format!("cannot serialize `meta.{name}`"))),
		}
		Ok(tokens)
	}

	fn push_meta_visibility(
		&self,
		fields: &BTreeMap<EcoString, Value>,
		tokens: &mut Vec<Spanned<Token>>,
		span: Span,
		name: &str,
	) -> Result<(), Diagnostic> {
		match option_value(fields.get("visibility")) {
			Some(Value::Variant { name, .. }) if name == "Public" => {
				tokens.push(Spanned(Token::Public, span));
			}
			Some(Value::Variant { name, .. }) if name == "Internal" => {
				tokens.push(Spanned(Token::Internal, span));
			}
			Some(Value::Variant { name, .. }) if name == "Private" => {
				tokens.push(Spanned(Token::Private, span));
			}
			Some(_) => return Err(self.error(span, format!("invalid `meta.{name}.visibility`"))),
			None => {}
		}
		Ok(())
	}

	fn function_record_tokens(
		&mut self,
		fields: &BTreeMap<EcoString, Value>,
		fallback: Span,
	) -> Result<Vec<Spanned<Token>>, Diagnostic> {
		let span = fields.get("span").and_then(value_span).unwrap_or(fallback);
		let fixed = |token| Spanned(token, span);
		let mut tokens = Vec::new();
		self.push_meta_visibility(fields, &mut tokens, span, "Function")?;
		if matches!(fields.get("is_async"), Some(Value::Boolean(true))) {
			tokens.push(fixed(Token::Async));
		}
		if matches!(fields.get("is_const"), Some(Value::Boolean(true))) {
			tokens.push(fixed(Token::Const));
		}
		tokens.push(fixed(Token::Func));
		let Some(name @ Value::Name(..)) = fields.get("name") else {
			return Err(self.error(span, "`meta.Function.name` must be a `meta.Name`"));
		};
		tokens.extend(self.token_convert(name.clone(), span)?);
		tokens.push(fixed(Token::LParen));
		let Some(Value::List(parameters)) = fields.get("parameters") else {
			return Err(self.error(span, "`meta.Function.parameters` must be a list"));
		};
		for (index, parameter) in parameters.iter().enumerate() {
			if index != 0 {
				tokens.push(fixed(Token::Comma));
			}
			tokens.extend(self.token_convert(parameter.clone(), span)?);
		}
		tokens.push(fixed(Token::RParen));
		if let Some(return_type) = option_value(fields.get("return_type")) {
			tokens.push(fixed(Token::Colon));
			tokens.extend(self.token_convert(return_type.clone(), span)?);
		}
		tokens.push(fixed(Token::Eq));
		let Some(body) = fields.get("body") else {
			return Err(self.error(span, "`meta.Function.body` is required"));
		};
		tokens.extend(self.token_convert(body.clone(), span)?);
		Ok(tokens)
	}

	fn eval_constructor_fields(
		&mut self,
		args: &[Spanned<CallArg>],
		names: &[&str],
		env: &mut Env,
		span: Span,
	) -> Result<BTreeMap<EcoString, Value>, Diagnostic> {
		if args.len() != names.len() {
			return Err(self.error(
				span,
				format!(
					"constructor expects {} arguments but received {}",
					names.len(),
					args.len()
				),
			));
		}
		let mut fields = BTreeMap::new();
		for (index, argument) in args.iter().enumerate() {
			let CallArg::Value { name, value } = &argument.0 else {
				return Err(self.error(argument.1, "constructor arguments cannot be spread"));
			};
			let field = name.as_ref().map_or(names[index], |name| name.0.as_str());
			if !names.contains(&field) || fields.contains_key(field) {
				return Err(self.error(
					argument.1,
					format!("unknown or duplicate constructor field `{field}`"),
				));
			}
			fields.insert(field.into(), self.eval(value, env)?);
		}
		if let Some(missing) = names.iter().find(|name| !fields.contains_key(**name)) {
			return Err(self.error(span, format!("constructor is missing field `{missing}`")));
		}
		Ok(fields)
	}

	fn intern_origin(&mut self, invocation: Span, definition: Span, parent: OriginId) -> OriginId {
		let mut id = OriginId(stable_hash(&[
			self.module.path.as_bytes(),
			&invocation.start.to_le_bytes(),
			&invocation.end.to_le_bytes(),
			&definition.start.to_le_bytes(),
			&definition.end.to_le_bytes(),
			&parent.0.to_le_bytes(),
		]));
		while id == OriginId::SOURCE || self.origins.contains_key(&id) {
			id.0 = id.0.wrapping_add(1);
		}
		self.origins.insert(
			id,
			ExpansionOrigin {
				id,
				invocation,
				definition,
				parent,
			},
		);
		id
	}

	fn check_attached_target(
		&self,
		meta: &FuncDeclaration,
		target: &AttachedTarget,
		span: Span,
	) -> Result<AttachedTargetType, Diagnostic> {
		let Some(parameter) = meta.params.first() else {
			return Err(self.error(span, "an attached macro needs an implicit target parameter"));
		};
		let Type::Reference { name, .. } = &parameter.0.type_.0 else {
			return Err(self.error(
				parameter.0.type_.1,
				"an attached macro target must use one std/meta declaration type",
			));
		};
		let expected = name.0.rsplit('.').next().unwrap_or(name.0.as_str());
		let AttachedTarget {
			kind, declaration, ..
		} = target;
		let matches = expected == "Declaration"
			|| matches!(
				(expected, kind),
				("Import", DeclarationKind::Import)
					| ("Let", DeclarationKind::Let)
					| ("Function", DeclarationKind::Function)
					| ("Struct", DeclarationKind::Struct)
					| ("Enum", DeclarationKind::Enum)
					| ("Interface", DeclarationKind::Interface)
					| ("Implementation", DeclarationKind::Implementation)
					| ("Namespace", DeclarationKind::Namespace)
					| ("Effect", DeclarationKind::Effect)
					| ("TypeAlias", DeclarationKind::TypeAlias)
					| ("ExternalDeclaration", DeclarationKind::External)
			);
		if matches {
			Ok(if expected == "Declaration" {
				AttachedTargetType::Declaration
			} else {
				AttachedTargetType::Narrow
			})
		} else {
			Err(
				self
					.error(
						span,
						format!("attached macro expects `meta.{expected}` but the target is {kind:?}"),
					)
					.with_label(Label::new(
						parameter.0.type_.1,
						format!("parameter requires `meta.{expected}`"),
					))
					.with_label(Label::new(
						declaration_span(declaration),
						format!("this target is {kind:?}"),
					))
					.with_help(
						"change the first parameter to the original target's meta type or use `meta.Declaration`",
					),
			)
		}
	}

	fn meta_declaration(
		&self,
		kind: DeclarationKind,
		tokens: Vec<Spanned<Token>>,
		declaration: &Declaration,
		wrap: bool,
	) -> Value {
		let name = format!("{kind:?}");
		let mut fields = BTreeMap::from([
			("kind".into(), Value::String(name.clone().into())),
			("span".into(), meta_span(declaration_tokens_span(&tokens))),
		]);
		let visibility = match declaration {
			Declaration::Let { visibility, .. }
			| Declaration::Func { visibility, .. }
			| Declaration::Struct { visibility, .. }
			| Declaration::Enum { visibility, .. }
			| Declaration::Interface { visibility, .. }
			| Declaration::Impl { visibility, .. }
			| Declaration::ImplFor { visibility, .. }
			| Declaration::Namespace { visibility, .. }
			| Declaration::Effect { visibility, .. }
			| Declaration::TypeAlias { visibility, .. }
			| Declaration::ExternalLet(visibility, ..)
			| Declaration::ExternalFunc(visibility, ..) => visibility.as_ref().map(meta_visibility),
			Declaration::Import { .. } | Declaration::Expansion(_) | Declaration::Attached { .. } => None,
		};
		fields.insert("visibility".into(), meta_option(visibility));
		match declaration {
			Declaration::Func { meta, body, .. } => {
				fields.insert("name".into(), Value::Name(meta.name.0.clone(), meta.name.1));
				fields.insert("is_const".into(), Value::Boolean(meta.is_const));
				fields.insert("is_async".into(), Value::Boolean(meta.is_async));
				fields.insert("body".into(), meta_syntax("Expression", body.span, &tokens));
				fields.insert(
					"return_type".into(),
					meta_option(
						meta
							.return_type
							.as_ref()
							.map(|return_type| meta_syntax("Type", return_type.1, &tokens)),
					),
				);
				fields.insert(
					"parameters".into(),
					Value::List(
						meta
							.params
							.iter()
							.map(|parameter| {
								let parameter_fields = BTreeMap::from([
									("span".into(), meta_span(parameter.1)),
									("spread".into(), Value::Boolean(parameter.0.spread)),
									(
										"type_".into(),
										meta_syntax("Type", parameter.0.type_.1, &tokens),
									),
									(
										"pattern".into(),
										meta_syntax("Pattern", parameter.0.name.1, &tokens),
									),
									(
										"name".into(),
										meta_option(
											parameter
												.0
												.name
												.0
												.as_binding()
												.map(|binding| Value::Name(binding.0.clone(), binding.1)),
										),
									),
								]);
								Value::Record {
									owner: None,
									name: "Parameter".into(),
									fields: parameter_fields,
									tokens: tokens_in_span(&tokens, parameter.1),
								}
							})
							.collect(),
					),
				);
			}
			Declaration::Struct {
				name,
				fields: declaration_fields,
				members,
				impls,
				..
			} => {
				fields.insert("name".into(), Value::Name(name.0.clone(), name.1));
				fields.insert(
					"fields".into(),
					Value::List(
						declaration_fields
							.iter()
							.map(|field| Value::Record {
								owner: None,
								name: "Field".into(),
								fields: BTreeMap::from([
									(
										"name".into(),
										Value::Name(field.0.name.0.clone(), field.0.name.1),
									),
									(
										"type_".into(),
										meta_syntax("Type", field.0.type_.1, &tokens),
									),
									(
										"default".into(),
										meta_option(
											field
												.0
												.default
												.as_ref()
												.map(|default| meta_syntax("Expression", default.span, &tokens)),
										),
									),
									("span".into(), meta_span(field.1)),
								]),
								tokens: tokens_in_span(&tokens, field.1),
							})
							.collect(),
					),
				);
				fields.insert(
					"members".into(),
					Value::List(meta_members(members, impls, &tokens)),
				);
			}
			Declaration::Enum {
				name,
				variants,
				members,
				impls,
				..
			} => {
				fields.insert("name".into(), Value::Name(name.0.clone(), name.1));
				fields.insert(
					"variants".into(),
					Value::List(
						variants
							.iter()
							.map(|variant| Value::Record {
								owner: None,
								name: "Variant".into(),
								fields: BTreeMap::from([
									(
										"name".into(),
										Value::Name(variant.0.name.0.clone(), variant.0.name.1),
									),
									(
										"fields".into(),
										Value::List(
											variant
												.0
												.fields
												.iter()
												.map(|field| meta_field(field, &tokens))
												.collect(),
										),
									),
									("span".into(), meta_span(variant.1)),
								]),
								tokens: tokens_in_span(&tokens, variant.1),
							})
							.collect(),
					),
				);
				fields.insert(
					"members".into(),
					Value::List(meta_members(members, impls, &tokens)),
				);
			}
			Declaration::Let { meta, value, .. } => {
				fields.insert(
					"name".into(),
					meta_option(
						meta
							.name
							.0
							.as_binding()
							.map(|binding| Value::Name(binding.0.clone(), binding.1)),
					),
				);
				fields.insert(
					"pattern".into(),
					meta_syntax("Pattern", meta.name.1, &tokens),
				);
				fields.insert(
					"type_".into(),
					meta_option(
						meta
							.type_
							.as_ref()
							.map(|type_| meta_syntax("Type", type_.1, &tokens)),
					),
				);
				fields.insert("is_const".into(), Value::Boolean(meta.is_const));
				fields.insert(
					"value".into(),
					meta_syntax("Expression", value.span, &tokens),
				);
			}
			Declaration::Effect { name, .. } => {
				fields.insert("name".into(), Value::Name(name.0.clone(), name.1));
			}
			Declaration::Namespace { name, members, .. } => {
				fields.insert("name".into(), Value::Name(name.0.clone(), name.1));
				fields.insert(
					"members".into(),
					Value::List(meta_members(members, &[], &tokens)),
				);
			}
			Declaration::Interface { name, members, .. } => {
				fields.insert("name".into(), Value::Name(name.0.clone(), name.1));
				fields.insert(
					"members".into(),
					Value::List(
						members
							.iter()
							.map(|member| meta_member("Declaration", member.1, &tokens))
							.collect(),
					),
				);
			}
			Declaration::Impl { type_, members, .. } => {
				fields.insert("target".into(), meta_syntax("Type", type_.1, &tokens));
				fields.insert("implemented_interface".into(), meta_option(None));
				fields.insert(
					"members".into(),
					Value::List(meta_members(members, &[], &tokens)),
				);
			}
			Declaration::ImplFor {
				type_,
				for_interface,
				members,
				..
			} => {
				fields.insert("target".into(), meta_syntax("Type", type_.1, &tokens));
				fields.insert(
					"implemented_interface".into(),
					meta_option(Some(meta_syntax("Type", for_interface.0.1, &tokens))),
				);
				fields.insert(
					"members".into(),
					Value::List(meta_members(members, &[], &tokens)),
				);
			}
			Declaration::TypeAlias { meta, value, .. } => {
				fields.insert("name".into(), Value::Name(meta.name.0.clone(), meta.name.1));
				fields.insert("value".into(), meta_syntax("Type", value.1, &tokens));
			}
			Declaration::ExternalLet(_, external_name, meta) => {
				if let Some(binding) = meta.name.0.as_binding() {
					fields.insert("name".into(), Value::Name(binding.0.clone(), binding.1));
				}
				fields.insert("external_name".into(), Value::String(external_name.clone()));
				fields.insert(
					"pattern".into(),
					meta_syntax("Pattern", meta.name.1, &tokens),
				);
				fields.insert(
					"type_".into(),
					meta_option(
						meta
							.type_
							.as_ref()
							.map(|type_| meta_syntax("Type", type_.1, &tokens)),
					),
				);
			}
			Declaration::ExternalFunc(_, external_name, meta) => {
				fields.insert("name".into(), Value::Name(meta.name.0.clone(), meta.name.1));
				fields.insert("external_name".into(), Value::String(external_name.clone()));
				fields.insert("is_async".into(), Value::Boolean(meta.is_async));
				fields.insert(
					"parameters".into(),
					Value::List(
						meta
							.params
							.iter()
							.map(|parameter| meta_parameter(parameter, &tokens))
							.collect(),
					),
				);
				fields.insert(
					"return_type".into(),
					meta_option(
						meta
							.return_type
							.as_ref()
							.map(|type_| meta_syntax("Type", type_.1, &tokens)),
					),
				);
			}
			Declaration::Import {
				root,
				path,
				alias,
				idents,
				..
			} => {
				fields.insert(
					"root".into(),
					match root {
						ImportRoot::Package(name) => Value::Variant {
							name: "Package".into(),
							value: Box::new(Value::Name(name.0.clone(), name.1)),
							tokens: Vec::new(),
						},
						ImportRoot::Project => meta_variant("Project", Value::Void),
						ImportRoot::Current => meta_variant("Current", Value::Void),
						ImportRoot::Parent => meta_variant("Parent", Value::Void),
					},
				);
				fields.insert(
					"path".into(),
					Value::List(
						path
							.iter()
							.map(|part| Value::Name(part.0.clone(), part.1))
							.collect(),
					),
				);
				fields.insert(
					"alias".into(),
					meta_option(
						alias
							.as_ref()
							.map(|alias| Value::Name(alias.0.clone(), alias.1)),
					),
				);
				fields.insert(
					"names".into(),
					Value::List(
						idents
							.iter()
							.flatten()
							.map(|(source, alias)| Value::Record {
								owner: None,
								name: "ImportName".into(),
								fields: BTreeMap::from([
									("source".into(), Value::Name(source.0.clone(), source.1)),
									(
										"alias".into(),
										meta_option(
											alias
												.as_ref()
												.map(|alias| Value::Name(alias.0.clone(), alias.1)),
										),
									),
								]),
								tokens: tokens_in_span(&tokens, source.1),
							})
							.collect(),
					),
				);
			}
			_ => {}
		}
		if kind == DeclarationKind::External {
			let (variant_name, record_name) = match declaration {
				Declaration::ExternalFunc(..) => ("Function", "ExternalFunction"),
				Declaration::ExternalLet(..) => ("Let", "ExternalLet"),
				_ => unreachable!(),
			};
			let external = Value::Variant {
				name: variant_name.into(),
				value: Box::new(Value::Record {
					owner: None,
					name: record_name.into(),
					fields,
					tokens: tokens.clone(),
				}),
				tokens: tokens.clone(),
			};
			return if wrap {
				Value::Variant {
					name: "External".into(),
					value: Box::new(external),
					tokens,
				}
			} else {
				external
			};
		}
		let record = Value::Record {
			owner: None,
			name: name.clone().into(),
			fields,
			tokens: tokens.clone(),
		};
		if wrap {
			Value::Variant {
				name: name.into(),
				value: Box::new(record),
				tokens,
			}
		} else {
			record
		}
	}

	fn bind_pattern(
		&self,
		pattern: &Pattern,
		value: &Value,
		env: &mut Env,
	) -> Result<bool, Diagnostic> {
		match pattern {
			Pattern::Placeholder => Ok(true),
			Pattern::Binding { name, inner } => {
				if !self.bind_pattern(&inner.0, value, env)? {
					return Ok(false);
				}
				env.insert(name.0.clone(), value.clone());
				Ok(true)
			}
			Pattern::Boolean(expected) => Ok(value == &Value::Boolean(expected.0)),
			Pattern::Int(expected) => Ok(value == &Value::Int(i128::from(expected.0))),
			Pattern::UInt(expected) => Ok(value == &Value::UInt(expected.0)),
			Pattern::Char(expected) => Ok(value == &Value::Char(expected.0)),
			Pattern::Struct { path, fields } => {
				let expected = path.last().map_or("", |part| part.0.as_str());
				let empty_fields = BTreeMap::new();
				let (actual, fields_value, variant_value) = match value {
					Value::Record { name, fields, .. } => (name.as_str(), fields, None),
					Value::Variant { name, value, .. } => {
						let fields = match value.as_ref() {
							Value::Record { fields, .. } => fields,
							_ => &empty_fields,
						};
						(name.as_str(), fields, Some(value.as_ref()))
					}
					_ => return Ok(false),
				};
				if actual != expected {
					return Ok(false);
				}
				for field in fields {
					match &field.0 {
						StructPatternField::Value { name, value } => {
							let field_value = if name.0 == "value" {
								variant_value.or_else(|| fields_value.get(&name.0))
							} else {
								fields_value.get(&name.0)
							};
							let Some(field_value) = field_value else {
								return Ok(false);
							};
							if !self.bind_pattern(&value.0, field_value, env)? {
								return Ok(false);
							}
						}
						StructPatternField::Named(name) => {
							let Some(field_value) = fields_value.get(&name.0) else {
								return Ok(false);
							};
							env.insert(name.0.clone(), field_value.clone());
						}
						StructPatternField::Positional(value) => {
							let Some(field_value) = variant_value.or_else(|| fields_value.get("value")) else {
								return Ok(false);
							};
							if !self.bind_pattern(&value.0, field_value, env)? {
								return Ok(false);
							}
						}
						StructPatternField::Rest => {}
					}
				}
				Ok(true)
			}
			Pattern::Grouped(inner) => self.bind_pattern(&inner.0, value, env),
			Pattern::Union(left, right) => {
				let original = env.clone();
				if self.bind_pattern(&left.0, value, env)? {
					Ok(true)
				} else {
					*env = original;
					self.bind_pattern(&right.0, value, env)
				}
			}
			_ => Err(self.error(
				Span::new(0, 0),
				"this pattern is not supported in const matching yet",
			)),
		}
	}

	fn eval_token_literal(
		&mut self,
		literal: &nymph_ast::expr::TokenLiteral,
		env: &mut Env,
	) -> Result<Vec<Spanned<Token>>, Diagnostic> {
		let mut tokens = Vec::new();
		for piece in &literal.pieces {
			match &piece.0 {
				TokenLiteralPiece::Token(token) => {
					tokens.push(Spanned(token.clone(), self.literal_span(piece.1)))
				}
				TokenLiteralPiece::Interpolation {
					value,
					splice,
					separator,
				} => {
					let value = self.eval(value, env)?;
					if *splice {
						let values = self.iterable_values(value, piece.1)?;
						for (index, value) in values.into_iter().enumerate() {
							if index != 0
								&& let Some(separator) = separator
							{
								tokens.push(Spanned(separator.0.clone(), self.literal_span(separator.1)));
							}
							tokens.extend(self.token_convert(value, piece.1)?);
						}
					} else {
						tokens.extend(self.token_convert(value, piece.1)?);
					}
				}
			}
			if tokens.len() > OUTPUT_TOKEN_LIMIT {
				return Err(self.error(piece.1, "compile-time token output limit exceeded"));
			}
		}
		Ok(tokens)
	}

	fn literal_span(&self, span: Span) -> Span {
		if self.current_definition == 0 {
			span.with_origin(self.current_origin)
		} else {
			span
				.with_context(SyntaxContext::Definition(self.current_definition))
				.with_origin(self.current_origin)
		}
	}

	fn eval_items(
		&mut self,
		items: &[Spanned<ListItem>],
		env: &mut Env,
	) -> Result<Vec<Value>, Diagnostic> {
		let mut values = Vec::new();
		for item in items {
			match &item.0 {
				ListItem::Expr(value) => values.push(self.eval(value, env)?),
				ListItem::Spread(value) => match self.eval(value, env)? {
					Value::List(spread) | Value::Tuple(spread) => values.extend(spread),
					_ => return Err(self.error(item.1, "const spread requires a collection")),
				},
			}
		}
		Ok(values)
	}

	fn eval_range(
		&mut self,
		range: &RangeKind,
		env: &mut Env,
		span: Span,
	) -> Result<Vec<Value>, Diagnostic> {
		let (min, max, inclusive) = match range {
			RangeKind::Exclusive { min, max } => (min, max, false),
			RangeKind::Inclusive { min, max } => (min, max, true),
			RangeKind::From(_) | RangeKind::To(_) | RangeKind::ToInclusive(_) => {
				return Err(self.error(
					span,
					"an unbounded range cannot be materialized at compile time",
				));
			}
		};
		let (Value::Int(mut current), Value::Int(end)) = (self.eval(min, env)?, self.eval(max, env)?)
		else {
			return Err(self.error(span, "const ranges currently require signed integer bounds"));
		};
		let mut values = Vec::new();
		while current < end || (inclusive && current == end) {
			self.step(span)?;
			if values.len() >= VALUE_LIMIT as usize {
				return Err(self.error(span, "compile-time range value limit exceeded"));
			}
			values.push(Value::Int(current));
			current = current
				.checked_add(1)
				.ok_or_else(|| self.error(span, "const range bound overflowed"))?;
		}
		Ok(values)
	}

	fn token_convert(&mut self, value: Value, span: Span) -> Result<Vec<Spanned<Token>>, Diagnostic> {
		let token = |token| vec![Spanned(token, span)];
		match value {
			Value::Tokens(tokens) => Ok(
				tokens
					.into_iter()
					.map(|Spanned(token, token_span)| {
						Spanned(
							token,
							if token_span.origin == OriginId::SOURCE {
								token_span.with_origin(self.current_origin)
							} else {
								token_span
							},
						)
					})
					.collect(),
			),
			Value::Record { name, tokens, .. } | Value::Variant { name, tokens, .. }
				if !tokens.is_empty() || meta_token_convertible_type(&name) =>
			{
				Ok(
					tokens
						.into_iter()
						.map(|Spanned(token, token_span)| {
							Spanned(
								token,
								if token_span.origin == OriginId::SOURCE {
									token_span.with_origin(self.current_origin)
								} else {
									token_span
								},
							)
						})
						.collect(),
				)
			}
			Value::Name(name, name_span) => Ok(vec![Spanned(Token::Identifier(name), name_span)]),
			Value::Int(value) if value >= 0 && value <= u64::MAX as i128 => {
				Ok(token(Token::Int(value as u64)))
			}
			Value::UInt(value) => Ok(token(Token::UInt(value))),
			Value::Float(value) => Ok(token(Token::Float(value.into()))),
			Value::Char(value) => Ok(token(Token::Char(value))),
			Value::Boolean(true) => Ok(token(Token::True)),
			Value::Boolean(false) => Ok(token(Token::False)),
			Value::String(value) => Ok(token(Token::Identifier(value))),
			value => {
				if let Some(converted) = self.eval_method(value, "into", Some("Into"), Vec::new(), span)? {
					self.token_convert(converted, span)
				} else {
					Err(self
						.error(span, "value does not implement `Into<meta.Tokens>`")
						.with_help(
							"implement `Into<meta.Tokens>` for this type or convert it to a std/meta syntax value before expansion",
						))
				}
			}
		}
	}

	fn require_tokens(
		&mut self,
		value: Value,
		span: Span,
	) -> Result<Vec<Spanned<Token>>, Diagnostic> {
		match value {
			Value::Variant { name, value, .. } if name == "Ok" => self.require_tokens(*value, span),
			Value::Variant { name, value, .. } if name == "Error" => {
				let mut diagnostics = self.returned_diagnostics(*value, span).into_iter();
				let first = diagnostics
					.next()
					.expect("returned diagnostics always contains one item");
				self.deferred_diagnostics.extend(diagnostics);
				Err(first)
			}
			Value::List(values) | Value::Tuple(values) => {
				let mut tokens = Vec::new();
				for value in values {
					tokens.extend(self.token_convert(value, span)?);
				}
				Ok(tokens)
			}
			value
				if self.value_has_method(&value, "iter", "Iterable")
					|| self.value_has_method(&value, "next", "Iterator") =>
			{
				let mut tokens = Vec::new();
				for item in self.iterable_values(value, span)? {
					tokens.extend(self.token_convert(item, span)?);
				}
				Ok(tokens)
			}
			value => self.token_convert(value, span),
		}
	}

	fn value_has_method(&self, value: &Value, method: &str, interface: &str) -> bool {
		let owner = value_type_owner(value);
		value_type_name(value).is_some_and(|target| {
			self
				.methods
				.get(&(target, method.into()))
				.is_some_and(|methods| {
					methods.iter().any(|candidate| {
						candidate.interface.as_deref() == Some(interface)
							&& owner.is_none_or(|owner| candidate.owner.as_str() == owner.as_str())
					})
				})
		})
	}

	fn iterable_values(&mut self, value: Value, span: Span) -> Result<Vec<Value>, Diagnostic> {
		match value {
			Value::List(values) | Value::Tuple(values) => Ok(values),
			Value::String(value) => Ok(value.chars().map(Value::Char).collect()),
			value => {
				let iterator = self
					.eval_method(value.clone(), "iter", Some("Iterable"), Vec::new(), span)?
					.unwrap_or(value);
				let mut iterator = iterator;
				let mut values = Vec::new();
				loop {
					self.step(span)?;
					let Some(next) =
						self.eval_method(iterator, "next", Some("Iterator"), Vec::new(), span)?
					else {
						return Err(
							self
								.error(
									span,
									"token splice requires an `Iterable` or `Iterator` value",
								)
								.with_help(
									"implement `Iterable` or `Iterator`, and make each item implement `Into<meta.Tokens>`",
								),
						);
					};
					match next {
						Value::Variant { name, .. } if name == "Done" => break,
						Value::Variant { name, value, .. } if name == "Yield" => {
							let Value::Record { fields, .. } = *value else {
								return Err(
									self.error(span, "iterator returned an invalid `Iteration.Yield` value"),
								);
							};
							let Some(item) = fields.get("item").cloned() else {
								return Err(self.error(span, "iterator yield is missing its `item`"));
							};
							let Some(next) = fields.get("next").cloned() else {
								return Err(self.error(span, "iterator yield is missing its `next` state"));
							};
							values.push(item);
							if values.len() >= VALUE_LIMIT as usize {
								return Err(self.limit(span, "iterated values", VALUE_LIMIT));
							}
							iterator = next;
						}
						_ => {
							return Err(
								self
									.error(
										span,
										"iterator `next()` must return `Iteration.Done` or `Iteration.Yield`",
									)
									.with_help(
										"return a valid immutable `Iteration` step from the iterator implementation",
									),
							);
						}
					}
				}
				Ok(values)
			}
		}
	}

	fn eval_method(
		&mut self,
		receiver: Value,
		method: &str,
		interface: Option<&str>,
		values: Vec<Value>,
		span: Span,
	) -> Result<Option<Value>, Diagnostic> {
		let Some(target) = value_type_name(&receiver) else {
			return Ok(None);
		};
		let Some(candidates) = self.methods.get(&(target.clone(), method.into())) else {
			return Ok(None);
		};
		let receiver_owner = value_type_owner(&receiver);
		let candidate = candidates
			.iter()
			.filter(|candidate| {
				interface.is_none_or(|required| candidate.interface.as_deref() == Some(required))
					&& receiver_owner.is_none_or(|owner| candidate.owner.as_str() == owner.as_str())
			})
			.min_by_key(|candidate| (candidate.owner != self.owner, candidate.owner.clone()))
			.cloned();
		let Some(ConstMethod {
			owner,
			path,
			meta,
			body,
			..
		}) = candidate
		else {
			return Ok(None);
		};
		if values.len() != meta.params.len() {
			return Err(
				self
					.error(
						span,
						format!(
							"const method `{target}.{method}` expects {} arguments but received {}",
							meta.params.len(),
							values.len()
						),
					)
					.with_help("pass the arguments required by the const-compatible method"),
			);
		}
		self.budget.depth += 1;
		if self.budget.depth > CALL_DEPTH_LIMIT {
			return Err(self.limit(span, "call depth", CALL_DEPTH_LIMIT));
		}
		let parent_origin = self.current_origin;
		let parent_definition = self.current_definition;
		let parent_owner = std::mem::replace(&mut self.owner, owner);
		let definition = meta.name.1;
		self.current_origin = self.intern_origin(span, definition, parent_origin);
		self.current_definition = definition_context(&path, &meta);
		self.trace.push((format!("{target}.{method}").into(), span));
		let mut method_env = Env::from_iter([("this".into(), receiver)]);
		for (parameter, value) in meta.params.iter().zip(values) {
			let Some(name) = parameter.0.name.0.as_binding() else {
				return Err(self.error(
					parameter.1,
					"const method parameters require named bindings",
				));
			};
			method_env.insert(name.0.clone(), value);
		}
		let result = self.eval(&body, &mut method_env);
		self.trace.pop();
		self.owner = parent_owner;
		self.current_origin = parent_origin;
		self.current_definition = parent_definition;
		self.budget.depth -= 1;
		result.map(Some)
	}

	fn returned_diagnostics(&self, value: Value, fallback: Span) -> Vec<Diagnostic> {
		let values = match value {
			Value::List(values) | Value::Tuple(values) if !values.is_empty() => values,
			value => vec![value],
		};
		values
			.into_iter()
			.map(|value| {
				let Value::Record { name, fields, .. } = value else {
					return self.error(
						fallback,
						"attached macro returned an invalid diagnostic value",
					);
				};
				if name != "Diagnostic" {
					return self.error(
						fallback,
						"attached macro returned an invalid diagnostic value",
					);
				}
				let message = match fields.get("message") {
					Some(Value::String(message)) => message.clone(),
					_ => "attached macro returned a diagnostic".into(),
				};
				let span = fields.get("span").and_then(value_span).unwrap_or(fallback);
				self.error(span, message)
			})
			.collect()
	}

	fn eval_prefix(&self, op: PrefixOperator, value: Value, span: Span) -> Result<Value, Diagnostic> {
		match (op, value) {
			(PrefixOperator::BoolNot, Value::Boolean(value)) => Ok(Value::Boolean(!value)),
			(PrefixOperator::Negate, Value::Int(value)) => Ok(Value::Int(-value)),
			(PrefixOperator::BitNot, Value::Int(value)) => Ok(Value::Int(!value)),
			_ => Err(self.error(span, "invalid const prefix operation")),
		}
	}

	fn eval_binary(
		&self,
		lhs: Value,
		op: BinaryOperator,
		rhs: Value,
		span: Span,
	) -> Result<Value, Diagnostic> {
		use BinaryOperator as Op;
		match (lhs, op, rhs) {
			(Value::Int(a), Op::Plus, Value::Int(b)) => Ok(Value::Int(a + b)),
			(Value::Int(a), Op::Minus, Value::Int(b)) => Ok(Value::Int(a - b)),
			(Value::Int(a), Op::Times, Value::Int(b)) => Ok(Value::Int(a * b)),
			(Value::Int(_), Op::Divide | Op::Remainder, Value::Int(0)) => {
				Err(self.error(span, "division by zero in const evaluation"))
			}
			(Value::Int(a), Op::Divide, Value::Int(b)) => Ok(Value::Int(a / b)),
			(Value::Int(a), Op::Remainder, Value::Int(b)) => Ok(Value::Int(a % b)),
			(Value::Int(a), Op::Equals, Value::Int(b)) => Ok(Value::Boolean(a == b)),
			(Value::Int(a), Op::NotEquals, Value::Int(b)) => Ok(Value::Boolean(a != b)),
			(Value::Int(a), Op::LessThan, Value::Int(b)) => Ok(Value::Boolean(a < b)),
			(Value::Int(a), Op::LessThanEquals, Value::Int(b)) => Ok(Value::Boolean(a <= b)),
			(Value::Int(a), Op::GreaterThan, Value::Int(b)) => Ok(Value::Boolean(a > b)),
			(Value::Int(a), Op::GreaterThanEquals, Value::Int(b)) => Ok(Value::Boolean(a >= b)),
			(Value::Boolean(a), Op::Equals, Value::Boolean(b)) => Ok(Value::Boolean(a == b)),
			(Value::Boolean(a), Op::NotEquals, Value::Boolean(b)) => Ok(Value::Boolean(a != b)),
			(Value::Boolean(a), Op::BoolAnd, Value::Boolean(b)) => Ok(Value::Boolean(a && b)),
			(Value::Boolean(a), Op::BoolOr, Value::Boolean(b)) => Ok(Value::Boolean(a || b)),
			(Value::String(mut a), Op::Plus, Value::String(b)) => {
				a.push_str(&b);
				Ok(Value::String(a))
			}
			(a, Op::Equals, b) => Ok(Value::Boolean(a == b)),
			(a, Op::NotEquals, b) => Ok(Value::Boolean(a != b)),
			_ => Err(self.error(span, "invalid const binary operation")),
		}
	}

	fn step(&mut self, span: Span) -> Result<(), Diagnostic> {
		self.budget.steps += 1;
		if self.budget.steps > STEP_LIMIT {
			Err(self.limit(span, "executed steps", STEP_LIMIT))
		} else {
			Ok(())
		}
	}

	fn allocate(&mut self, span: Span) -> Result<(), Diagnostic> {
		self.budget.values += 1;
		if self.budget.values > VALUE_LIMIT {
			Err(self.limit(span, "allocated values", VALUE_LIMIT))
		} else {
			Ok(())
		}
	}

	fn limit(&self, span: Span, resource: &str, limit: impl std::fmt::Display) -> Diagnostic {
		let mut diagnostic = self.error(
			span,
			format!("const evaluation exceeded its {resource} limit ({limit})"),
		);
		if let Some((name, call_span)) = self.trace.last() {
			diagnostic = diagnostic.with_label(Label::new(
				*call_span,
				format!("`{name}` was active when the limit was reached"),
			));
		}
		diagnostic.with_help(
			"make the active const function or macro terminate, or reduce the amount of generated output",
		)
	}

	fn error(&self, span: Span, message: impl Into<EcoString>) -> Diagnostic {
		meta_error_with_trace(span, message, &self.trace)
	}
}

fn meta_error_with_trace(
	span: Span,
	message: impl Into<EcoString>,
	trace: &[(EcoString, Span)],
) -> Diagnostic {
	let message = message.into();
	let mut diagnostic = Diagnostic::error("META001".into(), message.clone(), span)
		.with_help(meta_diagnostic_help(&message));
	for (name, call_span) in trace.iter().rev() {
		diagnostic = diagnostic
			.with_label(Label::new(
				*call_span,
				format!("const function `{name}` was called here"),
			))
			.with_note(format!(
				"while evaluating const function `{name}` at {}..{}",
				call_span.start, call_span.end
			));
	}
	diagnostic
}

fn meta_diagnostic_help(message: &str) -> EcoString {
	if message.contains("not a const function") || message.contains("not a const value") {
		"declare the dependency with `const func` or `const let`, or remove it from compile-time evaluation"
			.into()
	} else if message.contains("Into<meta.Tokens>") || message.contains("converted") {
		"implement `Into<meta.Tokens>` for this value's type or return a std/meta syntax value".into()
	} else if message.contains("limit") {
		"make the active const function or macro terminate, or reduce the amount of generated output"
			.into()
	} else if message.contains("cycle") {
		"break the const/import expansion cycle so every dependency reaches a finite fixed point".into()
	} else if message.contains("attached macro") || message.contains("attachment") {
		"use a compatible std/meta target parameter and return token-convertible output".into()
	} else if message.contains("parse")
		|| message.contains("syntax")
		|| message.contains("unterminated")
	{
		"emit valid Nymph for this grammar position and check delimiter balance in the token output"
			.into()
	} else if message.contains("compile-time-only") {
		"keep std/meta values in `const let` or `const func` declarations and convert generated syntax to runtime Nymph"
			.into()
	} else if message.contains("Iterable")
		|| message.contains("Iterator")
		|| message.contains("splice")
	{
		"implement a finite pure `Iterable` or `Iterator` whose items implement `Into<meta.Tokens>`"
			.into()
	} else {
		"rewrite this const expression using supported pure Nymph operations and compile-time-compatible values"
			.into()
	}
}

impl Value {
	fn display(&self) -> EcoString {
		match self {
			Value::Int(value) => value.to_string().into(),
			Value::UInt(value) => value.to_string().into(),
			Value::Float(value) => value.to_string().into(),
			Value::Char(value) => value.to_string().into(),
			Value::Boolean(value) => value.to_string().into(),
			Value::String(value) | Value::Name(value, _) => value.clone(),
			Value::Void => EcoString::new(),
			Value::List(_)
			| Value::Tuple(_)
			| Value::Map(_)
			| Value::Closure { .. }
			| Value::Tokens(_)
			| Value::Record { .. }
			| Value::Variant { .. }
			| Value::Break(..)
			| Value::Continue(..) => "<compile-time value>".into(),
		}
	}
}

fn declaration_tokens_span(tokens: &[Spanned<Token>]) -> Span {
	match (tokens.first(), tokens.last()) {
		(Some(first), Some(last)) => first.1.to(last.1),
		_ => Span::new(0, 0),
	}
}

fn tokens_in_span(tokens: &[Spanned<Token>], span: Span) -> Vec<Spanned<Token>> {
	tokens
		.iter()
		.filter(|token| token.1.start >= span.start && token.1.end <= span.end)
		.cloned()
		.collect()
}

fn parse_wrapped_meta_syntax(
	prefix: &str,
	tokens: &[Spanned<Token>],
	suffix: &str,
	span: Span,
	path: &EcoString,
) -> nymph_syntax::ParseResult<Module> {
	let wrapper_token = |token: Spanned<Token>| Spanned(token.0, span);
	let mut wrapped = nymph_syntax::lex(prefix)
		.tokens
		.into_iter()
		.map(wrapper_token)
		.collect::<Vec<_>>();
	wrapped.extend_from_slice(tokens);
	wrapped.extend(
		nymph_syntax::lex(suffix)
			.tokens
			.into_iter()
			.map(wrapper_token),
	);
	nymph_syntax::parse_module_tokens(&wrapped, span, path.clone())
}

fn meta_syntax(name: &str, span: Span, tokens: &[Spanned<Token>]) -> Value {
	let syntax_tokens = tokens_in_span(tokens, span);
	Value::Record {
		owner: None,
		name: name.into(),
		fields: BTreeMap::from([("span".into(), meta_span(span))]),
		tokens: syntax_tokens,
	}
}

fn meta_field(field: &Spanned<StructField>, tokens: &[Spanned<Token>]) -> Value {
	Value::Record {
		owner: None,
		name: "Field".into(),
		fields: BTreeMap::from([
			(
				"name".into(),
				Value::Name(field.0.name.0.clone(), field.0.name.1),
			),
			("type_".into(), meta_syntax("Type", field.0.type_.1, tokens)),
			(
				"default".into(),
				meta_option(
					field
						.0
						.default
						.as_ref()
						.map(|default| meta_syntax("Expression", default.span, tokens)),
				),
			),
			("span".into(), meta_span(field.1)),
		]),
		tokens: tokens_in_span(tokens, field.1),
	}
}

fn meta_parameter(parameter: &Spanned<FuncParam>, tokens: &[Spanned<Token>]) -> Value {
	Value::Record {
		owner: None,
		name: "Parameter".into(),
		fields: BTreeMap::from([
			("span".into(), meta_span(parameter.1)),
			("spread".into(), Value::Boolean(parameter.0.spread)),
			(
				"type_".into(),
				meta_syntax("Type", parameter.0.type_.1, tokens),
			),
			(
				"pattern".into(),
				meta_syntax("Pattern", parameter.0.name.1, tokens),
			),
			(
				"name".into(),
				meta_option(
					parameter
						.0
						.name
						.0
						.as_binding()
						.map(|binding| Value::Name(binding.0.clone(), binding.1)),
				),
			),
		]),
		tokens: tokens_in_span(tokens, parameter.1),
	}
}

fn meta_member(name: &str, span: Span, tokens: &[Spanned<Token>]) -> Value {
	let member_tokens = tokens_in_span(tokens, span);
	let record = Value::Record {
		owner: None,
		name: name.into(),
		fields: BTreeMap::from([("span".into(), meta_span(span))]),
		tokens: member_tokens.clone(),
	};
	Value::Variant {
		name: name.into(),
		value: Box::new(record),
		tokens: member_tokens,
	}
}

fn meta_members(
	members: &[Spanned<ImplMember>],
	impls: &[Spanned<StructImpl>],
	tokens: &[Spanned<Token>],
) -> Vec<Value> {
	let mut values = members
		.iter()
		.map(|member| {
			let name = match member.0 {
				ImplMember::Let { .. } => "Let",
				ImplMember::Func { .. } => "Function",
				ImplMember::ExternalLet(..) | ImplMember::ExternalFunc(..) => "External",
			};
			meta_member(name, member.1, tokens)
		})
		.collect::<Vec<_>>();
	values.extend(
		impls
			.iter()
			.map(|implementation| meta_member("Implementation", implementation.1, tokens)),
	);
	values
}

fn meta_span(span: Span) -> Value {
	let context = match span.context {
		SyntaxContext::Source => meta_variant("Source", Value::Void),
		SyntaxContext::Definition(id) => meta_variant("Definition", Value::UInt(id)),
		SyntaxContext::CallSite(origin) => meta_variant("CallSite", Value::UInt(origin.0)),
		SyntaxContext::Fresh { origin, index } => meta_variant(
			"Fresh",
			Value::Tuple(vec![Value::UInt(origin.0), Value::UInt(index.into())]),
		),
		SyntaxContext::Exposed(origin) => meta_variant("Exposed", Value::UInt(origin.0)),
	};
	Value::Record {
		owner: None,
		name: "Span".into(),
		fields: BTreeMap::from([
			("start".into(), Value::UInt(span.start as u64)),
			("end".into(), Value::UInt(span.end as u64)),
			("context".into(), context),
			("origin".into(), Value::UInt(span.origin.0)),
		]),
		tokens: Vec::new(),
	}
}

fn meta_variant(name: &str, value: Value) -> Value {
	Value::Variant {
		name: name.into(),
		value: Box::new(value),
		tokens: Vec::new(),
	}
}

fn meta_option(value: Option<Value>) -> Value {
	match value {
		Some(value) => Value::Variant {
			name: "Some".into(),
			value: Box::new(value),
			tokens: Vec::new(),
		},
		None => Value::Variant {
			name: "None".into(),
			value: Box::new(Value::Void),
			tokens: Vec::new(),
		},
	}
}

fn option_value(value: Option<&Value>) -> Option<&Value> {
	match value {
		Some(Value::Variant { name, value, .. }) if name == "Some" => Some(value),
		Some(Value::Variant { name, .. }) if name == "None" => None,
		_ => None,
	}
}

fn meta_visibility(visibility: &nymph_ast::decl::Visibility) -> Value {
	Value::Variant {
		name: format!("{visibility:?}").into(),
		value: Box::new(Value::Void),
		tokens: Vec::new(),
	}
}

fn value_span(value: &Value) -> Option<Span> {
	let Value::Record { name, fields, .. } = value else {
		return None;
	};
	if name != "Span" {
		return None;
	}
	let (Some(Value::UInt(start)), Some(Value::UInt(end)), Some(context), Some(Value::UInt(origin))) = (
		fields.get("start"),
		fields.get("end"),
		fields.get("context"),
		fields.get("origin"),
	) else {
		return None;
	};
	let context = match context {
		Value::Variant { name, .. } if name == "Source" => SyntaxContext::Source,
		Value::Variant { name, value, .. } if name == "Definition" => {
			SyntaxContext::Definition(value_uint(value)?)
		}
		Value::Variant { name, value, .. } if name == "CallSite" => {
			SyntaxContext::CallSite(OriginId(value_uint(value)?))
		}
		Value::Variant { name, value, .. } if name == "Fresh" => {
			let Value::Tuple(parts) = value.as_ref() else {
				return None;
			};
			let [origin, index] = parts.as_slice() else {
				return None;
			};
			SyntaxContext::Fresh {
				origin: OriginId(value_uint(origin)?),
				index: value_uint(index)?.try_into().ok()?,
			}
		}
		Value::Variant { name, value, .. } if name == "Exposed" => {
			SyntaxContext::Exposed(OriginId(value_uint(value)?))
		}
		_ => return None,
	};
	Some(
		Span::new((*start).try_into().ok()?, (*end).try_into().ok()?)
			.with_context(context)
			.with_origin(OriginId(*origin)),
	)
}

fn value_uint(value: &Value) -> Option<u64> {
	match value {
		Value::UInt(value) => Some(*value),
		Value::Int(value) => (*value).try_into().ok(),
		_ => None,
	}
}

fn meta_unit_token(name: &str) -> Option<Token> {
	macro_rules! token {
		($($variant:ident),+ $(,)?) => {
			match name {
				$(stringify!($variant) => Some(Token::$variant),)+
				_ => None,
			}
		};
	}
	token!(
		True,
		False,
		Public,
		Internal,
		Private,
		Import,
		With,
		Type,
		Struct,
		Enum,
		Let,
		External,
		Effect,
		Const,
		Func,
		Interface,
		Impl,
		Namespace,
		For,
		Loop,
		If,
		Else,
		Match,
		Continue,
		Break,
		Echo,
		This,
		In,
		As,
		Is,
		Async,
		Await,
		IntType,
		UIntType,
		FloatType,
		BooleanType,
		CharType,
		StringType,
		VoidType,
		NeverType,
		SelfType,
		LParen,
		RParen,
		LBracket,
		RBracket,
		LBrace,
		RBrace,
		HashLParen,
		HashLBracket,
		HashLBrace,
		Arrow,
		DotDotDot,
		Question,
		DoubleQuestion,
		QuestionDot,
		Dot,
		At,
		Hash,
		Dollar,
		Backslash,
		Backtick,
		Comma,
		Semicolon,
		Colon,
		ColonColon,
		Underscore,
		PipeArrow,
		Bang,
		Plus,
		Minus,
		Star,
		Slash,
		Percent,
		StarStar,
		Amp,
		Pipe,
		Caret,
		Tilde,
		EqEq,
		BangEq,
		Lt,
		Gt,
		LtEq,
		GtEq,
		BangIn,
		BangIs,
		AmpAmp,
		PipePipe,
		Eq,
		DotDot,
		DotDotEq,
	)
}

fn definition_context(module: &str, function: &FuncDeclaration) -> u64 {
	stable_hash(&[
		module.as_bytes(),
		function.name.0.as_bytes(),
		&function.name.1.start.to_le_bytes(),
	])
}

fn stable_hash(parts: &[&[u8]]) -> u64 {
	let mut hash = 0xcbf2_9ce4_8422_2325_u64;
	for part in parts {
		for byte in *part {
			hash ^= u64::from(*byte);
			hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
		}
		hash ^= 0xff;
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
	}
	hash
}

fn collect_const_declarations(
	module: &Module,
	owner: &EcoString,
	path: &EcoString,
	functions: &mut BTreeMap<(EcoString, EcoString), ConstFunction>,
	methods: &mut BTreeMap<(EcoString, EcoString), Vec<ConstMethod>>,
	const_lets: &mut BTreeMap<(EcoString, EcoString), Expr>,
	public_consts: &mut BTreeSet<(EcoString, EcoString)>,
) {
	for declaration in &module.members {
		match declaration {
			Declaration::Func {
				visibility,
				meta,
				body,
			} if meta.is_const => {
				let key = (owner.clone(), meta.name.0.clone());
				if visibility == &Some(nymph_ast::decl::Visibility::Public) {
					public_consts.insert(key.clone());
				}
				functions.insert(
					key,
					ConstFunction {
						owner: owner.clone(),
						path: path.clone(),
						meta: meta.clone(),
						body: body.clone(),
					},
				);
			}
			Declaration::Let {
				visibility,
				meta,
				value,
			} if meta.is_const => {
				if let Some(name) = meta.name.0.as_binding() {
					let key = (owner.clone(), name.0.clone());
					if visibility == &Some(nymph_ast::decl::Visibility::Public) {
						public_consts.insert(key.clone());
					}
					const_lets.insert(key, value.clone());
				}
			}
			Declaration::Impl { type_, members, .. } | Declaration::ImplFor { type_, members, .. } => {
				let Some(target) = type_name(&type_.0) else {
					continue;
				};
				let interface = match declaration {
					Declaration::ImplFor { for_interface, .. } => Some(
						for_interface
							.0
							.0
							.rsplit('.')
							.next()
							.unwrap_or(for_interface.0.0.as_str())
							.into(),
					),
					_ => None,
				};
				collect_const_methods(owner, path, &target, interface, members, methods);
			}
			Declaration::Struct {
				name,
				members,
				impls,
				..
			}
			| Declaration::Enum {
				name,
				members,
				impls,
				..
			} => {
				collect_const_methods(owner, path, &name.0, None, members, methods);
				for implementation in impls {
					let interface = implementation
						.0
						.interface
						.0
						.0
						.rsplit('.')
						.next()
						.unwrap_or(implementation.0.interface.0.0.as_str())
						.into();
					collect_const_methods(
						owner,
						path,
						&name.0,
						Some(interface),
						&implementation.0.members,
						methods,
					);
				}
			}
			_ => {}
		}
	}
}

fn collect_const_methods(
	owner: &EcoString,
	path: &EcoString,
	target: &EcoString,
	interface: Option<EcoString>,
	members: &[Spanned<ImplMember>],
	methods: &mut BTreeMap<(EcoString, EcoString), Vec<ConstMethod>>,
) {
	for member in members {
		let ImplMember::Func { meta, body, .. } = &member.0 else {
			continue;
		};
		methods
			.entry((target.clone(), meta.name.0.clone()))
			.or_default()
			.push(ConstMethod {
				owner: owner.clone(),
				path: path.clone(),
				interface: interface.clone(),
				meta: meta.clone(),
				body: body.clone(),
			});
	}
}

fn type_name(ty: &Type) -> Option<EcoString> {
	let Type::Reference { name, .. } = ty else {
		return None;
	};
	Some(name.0.rsplit('.').next().unwrap_or(name.0.as_str()).into())
}

fn value_type_name(value: &Value) -> Option<EcoString> {
	match value {
		Value::Record { name, .. } => Some(name.clone()),
		Value::Variant { value, .. } => value_type_name(value),
		_ => None,
	}
}

fn value_type_owner(value: &Value) -> Option<&EcoString> {
	match value {
		Value::Record { owner, .. } => owner.as_ref(),
		Value::Variant { value, .. } => value_type_owner(value),
		_ => None,
	}
}

fn meta_token_convertible_type(name: &str) -> bool {
	matches!(
		name,
		"Declaration"
			| "Function"
			| "Let"
			| "Struct"
			| "Enum"
			| "Interface"
			| "Implementation"
			| "Namespace"
			| "Import"
			| "Effect"
			| "TypeAlias"
			| "External"
			| "ExternalDeclaration"
			| "ExternalFunction"
			| "ExternalLet"
			| "Expression"
			| "Statement"
			| "Pattern"
			| "Type"
			| "Parameter"
			| "Field"
			| "Variant"
			| "MatchArm"
			| "ImportName"
	)
}

fn imported_meta_type_names(module: &Module) -> BTreeSet<EcoString> {
	let mut names = BTreeSet::new();
	for declaration in &module.members {
		let Declaration::Import {
			root: ImportRoot::Package(package),
			path,
			alias,
			idents,
		} = declaration
		else {
			continue;
		};
		if package.0 != "std" || path.len() != 1 || path[0].0 != "meta" {
			continue;
		}
		if let Some(idents) = idents {
			for (source, alias) in idents {
				names.insert(alias.as_ref().unwrap_or(source).0.clone());
			}
		} else {
			names.insert(
				alias
					.as_ref()
					.map_or_else(|| path[0].0.clone(), |alias| alias.0.clone()),
			);
		}
	}
	names
}

fn declaration_kind(declaration: &Declaration) -> Option<DeclarationKind> {
	Some(match declaration {
		Declaration::Import { .. } => DeclarationKind::Import,
		Declaration::Let { .. } => DeclarationKind::Let,
		Declaration::Func { .. } => DeclarationKind::Function,
		Declaration::Struct { .. } => DeclarationKind::Struct,
		Declaration::Enum { .. } => DeclarationKind::Enum,
		Declaration::Interface { .. } => DeclarationKind::Interface,
		Declaration::Impl { .. } | Declaration::ImplFor { .. } => DeclarationKind::Implementation,
		Declaration::Namespace { .. } => DeclarationKind::Namespace,
		Declaration::Effect { .. } => DeclarationKind::Effect,
		Declaration::TypeAlias { .. } => DeclarationKind::TypeAlias,
		Declaration::ExternalLet(..) | Declaration::ExternalFunc(..) => DeclarationKind::External,
		Declaration::Expansion(_) | Declaration::Attached { .. } => return None,
	})
}

fn import_tokens(declaration: &Declaration) -> Vec<Spanned<Token>> {
	let Declaration::Import {
		root,
		path,
		alias,
		idents,
	} = declaration
	else {
		unreachable!("import token rendering requires an import")
	};
	let span = declaration_span(declaration);
	let mut tokens = vec![Spanned(Token::Import, span)];
	match root {
		ImportRoot::Package(package) => {
			tokens.push(Spanned(Token::Identifier(package.0.clone()), package.1))
		}
		ImportRoot::Project => tokens.push(Spanned(Token::At, span)),
		ImportRoot::Current => tokens.push(Spanned(Token::Dot, span)),
		ImportRoot::Parent => tokens.push(Spanned(Token::DotDot, span)),
	}
	for part in path {
		tokens.push(Spanned(Token::Slash, part.1));
		tokens.push(Spanned(Token::Identifier(part.0.clone()), part.1));
	}
	if let Some(alias) = alias {
		tokens.push(Spanned(Token::As, alias.1));
		tokens.push(Spanned(Token::Identifier(alias.0.clone()), alias.1));
	}
	if let Some(idents) = idents {
		tokens.push(Spanned(Token::With, span));
		tokens.push(Spanned(Token::LParen, span));
		for (index, (source, alias)) in idents.iter().enumerate() {
			if index != 0 {
				tokens.push(Spanned(Token::Comma, source.1));
			}
			tokens.push(Spanned(Token::Identifier(source.0.clone()), source.1));
			if let Some(alias) = alias {
				tokens.push(Spanned(Token::As, alias.1));
				tokens.push(Spanned(Token::Identifier(alias.0.clone()), alias.1));
			}
		}
		tokens.push(Spanned(Token::RParen, span));
	}
	tokens
}

pub(crate) fn declaration_span(declaration: &Declaration) -> Span {
	match declaration {
		Declaration::Expansion(value) => value.span,
		Declaration::Attached { macros, target, .. } => macros
			.first()
			.map_or_else(|| declaration_span(target), |call| call.span),
		Declaration::Import {
			root, path, alias, ..
		} => match root {
			ImportRoot::Package(package) => package.1,
			_ => path.first().map_or_else(
				|| alias.as_ref().map_or(Span::new(0, 0), |alias| alias.1),
				|part| part.1,
			),
		},
		Declaration::Let { meta, .. } | Declaration::ExternalLet(_, _, meta) => meta.name.1,
		Declaration::Effect { name, .. }
		| Declaration::Struct { name, .. }
		| Declaration::Enum { name, .. }
		| Declaration::Namespace { name, .. }
		| Declaration::Interface { name, .. } => name.1,
		Declaration::Func { meta, .. } | Declaration::ExternalFunc(_, _, meta) => meta.name.1,
		Declaration::TypeAlias { meta, .. } => meta.name.1,
		Declaration::Impl { type_, .. } | Declaration::ImplFor { type_, .. } => type_.1,
	}
}

fn expression_path(expression: &Expr) -> Option<String> {
	match &expression.kind {
		ExprKind::Identifier(name) => Some(name.0.to_string()),
		ExprKind::MemberAccess { parent, member, .. } => {
			Some(format!("{}.{}", expression_path(parent)?, member.0))
		}
		_ => None,
	}
}

fn next_node_id(module: &Module) -> u32 {
	max_node_id(module).map_or(0, |id| id.saturating_add(1))
}

fn matching_rparen(tokens: &[Spanned<Token>], open: usize) -> Option<usize> {
	let mut depth = 0_u32;
	for (index, token) in tokens.iter().enumerate().skip(open) {
		match token.0 {
			Token::LParen => depth += 1,
			Token::RParen => {
				depth = depth.checked_sub(1)?;
				if depth == 0 {
					return Some(index);
				}
			}
			_ => {}
		}
	}
	None
}

fn max_expr_node_id(expression: &Expr) -> Option<u32> {
	let mut max = None;
	visit_expr(expression, &mut |expression| {
		max = Some(max.map_or(expression.id.0, |current: u32| current.max(expression.id.0)));
	});
	max
}

fn max_node_id(module: &Module) -> Option<u32> {
	let mut max = None;
	for declaration in &module.members {
		for_declaration_exprs(declaration, &mut |expression| {
			visit_expr(expression, &mut |expression| {
				max = Some(max.map_or(expression.id.0, |current: u32| current.max(expression.id.0)));
			});
		});
	}
	max
}

fn visit_expr(expression: &Expr, visitor: &mut impl FnMut(&Expr)) {
	visitor(expression);
	expression.for_each_child(|child| visit_expr(child, visitor));
}

fn for_declaration_exprs(declaration: &Declaration, visitor: &mut impl FnMut(&Expr)) {
	match declaration {
		Declaration::Expansion(value) => visitor(value),
		Declaration::Attached { macros, target, .. } => {
			for call in macros {
				visitor(call);
			}
			for_declaration_exprs(target, visitor);
		}
		Declaration::Let { value, .. } | Declaration::Func { body: value, .. } => visitor(value),
		Declaration::Struct {
			fields,
			members,
			impls,
			..
		} => {
			for field in fields {
				if let Some(default) = &field.0.default {
					visitor(default);
				}
			}
			for member in members {
				for_impl_member_exprs(&member.0, visitor);
			}
			for implementation in impls {
				for member in &implementation.0.members {
					for_impl_member_exprs(&member.0, visitor);
				}
			}
		}
		Declaration::Enum {
			variants,
			members,
			impls,
			..
		} => {
			for variant in variants {
				for field in &variant.0.fields {
					if let Some(default) = &field.0.default {
						visitor(default);
					}
				}
			}
			for member in members {
				for_impl_member_exprs(&member.0, visitor);
			}
			for implementation in impls {
				for member in &implementation.0.members {
					for_impl_member_exprs(&member.0, visitor);
				}
			}
		}
		Declaration::Namespace { members, .. }
		| Declaration::Impl { members, .. }
		| Declaration::ImplFor { members, .. } => {
			for member in members {
				for_impl_member_exprs(&member.0, visitor);
			}
		}
		Declaration::Interface { members, .. } => {
			for member in members {
				match &member.0 {
					nymph_ast::decl::InterfaceMember::Element(element) => match &element.as_ref().0 {
						nymph_ast::decl::InterfaceElement::Let { value, .. } => {
							if let Some(value) = value {
								visitor(value);
							}
						}
						nymph_ast::decl::InterfaceElement::Func { body, .. } => {
							if let Some(body) = body {
								visitor(body);
							}
						}
					},
					nymph_ast::decl::InterfaceMember::Impl { members, .. } => {
						for member in members {
							for_impl_member_exprs(&member.0, visitor);
						}
					}
				}
			}
		}
		Declaration::Import { .. }
		| Declaration::ExternalLet(..)
		| Declaration::ExternalFunc(..)
		| Declaration::Effect { .. }
		| Declaration::TypeAlias { .. } => {}
	}
}

fn for_impl_member_exprs(member: &nymph_ast::decl::ImplMember, visitor: &mut impl FnMut(&Expr)) {
	match member {
		nymph_ast::decl::ImplMember::Let { value, .. }
		| nymph_ast::decl::ImplMember::Func { body: value, .. } => visitor(value),
		nymph_ast::decl::ImplMember::ExternalLet(..)
		| nymph_ast::decl::ImplMember::ExternalFunc(..) => {}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn generated_string_tokens_are_rendered_as_parseable_literals() {
		let token = Token::Str(vec![Spanned(
			StrFragment::Text("line\n\"quoted\" ${literal} \\".into()),
			Span::new(0, 0),
		)]);
		assert_eq!(
			token_text(&token),
			"\"line\\n\\\"quoted\\\" \\${literal} \\\\\""
		)
	}

	#[test]
	fn generated_syntax_keeps_definition_context_and_origin() {
		let parsed = nymph_syntax::parse_module(
			"const func make(): meta.Tokens = \\(func answer(): int = 42)\n$(make())",
			"main.nym",
		);
		assert!(parsed.diagnostics.is_empty());
		let first = expand_module(&parsed.tree);
		let second = expand_module(&parsed.tree);
		assert_eq!(first.origins, second.origins);
		let [Declaration::Func { meta, body, .. }] = first.tree.members.as_slice() else {
			panic!("expected one generated function");
		};
		assert!(matches!(meta.name.1.context, SyntaxContext::Definition(_)));
		assert_ne!(meta.name.1.origin, OriginId::SOURCE);
		assert_eq!(body.id.1, meta.name.1.origin);
		assert!(first.origins.iter().any(|origin| origin.id == body.id.1));
	}

	#[test]
	fn exposed_names_keep_an_explicit_call_site_context() {
		let parsed = nymph_syntax::parse_module(
			"const func make(): meta.Tokens = { let name = meta.Name.exposed(\"answer\") \\(func $(name)(): int = 42) }\n$(make())",
			"main.nym",
		);
		assert!(parsed.diagnostics.is_empty());
		let expanded = expand_module(&parsed.tree);
		let [Declaration::Func { meta, .. }] = expanded.tree.members.as_slice() else {
			panic!("expected one generated function");
		};
		assert!(matches!(meta.name.1.context, SyntaxContext::Exposed(_)));
		assert_ne!(meta.name.1.origin, OriginId::SOURCE);
	}

	#[test]
	fn imported_const_functions_use_their_own_lexical_scope() {
		let parsed = nymph_syntax::parse_module("$(macros.make(41))", "main.nym");
		assert!(parsed.diagnostics.is_empty());
		let imported = ConstModuleSource {
			key: "macros".into(),
			path: "macros.nym".into(),
			source: Arc::from(
				"const func plus_one(value: int): int = value + 1\n\
				 public const func make(value: int): meta.Tokens = \\(func answer(): int = $(plus_one(value)))",
			),
			imports: Vec::new(),
		};
		let root_imports = ConstModuleSource {
			key: "main".into(),
			path: "main.nym".into(),
			source: Arc::from(""),
			imports: vec![ConstImport {
				target: "macros".into(),
				namespace: "macros".into(),
				names: Vec::new(),
			}],
		};
		let expanded =
			expand_module_with_imports(&parsed.tree, "main".into(), &[root_imports, imported]);
		assert!(
			expanded.diagnostics.is_empty(),
			"{:?}",
			expanded.diagnostics
		);
		let [Declaration::Func { body, .. }] = expanded.tree.members.as_slice() else {
			panic!("expected one generated function");
		};
		assert!(matches!(&body.kind, ExprKind::Int(value) if value.0 == 42));
	}
}
