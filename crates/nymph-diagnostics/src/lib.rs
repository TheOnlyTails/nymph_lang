//! A single canonical diagnostic type used across the whole toolchain.
//!
//! Every stage (lexer, parser, checker, codegen) produces [`Diagnostic`]s. The CLI
//! renders them with [`ariadne`] into pretty terminal output; the language server
//! converts them into LSP diagnostics. Keeping one type here means neither backend
//! leaks into the compiler crates.

use ariadne::{Color, Config, IndexType, Label as AriadneLabel, Report, ReportKind, Source};
use ecow::EcoString;
use nymph_ast::Span;

/// Exact compiler source identity used by a migration edit.
///
/// This is semantic identity, not a filesystem path or editor URI. Frontends
/// map it to their own source handles without changing the canonical edit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId {
	project: EcoString,
	module: EcoString,
}

impl SourceId {
	#[must_use]
	pub fn new(project: impl Into<EcoString>, module: impl Into<EcoString>) -> Self {
		Self {
			project: project.into(),
			module: module.into(),
		}
	}

	#[must_use]
	pub fn project(&self) -> &str {
		&self.project
	}

	#[must_use]
	pub fn module(&self) -> &str {
		&self.module
	}
}

/// Version of the exact source text against which an edit was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceVersion(pub i64);

/// The only applicability accepted for automatic migration edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Applicability {
	MachineApplicable,
}

/// One exact byte-span replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextReplacement {
	span: Span,
	replacement: EcoString,
}

impl TextReplacement {
	#[must_use]
	pub fn new(span: Span, replacement: impl Into<EcoString>) -> Self {
		Self {
			span,
			replacement: replacement.into(),
		}
	}

	#[must_use]
	pub fn span(&self) -> Span {
		self.span
	}

	#[must_use]
	pub fn replacement(&self) -> &str {
		&self.replacement
	}
}

/// All replacements for one exact version of one source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEdit {
	source: SourceId,
	version: SourceVersion,
	replacements: Vec<TextReplacement>,
}

impl SourceEdit {
	/// Build one source edit and sort replacements by byte span.
	///
	/// # Errors
	/// Returns an error if the source identity is empty, there are no
	/// replacements, a span is reversed, or replacements overlap.
	pub fn new(
		source: SourceId,
		version: SourceVersion,
		mut replacements: Vec<TextReplacement>,
	) -> Result<Self, EditError> {
		if source.project.is_empty() || source.module.is_empty() {
			return Err(EditError::EmptySourceIdentity { source });
		}
		if replacements.is_empty() {
			return Err(EditError::EmptySourceEdit { source });
		}
		for replacement in &replacements {
			if replacement.span.start > replacement.span.end {
				return Err(EditError::InvalidSpan {
					source,
					span: replacement.span,
				});
			}
		}
		replacements.sort_by_key(|replacement| (replacement.span.start, replacement.span.end));
		for pair in replacements.windows(2) {
			let left = &pair[0];
			let right = &pair[1];
			if left.span.end > right.span.start || left.span.start == right.span.start {
				return Err(EditError::OverlappingReplacements {
					source,
					first: left.span,
					second: right.span,
				});
			}
		}
		Ok(Self {
			source,
			version,
			replacements,
		})
	}

	#[must_use]
	pub fn source(&self) -> &SourceId {
		&self.source
	}

	#[must_use]
	pub fn version(&self) -> SourceVersion {
		self.version
	}

	#[must_use]
	pub fn replacements(&self) -> &[TextReplacement] {
		&self.replacements
	}
}

/// A backend-neutral, all-or-nothing migration edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditGroup {
	title: EcoString,
	applicability: Applicability,
	sources: Vec<SourceEdit>,
}

impl EditGroup {
	/// Build a machine-applicable edit and sort sources by identity.
	///
	/// # Errors
	/// Returns an error if the title or source list is empty, or if the same
	/// source occurs more than once.
	pub fn new(title: impl Into<EcoString>, mut sources: Vec<SourceEdit>) -> Result<Self, EditError> {
		let title = title.into();
		if title.is_empty() {
			return Err(EditError::EmptyTitle);
		}
		if sources.is_empty() {
			return Err(EditError::EmptyGroup);
		}
		sources.sort_by(|left, right| left.source.cmp(&right.source));
		for pair in sources.windows(2) {
			if pair[0].source == pair[1].source {
				return Err(EditError::DuplicateSource {
					source: pair[0].source.clone(),
				});
			}
		}
		Ok(Self {
			title,
			applicability: Applicability::MachineApplicable,
			sources,
		})
	}

	#[must_use]
	pub fn title(&self) -> &str {
		&self.title
	}

	#[must_use]
	pub fn applicability(&self) -> Applicability {
		self.applicability
	}

	#[must_use]
	pub fn sources(&self) -> &[SourceEdit] {
		&self.sources
	}

