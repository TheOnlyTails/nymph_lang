//! Compile-check fence harness for every ` ```nym ` code sample under
//! `docs/**/*.md`.
//!
//! This is a data-driven walker: adding, editing, or removing a doc sample
//! never needs a test edit here, only the right fence tag (`nym` to be
//! checked, anything else — typically `nymph` — to be left alone) on the
//! sample itself.
//!
//! # Verification model
//!
//! Deliberately simple: each ` ```nym ` fenced block is *checked* (parsed +
//! type-checked via [`nymph_compiler::check`]) — never lowered, emitted, or
//! run under Node, and no stdout/value is asserted.
//!
//! - A block with **no** line carrying VitePress's `// [!code error]` line
//!   comment must check clean: zero error diagnostics.
//! - A block with **one or more** such lines must fail to check, with at
//!   least one error diagnostic landing on one of the marked lines. The
//!   marker comment does double duty: VitePress renders it as an inline
//!   error highlight in the rendered docs, and it tells this harness the
//!   sample is expected to fail right there. The marker is stripped (line
//!   number preserved) before the block is handed to the checker.
//!
//! A fence whose info string's first token isn't exactly `nym` (e.g. the
//! `nymph`-tagged illustrative fragments) is not extracted at all — it is
//! simply not checked.

use nymph_compiler::check;
use std::path::{Path, PathBuf};

// ── markdown fence extraction ───────────────────────────────────────────────

/// One extracted `nym`-tagged fenced code block.
struct Fence {
	body: String,
	/// 1-based line number of the first line of the fence body (the line
	/// right after the opening ` ``` `), for failure messages.
	line: usize,
}

/// Walk `markdown`'s fenced code blocks and extract every one whose info
/// string's first whitespace-separated token is exactly `nym` (so
/// ```` ```nym [file.nym] ```` — VitePress's code-group label — counts, but
/// ```` ```nymph ```` and anything else does not).
fn extract_nym_fences(markdown: &str) -> Vec<Fence> {
	let mut fences = Vec::new();
	// `Some(len)` once inside any fence (nym or otherwise), tracking the
	// opening backtick run length so the close is matched correctly.
	let mut open_len: Option<usize> = None;
	let mut capture: Option<(Vec<&str>, usize)> = None;

	for (idx, line) in markdown.lines().enumerate() {
		let trimmed = line.trim_start();
		let backticks = trimmed.chars().take_while(|&c| c == '`').count();

		if let Some(len) = open_len {
			let is_close = backticks >= len && trimmed[backticks..].trim().is_empty();
			if is_close {
				open_len = None;
				if let Some((body, line)) = capture.take() {
					fences.push(Fence {
						body: body.join("\n"),
						line,
					});
				}
			} else if let Some((body, _)) = capture.as_mut() {
				body.push(line);
			}
		} else if backticks >= 3 {
			let info = trimmed[backticks..].trim();
			let first_token = info.split_whitespace().next().unwrap_or("");
			open_len = Some(backticks);
			capture = (first_token == "nym").then(|| (Vec::new(), idx + 2));
		}
	}

	fences
}

/// Recursively collect every `*.md` file under `dir`.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			collect_md_files(&path, out);
		} else if path.extension().is_some_and(|ext| ext == "md") {
			out.push(path);
		}
	}
}

// ── expression-vs-program heuristic + the CLI's inline-eval wrap ───────────

const DECL_KEYWORDS: &[&str] = &[
	"func ",
	"struct ",
	"enum ",
	"interface ",
	"impl ",
	"namespace ",
	"type ",
	"import",
	"use",
	"let ",
];

/// A block is a full program if any top-level (bracket-depth-0) line starts
/// with a declaration keyword; otherwise it's a bare expression (or sequence
/// of expression-statements), mirroring the CLI's `run -e` inline-eval path.
fn is_full_program(body: &str) -> bool {
	let mut depth: i32 = 0;
	for line in body.lines() {
		let trimmed = line.trim();
		if depth == 0
			&& !trimmed.is_empty()
			&& !trimmed.starts_with("//")
			&& DECL_KEYWORDS.iter().any(|kw| trimmed.starts_with(kw))
		{
			return true;
		}
		for ch in line.chars() {
			match ch {
				'{' | '(' | '[' => depth += 1,
				'}' | ')' | ']' => depth -= 1,
				_ => {}
			}
		}
	}
	false
}

// ── `// [!code error]` marker handling ──────────────────────────────────────

/// Does this line carry a trailing VitePress `// [!code error]` (optionally
/// `// [!code error:N]`) line comment?
fn has_error_marker(line: &str) -> bool {
	let Some(idx) = line.find("[!code error") else {
		return false;
	};
	let rest = line[idx + "[!code error".len()..].trim_start();
	let rest = rest.strip_prefix(':').map_or(rest, |after_colon| {
		after_colon.trim_start_matches(|c: char| c.is_ascii_digit())
	});
	rest.trim_start().starts_with(']')
}

