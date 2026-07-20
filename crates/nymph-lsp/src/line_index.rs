//! Byte-offset <-> UTF-16 `(line, column)` conversion, so [`nymph_ast::Span`]
//! (half-open byte ranges) can be turned into [`lsp_types::Range`] (UTF-16
//! `Position`s, per the LSP spec's default `positionEncoding`).
//!
//! Built once per document version from its full text; every line's start
//! byte offset is recorded up front so a byte offset -> position lookup is a
//! binary search rather than a re-scan of the whole document.

use lsp_types::{Position, Range};
use nymph_ast::Span;

/// A document's line-start table, built once per text so repeated
/// offset->position lookups (one per diagnostic/hover request) don't re-scan
/// the whole file. `\r\n` is treated as ending its line at the position
/// right after the `\n` (the `\r` itself lands on the previous line, mirroring
/// how every LSP client already reasons about CRLF documents).
pub struct LineIndex {
	/// Byte offset of the start of each line; `line_starts[0] == 0`.
	line_starts: Vec<usize>,
	text_len: usize,
}

impl LineIndex {
	#[must_use]
	pub fn new(text: &str) -> Self {
		let mut line_starts = vec![0];
		for (i, b) in text.bytes().enumerate() {
			if b == b'\n' {
				line_starts.push(i + 1);
			}
		}
		Self {
			line_starts,
			text_len: text.len(),
		}
	}

	/// Convert a byte offset into a `(line, utf16_column)` pair, both
	/// 0-based. An offset past the end of the text clamps to the last valid
	/// position.
	#[must_use]
	pub fn position(&self, text: &str, offset: usize) -> Position {
		let offset = offset.min(self.text_len);
		// The last line-start <= offset is this offset's line (binary search
		// over a sorted, deduplicated ascending table).
		let line = match self.line_starts.binary_search(&offset) {
			Ok(exact) => exact,
			Err(insert_at) => insert_at - 1,
		};
		let line_start = self.line_starts[line];
		// UTF-16 column: count UTF-16 code units of every char strictly
		// between the line start and `offset` (handles multi-byte UTF-8 and
		// surrogate-pair-requiring astral characters alike).
		let col: usize = text[line_start..offset].chars().map(char::len_utf16).sum();
		Position {
			line: line as u32,
			character: col as u32,
		}
	}

	/// Convert a [`Span`] (half-open byte range) to an LSP [`Range`].
	#[must_use]
	pub fn range(&self, text: &str, span: Span) -> Range {
		Range {
			start: self.position(text, span.start),
			end: self.position(text, span.end),
		}
	}

	/// Convert a `(line, utf16_column)` position back to a byte offset — the
	/// inverse of [`Self::position`], needed to turn a hover request's
	/// cursor position into an offset [`nymph_sema::query::type_at`] can
	/// search with. A line/column past the end of the text clamps to the
	/// nearest valid offset.
	#[must_use]
	pub fn offset(&self, text: &str, position: Position) -> usize {
		let line = (position.line as usize).min(self.line_starts.len() - 1);
		let line_start = self.line_starts[line];
		let line_end = self
			.line_starts
			.get(line + 1)
			.copied()
			.unwrap_or(self.text_len)
			.min(text.len());
		let slice = &text[line_start..line_end];

		let mut utf16_count: u32 = 0;
		for (byte_idx, ch) in slice.char_indices() {
			if utf16_count >= position.character {
				return line_start + byte_idx;
			}
			utf16_count += ch.len_utf16() as u32;
		}
		line_end
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ascii_single_line_round_trips() {
		let text = "let x = 1";
		let idx = LineIndex::new(text);
		assert_eq!(
			idx.position(text, 4),
			Position {
				line: 0,
				character: 4
			}
		);
	}

	#[test]
	fn multi_byte_char_advances_by_utf16_units() {
		// "é" is 2 UTF-8 bytes but 1 UTF-16 unit; "𝔘" (U+1D518) is 4 UTF-8
		// bytes but a UTF-16 *surrogate pair* (2 units).
		let text = "é𝔘x";
		let idx = LineIndex::new(text);
		// offset 0 -> before 'é'
		assert_eq!(idx.position(text, 0).character, 0);
		// offset 2 -> after 'é' (2 UTF-8 bytes), 1 UTF-16 unit in
		assert_eq!(idx.position(text, 2).character, 1);
		// offset 2 + 4 = 6 -> after '𝔘' (4 UTF-8 bytes), 1 + 2 = 3 UTF-16 units in
		assert_eq!(idx.position(text, 6).character, 3);
		// offset 7 -> after 'x'
		assert_eq!(idx.position(text, 7).character, 4);
	}

	#[test]
	fn crlf_line_endings_land_on_the_right_line() {
		let text = "abc\r\ndef";
		let idx = LineIndex::new(text);
		// 'a','b','c' on line 0; '\r' at offset 3 is still line 0 (column 3);
		// '\n' is at offset 4, so offset 5 ('d') starts line 1, column 0.
		assert_eq!(
			idx.position(text, 3),
			Position {
				line: 0,
				character: 3
			}
		);
		assert_eq!(
			idx.position(text, 5),
			Position {
				line: 1,
				character: 0
			}
		);
	}

	#[test]
	fn range_covers_a_span() {
		let text = "func f() = 1 + 2";
		let idx = LineIndex::new(text);
		let span = Span::new(11, 16);
		let range = idx.range(text, span);
		assert_eq!(range.start.character, 11);
		assert_eq!(range.end.character, 16);
	}

	#[test]
	fn offset_round_trips_through_position_for_ascii() {
		let text = "func f() = 1 + 2";
		let idx = LineIndex::new(text);
		for byte in 0..=text.len() {
			let pos = idx.position(text, byte);
			assert_eq!(
				idx.offset(text, pos),
				byte,
				"byte {byte} did not round-trip"
			);
		}
	}

	#[test]
	fn offset_round_trips_through_a_multi_byte_char_and_crlf() {
		let text = "é𝔘x\r\nsecond";
		let idx = LineIndex::new(text);
		for (byte, _) in text.char_indices() {
			let pos = idx.position(text, byte);
			assert_eq!(
				idx.offset(text, pos),
				byte,
				"byte {byte} did not round-trip"
			);
		}
	}
}