	/// Validate every source and replacement before any edit is applied.
	///
	/// Extra snapshots are allowed; every source named by the group must occur
	/// exactly once at the expected version.
	///
	/// # Errors
	/// Returns an error for duplicate/missing snapshots, stale versions,
	/// out-of-bounds spans, or spans that split a UTF-8 code point.
	pub fn validate(&self, snapshots: &[SourceSnapshot<'_>]) -> Result<(), EditError> {
		let mut available = std::collections::BTreeMap::new();
		for snapshot in snapshots {
			if available.insert(&snapshot.source, snapshot).is_some() {
				return Err(EditError::DuplicateSnapshot {
					source: snapshot.source.clone(),
				});
			}
		}
		for source in &self.sources {
			let snapshot = available
				.get(&source.source)
				.ok_or_else(|| EditError::MissingSource {
					source: source.source.clone(),
				})?;
			if snapshot.version != source.version {
				return Err(EditError::StaleSource {
					source: source.source.clone(),
					expected: source.version,
					actual: snapshot.version,
				});
			}
			for replacement in &source.replacements {
				if replacement.span.end > snapshot.text.len() {
					return Err(EditError::SpanOutOfBounds {
						source: source.source.clone(),
						span: replacement.span,
						len: snapshot.text.len(),
					});
				}
				if !snapshot.text.is_char_boundary(replacement.span.start)
					|| !snapshot.text.is_char_boundary(replacement.span.end)
				{
					return Err(EditError::InvalidUtf8Boundary {
						source: source.source.clone(),
						span: replacement.span,
					});
				}
			}
		}
		Ok(())
	}

	/// Apply this group to immutable snapshots after validating the whole group.
	///
	/// No partially edited source is returned when any validation fails.
	///
	/// # Errors
	/// Returns the same atomic validation errors as [`Self::validate`].
	pub fn apply(&self, snapshots: &[SourceSnapshot<'_>]) -> Result<Vec<EditedSource>, EditError> {
		self.validate(snapshots)?;
		let available = snapshots
			.iter()
			.map(|snapshot| (&snapshot.source, snapshot))
			.collect::<std::collections::BTreeMap<_, _>>();
		let mut edited = Vec::with_capacity(self.sources.len());
		for source in &self.sources {
			let snapshot = available[&source.source];
			let mut text = snapshot.text.to_string();
			for replacement in source.replacements.iter().rev() {
				text.replace_range(
					replacement.span.start..replacement.span.end,
					replacement.replacement.as_str(),
				);
			}
			edited.push(EditedSource {
				source: source.source.clone(),
				version: source.version,
				text,
			});
		}
		Ok(edited)
	}
}

/// Immutable current source supplied when validating or applying an edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSnapshot<'a> {
	pub source: SourceId,
	pub version: SourceVersion,
	pub text: &'a str,
}

impl<'a> SourceSnapshot<'a> {
	#[must_use]
	pub fn new(source: SourceId, version: SourceVersion, text: &'a str) -> Self {
		Self {
			source,
			version,
			text,
		}
	}
}

/// Result for one source after a complete edit group is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditedSource {
	pub source: SourceId,
	pub version: SourceVersion,
	pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
	EmptyTitle,
	EmptyGroup,
	EmptySourceIdentity {
		source: SourceId,
	},
	EmptySourceEdit {
		source: SourceId,
	},
	InvalidSpan {
		source: SourceId,
		span: Span,
	},
	OverlappingReplacements {
		source: SourceId,
		first: Span,
		second: Span,
	},
	DuplicateSource {
		source: SourceId,
	},
	DuplicateSnapshot {
		source: SourceId,
	},
	MissingSource {
		source: SourceId,
	},
	StaleSource {
		source: SourceId,
		expected: SourceVersion,
		actual: SourceVersion,
	},
	SpanOutOfBounds {
		source: SourceId,
		span: Span,
		len: usize,
	},
	InvalidUtf8Boundary {
		source: SourceId,
		span: Span,
	},
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Severity {
	Error,
	Warning,
	Info,
	Hint,
}

/// A secondary annotation attached to a diagnostic, pointing at a related span.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
	pub span: Span,
	pub message: EcoString,
}

impl Label {
	pub fn new(span: Span, message: impl Into<EcoString>) -> Self {
		Self {
			span,
			message: message.into(),
		}
	}
}

/// A compiler diagnostic: a primary message anchored at a span, plus optional
/// secondary labels, notes, and a help hint.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
	pub severity: Severity,
	pub code: EcoString,
	pub message: EcoString,
	pub span: Span,
	pub labels: Vec<Label>,
	pub notes: Vec<EcoString>,
	pub help: Option<EcoString>,
	edits: Vec<EditGroup>,
}