/// Strip a trailing `// [!code error]` marker (and any leading whitespace
/// that introduced the `//`) from `line`, leaving the code before it intact
/// and NOT shifting line numbers (the line itself is kept, just shortened).
fn strip_error_marker(line: &str) -> String {
	let Some(comment_idx) = line.find("//") else {
		return line.to_string();
	};
	if has_error_marker(&line[comment_idx..]) {
		line[..comment_idx].trim_end().to_string()
	} else {
		line.to_string()
	}
}

/// Split `body` into (checkable source with markers stripped, set of 1-based
/// line numbers that carried a marker).
fn strip_markers(body: &str) -> (String, Vec<usize>) {
	let mut marked_lines = Vec::new();
	let mut out_lines = Vec::new();
	for (idx, line) in body.lines().enumerate() {
		if has_error_marker(line) {
			marked_lines.push(idx + 1);
			out_lines.push(strip_error_marker(line));
		} else {
			out_lines.push(line.to_string());
		}
	}
	(out_lines.join("\n"), marked_lines)
}

/// 1-based line number of a byte offset into `source`.
fn line_of_offset(source: &str, offset: usize) -> usize {
	source[..offset.min(source.len())]
		.bytes()
		.filter(|&b| b == b'\n')
		.count()
		+ 1
}

// ── per-fence verdict ────────────────────────────────────────────────────────

/// Check one extracted fence, returning `Err(reason)` on failure.
fn check_fence(file: &Path, fence: &Fence) -> Result<(), String> {
	let label = format!("{}:{}", file.display(), fence.line);
	let (stripped, marked_lines) = strip_markers(&fence.body);

	if marked_lines.is_empty() {
		// Compile-clean case. A bare expression needs the same throwaway-func
		// wrap the CLI's `run -e` inline-eval path uses so it forms a valid
		// module to check; every current doc sample is a full program, so
		// this path exists for completeness/future samples.
		let checked = if is_full_program(&stripped) {
			stripped
		} else {
			format!("func __nymph_repl() = {{\n{stripped}\n}}\n")
		};
		let diags = check(&checked, &label);
		let errs: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
		if errs.is_empty() {
			Ok(())
		} else {
			Err(format!("{label}: expected a clean check, got: {errs:?}"))
		}
	} else {
		// Expected-error case: every marked sample in the docs today is a
		// full program, and wrapping would shift line numbers, so this path
		// never wraps.
		let diags = check(&stripped, &label);
		let errs: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
		if errs.is_empty() {
			return Err(format!(
				"{label}: marked with `// [!code error]` but checked cleanly"
			));
		}
		let hit_marked_line = errs
			.iter()
			.any(|d| marked_lines.contains(&line_of_offset(&stripped, d.span.start)));
		if hit_marked_line {
			Ok(())
		} else {
			let lines: Vec<_> = errs
				.iter()
				.map(|d| line_of_offset(&stripped, d.span.start))
				.collect();
			Err(format!(
				"{label}: expected an error diagnostic on one of marked line(s) {marked_lines:?}, \
				 but errors landed on line(s) {lines:?}: {errs:?}"
			))
		}
	}
}

// ── the actual doc-sample sweep ──────────────────────────────────────────────

#[test]
fn every_doc_sample_is_covered() {
	let docs_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
	assert!(docs_dir.is_dir(), "expected {docs_dir:?} to exist");

	let mut md_files = Vec::new();
	collect_md_files(&docs_dir, &mut md_files);
	assert!(
		!md_files.is_empty(),
		"expected to find at least one markdown file under {docs_dir:?}"
	);

	let mut clean_count = 0;
	let mut error_marked_count = 0;
	let mut failures = Vec::new();

	for file in &md_files {
		let markdown = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
		for fence in extract_nym_fences(&markdown) {
			if fence.body.lines().any(has_error_marker) {
				error_marked_count += 1;
			} else {
				clean_count += 1;
			}
			if let Err(reason) = check_fence(file, &fence) {
				failures.push(reason);
			}
		}
	}

	eprintln!(
		"doc samples: {clean_count} compile-clean, {error_marked_count} error-marked (total {})",
		clean_count + error_marked_count
	);

	assert!(
		clean_count + error_marked_count > 0,
		"expected at least one `nym` fence across {} markdown file(s) under {docs_dir:?}, found none — \
		 the fence walker or the `nym` tag match is likely broken, which would make this test pass vacuously",
		md_files.len()
	);

	assert!(
		failures.is_empty(),
		"{} of {} doc sample(s) failed:\n\n{}",
		failures.len(),
		clean_count + error_marked_count,
		failures.join("\n\n")
	);
}
