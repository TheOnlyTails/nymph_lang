# Formatting

Nymph has one canonical source style. Formatter integrations use the reusable
`nymph-format` library and produce the same result independently of the filesystem,
editor, or command-line environment.

## Layout

- Indentation is tabs. A tab has display width 2; spaces are used only for alignment.
- Lines have a soft width of 100 columns.
- Output uses LF line endings and ends with exactly one newline.
- Nonempty blocks and `match` expressions are multiline.
- A comma-separated list which becomes multiline has one item on each line and a
  trailing comma.
- Semicolons are not emitted. Imports remain in source order.
- Blank-line runs collapse to one blank line; formatting does not invent declaration groups.

## Source preservation

Comments stay in source order. Literal spelling, escapes, numeric bases and separators,
and all other non-trivia lexemes are preserved. String interpolations are formatted as
expressions, including interpolations containing balanced blocks and `match` expressions.

The formatter removes only grouping or single-expression block boundaries that are
provably redundant. It retains boundaries required by precedence, control flow, guards,
closures, labels, or ambiguous comment attachment.

Malformed source is never rewritten: lexer and parser diagnostics are returned as
structured data and no output is produced. Range formatting expands a request to a
supported syntax unit and returns both that exact replacement span and its text.