impl Diagnostic {
	pub fn new(
		severity: Severity,
		code: EcoString,
		message: impl Into<EcoString>,
		span: Span,
	) -> Self {
		Self {
			severity,
			code,
			message: message.into(),
			span,
			labels: Vec::new(),
			notes: Vec::new(),
			help: None,
			edits: Vec::new(),
		}
	}

	pub fn error(code: EcoString, message: impl Into<EcoString>, span: impl Into<Span>) -> Self {
		Self::new(Severity::Error, code, message, span.into())
	}

	pub fn warning(code: EcoString, message: impl Into<EcoString>, span: impl Into<Span>) -> Self {
		Self::new(Severity::Warning, code, message, span.into())
	}

	#[must_use]
	pub fn with_code(mut self, code: impl Into<EcoString>) -> Self {
		self.code = code.into();
		self
	}

	#[must_use]
	pub fn with_label(mut self, label: Label) -> Self {
		self.labels.push(label);
		self
	}

	#[must_use]
	pub fn with_note(mut self, note: impl Into<EcoString>) -> Self {
		self.notes.push(note.into());
		self
	}

	#[must_use]
	pub fn with_help(mut self, help: impl Into<EcoString>) -> Self {
		self.help = Some(help.into());
		self
	}

	#[must_use]
	pub fn with_edit(mut self, edit: EditGroup) -> Self {
		self.edits.push(edit);
		self
	}

	#[must_use]
	pub fn edits(&self) -> &[EditGroup] {
		&self.edits
	}

	pub fn is_error(&self) -> bool {
		self.severity == Severity::Error
	}
}

/// A typed diagnostic: an error/warning defined as one variant of a phase enum
/// (`LexError`, `ParseError`, `TypeError`, …) rather than an inline string. The
/// variant owns its message, severity, secondary labels, notes, and help; the
/// primary span is supplied when it is emitted. Catalog variants are append-only
/// because their stable numeric codes include their declaration position.
pub trait IntoDiagnostic: ErrorCode {
	/// The primary human-readable message.
	fn message(&self) -> EcoString;

	fn severity(&self) -> Severity {
		Severity::Error
	}

	/// Secondary annotations pointing at related spans (e.g. a previous definition).
	fn labels(&self) -> Vec<Label> {
		Vec::new()
	}

	fn notes(&self) -> Vec<EcoString> {
		Vec::new()
	}

	fn help(&self) -> Option<EcoString> {
		None
	}

	fn edits(&self) -> Vec<EditGroup> {
		Vec::new()
	}

	/// Assemble the full [`Diagnostic`], anchoring the primary message at `span`.
	fn as_diagnostic(&self, span: impl Into<Span>) -> Diagnostic
	where
		Self: Sized,
	{
		Diagnostic {
			severity: self.severity(),
			code: self.code().to_string().into(),
			message: self.message(),
			span: span.into(),
			labels: self.labels(),
			notes: self.notes(),
			help: self.help(),
			edits: self.edits(),
		}
	}
}

pub trait ErrorCode {
	/// The stable error code assigned to this catalog variant.
	fn code(&self) -> u16;
}

fn range(span: Span) -> std::ops::Range<usize> {
	span.start..span.end
}

fn report_kind(severity: Severity) -> ReportKind<'static> {
	match severity {
		Severity::Error => ReportKind::Error,
		Severity::Warning => ReportKind::Warning,
		Severity::Info | Severity::Hint => ReportKind::Advice,
	}
}

/// Render a batch of diagnostics for one source file into a pretty string suitable
/// for terminal output. `filename` is used as the report's source id.
pub fn render(filename: &str, source: &str, diagnostics: &[Diagnostic]) -> String {
	let mut out = Vec::new();
	for diagnostic in diagnostics {
		let mut builder = Report::build(
			report_kind(diagnostic.severity),
			(filename, range(diagnostic.span)),
		)
		.with_config(Config::default().with_index_type(IndexType::Byte))
		.with_message(diagnostic.message.as_str());

		builder = builder.with_code(diagnostic.code.as_str());

		builder = builder.with_label(
			AriadneLabel::new((filename, range(diagnostic.span)))
				.with_message(diagnostic.message.as_str())
				.with_color(Color::Red),
		);

		for label in &diagnostic.labels {
			builder = builder.with_label(
				AriadneLabel::new((filename, range(label.span)))
					.with_message(label.message.as_str())
					.with_color(Color::Yellow),
			);
		}

		for note in &diagnostic.notes {
			builder = builder.with_note(note.as_str());
		}
		for edit in diagnostic.edits() {
			builder = builder.with_note(format!("machine-applicable edit: {}", edit.title()));
		}
		if let Some(help) = &diagnostic.help {
			builder = builder.with_help(help.as_str());
		}

		let mut buf = Vec::new();
		let _ = builder
			.finish()
			.write((filename, Source::from(source)), &mut buf);
		out.extend_from_slice(&buf);
	}
	String::from_utf8_lossy(&out).into_owned()
}
