//! A single canonical diagnostic type used across the whole toolchain.
//!
//! Every stage (lexer, parser, checker, codegen) produces [`Diagnostic`]s. The CLI
//! renders them with [`ariadne`] into pretty terminal output; the language server
//! converts them into LSP diagnostics. Keeping one type here means neither backend
//! leaks into the compiler crates.

use ariadne::{Color, Config, IndexType, Label as AriadneLabel, Report, ReportKind, Source};
use ecow::EcoString;
use nymph_ast::Span;

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

	pub fn is_error(&self) -> bool {
		self.severity == Severity::Error
	}
}

/// A typed diagnostic: an error/warning defined as one variant of a phase enum
/// (`LexError`, `ParseError`, `TypeError`, …) rather than an inline string. The
/// variant owns its message, severity, secondary labels, notes, and help; the
/// primary span is supplied when it is emitted. `code` defaults to `None` and is
/// filled in per-variant once the code scheme is assigned.
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

	/// Assemble the full [`Diagnostic`], anchoring the primary message at `span`.
	fn into_diagnostic(&self, span: impl Into<Span>) -> Diagnostic
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
		}
	}
}

pub trait ErrorCode {
	/// The stable error code, once assigned. Defaults to `None`.
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
