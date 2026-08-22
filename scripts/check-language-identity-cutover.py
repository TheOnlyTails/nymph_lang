#!/usr/bin/env python3
"""Freeze and check every legacy language-identity path owned by the repository."""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASELINE = ROOT / "scripts/language-identity-cutover-inventory.json"
CORPUS = ROOT / "crates/nymph-compiler/testdata/legacy-migration"


@dataclass(frozen=True)
class Rule:
    category: str
    name: str
    paths: tuple[str, ...]
    pattern: re.Pattern[str]
    seed_path: str
    seed: str


RULES = (
    Rule(
        "syntax-ast",
        "accepted-legacy-ast",
        ("crates/nymph-ast", "crates/nymph-syntax", "crates/nymph-format", "crates/nymph-lsp"),
        re.compile(r"\b(?:Type::Mut|LetKind::Mut|FuncKind::Mut|ExprKind::AssignOp|ExprKind::While|AssignOperator)\b"),
        "crates/nymph-ast/src/seed.rs",
        "ExprKind::While",
    ),
    Rule(
        "sema-stable",
        "semantic-and-stable-legacy",
        ("crates/nymph-sema",),
        re.compile(r"\b(?:TyKind::Mut|implicit_uint_to_int|StableExprKind::AssignOp|StableExprKind::While)\b"),
        "crates/nymph-sema/src/seed.rs",
        "StableExprKind::AssignOp",
    ),
    Rule(
        "hir-emitter",
        "legacy-hir-and-emitter",
        ("crates/nymph-hir", "crates/nymph-codegen", "crates/nymph-sema/src/stable_lowering.rs"),
        re.compile(r"\b(?:HirExpr::Assign|HirExpr::While|lower_for)\b|\bmutable\s*:\s*bool\b"),
        "crates/nymph-codegen/src/seed.rs",
        "HirExpr::Assign",
    ),
    Rule(
        "runtime",
        "source-compatibility-runtime",
        ("crates/nymph-codegen", "stdlib/src"),
        re.compile(r"\b(?:nymphCell(?:Get|Set)?|NymphListIterator|NymphMapIterator)\b"),
        "crates/nymph-codegen/src/seed.rs",
        "nymphCellSet",
    ),
    Rule(
        "extension",
        "retired-extension-grammar",
        ("extension/syntaxes", "extension/snippets"),
        re.compile(r"(?i)\b(?:while|mut)\b|compound\.assignment|keyword\.control\.while"),
        "extension/syntaxes/seed.json",
        "while",
    ),
    Rule(
        "release-echo",
        "release-echo-bytes",
        ("crates/nymph-compiler/src", "crates/nymph-codegen/src", "stdlib/src"),
        re.compile(r"\b(?:nymphEcho|echoObserver|echoSite|echoSourceUri)\b"),
        "crates/nymph-codegen/src/seed.rs",
        "nymphEcho",
    ),
    Rule(
        "inert-build",
        "ordinary-build-launcher",
        ("crates/nymph-compiler/src/project",),
        re.compile(r"(?:append|push_str|format!)\s*\([^\n]{0,120}\bmain\s*\(\s*\)"),
        "crates/nymph-compiler/src/project/seed.rs",
        'output.push_str("main()")',
    ),
)

SOURCE_ROOTS = (ROOT / "stdlib/src", ROOT / "examples")
RUST_SOURCE_ROOTS = (
    ROOT / "crates/nymph-syntax/tests",
    ROOT / "crates/nymph-sema/tests",
    ROOT / "crates/nymph-compiler/tests",
    ROOT / "crates/nymph-cli/tests",
    ROOT / "crates/nymph-format/tests",
    ROOT / "crates/nymph-lsp/tests",
)
GENERATED_SOURCE_FILES = (
    ROOT / "crates/nymph-compiler/src/project/repl.rs",
    ROOT / "crates/nymph-cli/src/commands/new.rs",
    ROOT / "crates/nymph-compiler/src/host_runtime.rs",
)
MUTATION_ROOTS = (ROOT / "stdlib/src", ROOT / "crates/nymph-codegen/src")

TOKEN = re.compile(
    r"[A-Za-z_][A-Za-z0-9_]*|\*\*=|<<=|>>=|&&=|\|\|=|[+\-*/%&|^~]=|==|!=|<=|>=|\S"
)
COMPOUND_ASSIGN = re.compile(r"^(?:\*\*=|<<=|>>=|&&=|\|\|=|[+\-*/%&|^~]=)$")
SIMPLE_ASSIGN = re.compile(
    r"(?m)^[ \t]*(?:[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*|\[[^\]\n]+\])*)[ \t]*=(?!=)"
)
RUNTIME_MUTATION = re.compile(
    r"(?:\.[A-Za-z_$][\w$]*|\[[^\]\n]+\])\s*(?:=|\+=|-=|\*=|/=)|"
    r"\.(?:push|pop|splice|set|delete|clear)\s*\("
)


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def mask_comments_and_literals(source: str) -> str:
    """Keep code positions/newlines while masking comments, strings, and chars."""
    chars = list(source)
    index = 0
    block_depth = 0
    quote: str | None = None
    while index < len(chars):
        if block_depth:
            if source.startswith("/*", index):
                chars[index : index + 2] = "  "
                block_depth += 1
                index += 2
            elif source.startswith("*/", index):
                chars[index : index + 2] = "  "
                block_depth -= 1
                index += 2
            else:
                if chars[index] != "\n":
                    chars[index] = " "
                index += 1
            continue
        if quote:
            if source[index] == "\\":
                chars[index] = " "
                if index + 1 < len(chars) and chars[index + 1] != "\n":
                    chars[index + 1] = " "
                index += 2
            else:
                current = source[index]
                if current != "\n":
                    chars[index] = " "
                index += 1
                if current == quote:
                    quote = None
            continue
        if source.startswith("//", index):
            end = source.find("\n", index)
            end = len(chars) if end < 0 else end
            chars[index:end] = " " * (end - index)
            index = end
        elif source.startswith("/*", index):
            chars[index : index + 2] = "  "
            block_depth = 1
            index += 2
        elif source[index] in ('"', "'"):
            quote = source[index]
            chars[index] = " "
            index += 1
        else:
            index += 1
    return "".join(chars)


def line_number(source: str, offset: int, base: int = 1) -> int:
    return base + source.count("\n", 0, offset)


def source_matches(source: str, base_line: int = 1) -> list[tuple[str, int, str]]:
    masked = mask_comments_and_literals(source)
    matches: list[tuple[str, int, str]] = []
    for token in TOKEN.finditer(masked):
        value = token.group()
        rule = None
        if value == "mut":
            rule = "source-mut"
        elif value == "while":
            rule = "source-while"
        elif COMPOUND_ASSIGN.fullmatch(value):
            rule = "source-compound-assignment"
        if rule:
            matches.append((rule, line_number(source, token.start(), base_line), value))
    for match in re.finditer(r"\bnext\s*\([^)]*\)\s*:\s*Option\b", masked):
        matches.append(
            ("source-option-iterator", line_number(source, match.start(), base_line), match.group())
        )
    for match in SIMPLE_ASSIGN.finditer(masked):
        if match.group().strip().removesuffix("=").strip() == "let":
            continue
        # Destination named fields and nested pattern bindings also use `=`.
        # A mutable assignment statement is never nested inside parentheses or
        # brackets, and a whole-value pattern binding is followed by `->`.
        prefix = masked[: match.start()]
        paren_depth = prefix.count("(") - prefix.count(")")
        bracket_depth = prefix.count("[") - prefix.count("]")
        line_end = masked.find("\n", match.end())
        line_end = len(masked) if line_end < 0 else line_end
        if paren_depth > 0 or bracket_depth > 0 or "->" in masked[match.end() : line_end]:
            continue
        matches.append(
            ("source-assignment", line_number(source, match.start(), base_line), match.group())
        )
    return matches


def markdown_sources(path: Path) -> list[tuple[str, str, int]]:
    text = path.read_text(encoding="utf-8")
    sources = []
    fence = re.compile(r"^```([^\n]*)\n(.*?)^```\s*$", re.MULTILINE | re.DOTALL)
    index = 0
    for match in fence.finditer(text):
        language = match.group(1).strip().split(maxsplit=1)[0] if match.group(1).strip() else ""
        if language != "nym":
            continue
        index += 1
        line = line_number(text, match.start(2))
        sources.append((f"{relative(path)}#nym-fence-{index}", match.group(2), line))
    return sources


def rust_strings(path: Path) -> list[tuple[str, str, int]]:
    text = path.read_text(encoding="utf-8")
    found: list[tuple[str, str, int]] = []
    occupied: list[tuple[int, int]] = []
    raw = re.compile(
        r'(?<![A-Za-z0-9_])r(?P<hash>#{0,16})"(?P<body>.*?)"(?P=hash)', re.DOTALL
    )
    for match in raw.finditer(text):
        occupied.append(match.span())
        found.append((relative(path), match.group("body"), line_number(text, match.start())))
    normal = re.compile(r'"(?:\\.|[^"\\])*"', re.DOTALL)
    for match in normal.finditer(text):
        if any(start <= match.start() < end for start, end in occupied):
            continue
        try:
            value = ast.literal_eval(match.group())
        except (SyntaxError, ValueError):
            continue
        if isinstance(value, str):
            line = line_number(text, match.start())
            found.append((relative(path), value, line))
    return found


def looks_like_nymph_source(source: str) -> bool:
    if re.search(r"(?:=>|\b(?:const|function|class)\b|console\.|process\.|</?[A-Za-z]|\[package\])", source):
        return False
    return re.search(
        r"(?m)^\s*(?:(?:public|internal|private)\s+)?(?:async\s+)?"
        r"(?:func|struct|enum|interface|impl|namespace|type|import|external)\b|"
        r"^\s*let\s+(?:mut\s+)?[A-Za-z_]",
        source,
    ) is not None


def add_match(
    findings: dict[tuple[str, str, str], list[tuple[int, str]]],
    category: str,
    rule: str,
    path: str,
    line: int,
    text: str,
) -> None:
    findings.setdefault((category, rule, path), []).append((line, " ".join(text.split())))


def scan_sources(findings: dict[tuple[str, str, str], list[tuple[int, str]]]) -> None:
    for path in sorted(CORPUS.rglob("*.nym.txt")):
        source = path.read_text(encoding="utf-8")
        for rule, line, text in source_matches(source):
            add_match(findings, "frozen-legacy-source", rule, relative(path), line, text)
    for source_root in SOURCE_ROOTS:
        for path in sorted(source_root.rglob("*.nym")):
            source = path.read_text(encoding="utf-8")
            for rule, line, text in source_matches(source):
                add_match(findings, "source", rule, relative(path), line, text)
    for path in sorted((ROOT / "docs").rglob("*.md")):
        if relative(path).startswith(("docs/research/", "docs/design/", "docs/adr/")):
            continue
        for name, source, base_line in markdown_sources(path):
            for rule, line, text in source_matches(source, base_line):
                add_match(findings, "docs-source", rule, name, line, text)
    rust_files = set(GENERATED_SOURCE_FILES)
    for source_root in RUST_SOURCE_ROOTS:
        rust_files.update(source_root.rglob("*.rs"))
    for path in sorted(rust_files):
        if not path.is_file():
            continue
        category = "generated-source" if path in GENERATED_SOURCE_FILES else "parser-fixture-source"
        for name, source, base_line in rust_strings(path):
            if not looks_like_nymph_source(source):
                continue
            for rule, line, text in source_matches(source, base_line):
                add_match(findings, category, rule, name, line, text)


def scan_static_rules(findings: dict[tuple[str, str, str], list[tuple[int, str]]]) -> None:
    for rule in RULES:
        files: set[Path] = set()
        for owner in rule.paths:
            path = ROOT / owner
            if path.is_file():
                files.add(path)
            elif path.is_dir():
                files.update(candidate for candidate in path.rglob("*") if candidate.is_file())
        for path in sorted(files):
            if path.suffix not in {".rs", ".ts", ".js", ".json", ".nym", ".d.ts"}:
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            for match in rule.pattern.finditer(text):
                add_match(
                    findings,
                    rule.category,
                    rule.name,
                    relative(path),
                    line_number(text, match.start()),
                    match.group(),
                )


def scan_runtime_mutation(findings: dict[tuple[str, str, str], list[tuple[int, str]]]) -> None:
    for owner in MUTATION_ROOTS:
        for path in sorted(candidate for candidate in owner.rglob("*") if candidate.suffix in {".js", ".ts"}):
            text = path.read_text(encoding="utf-8")
            masked = mask_comments_and_literals(text)
            for match in RUNTIME_MUTATION.finditer(masked):
                add_match(
                    findings,
                    "private-runtime-mutation",
                    "reviewed-runtime-mutation",
                    relative(path),
                    line_number(text, match.start()),
                    match.group(),
                )


def snapshot() -> list[dict[str, object]]:
    findings: dict[tuple[str, str, str], list[tuple[int, str]]] = {}
    scan_sources(findings)
    scan_static_rules(findings)
    scan_runtime_mutation(findings)
    categories = {
        "source",
        "docs-source",
        "frozen-legacy-source",
        "generated-source",
        "parser-fixture-source",
        "private-runtime-mutation",
        *(rule.category for rule in RULES),
    }
    inventory = []
    for category in sorted(categories):
        records = []
        for (record_category, rule, path), matches in sorted(findings.items()):
            if record_category != category:
                continue
            for line, text in sorted(matches):
                records.append(f"{rule}\0{path}\0{line}\0{text}")
        entry = {
            "category": category,
            "rules": sorted({key[1] for key in findings if key[0] == category}),
            "paths": sorted({key[2] for key in findings if key[0] == category}),
            "occurrences": len(records),
            "digest": hashlib.sha256("\n".join(records).encode()).hexdigest(),
        }
        if category == "private-runtime-mutation":
            entry["reviewed"] = []
            for path in entry["paths"]:
                operations = [record for record in records if f"\0{path}\0" in record]
                entry["reviewed"].append(
                    {
                        "path": path,
                        "occurrences": len(operations),
                        "digest": hashlib.sha256("\n".join(operations).encode()).hexdigest(),
                    }
                )
        inventory.append(entry)
    return inventory


def self_test() -> None:
    source = 'let value = "while mut +=" // while\n/* mut */\nlet mut x = 0\nx += 1\nx = 2\nwhile x < 2 {}\n'
    found = {rule for rule, _, _ in source_matches(source)}
    assert found == {
        "source-mut",
        "source-assignment",
        "source-compound-assignment",
        "source-while",
    }, found
    iterator = source_matches("mut func next(): Option<int>")
    assert {rule for rule, _, _ in iterator} == {"source-mut", "source-option-iterator"}
    assert looks_like_nymph_source("func main(): void = {}")
    assert looks_like_nymph_source("let mut value = 0\nvalue = 1")
    assert not looks_like_nymph_source("const value = 0; value += 1;")
    assert not looks_like_nymph_source('[package]\nname = "fixture"')
    assert not source_matches("let stable = 1\nlet = 2")
    markdown = "```nymph\nlet mut ignored = 0\n```\n```nym\nlet mut found = 0\n```\n"
    temporary = ROOT / ".language-identity-self-test.md"
    try:
        temporary.write_text(markdown, encoding="utf-8")
        extracted = markdown_sources(temporary)
        assert len(extracted) == 1
        assert {rule for rule, _, _ in source_matches(extracted[0][1])} == {"source-mut"}
    finally:
        temporary.unlink(missing_ok=True)
    for rule in RULES:
        assert any(
            rule.seed_path == owner or rule.seed_path.startswith(f"{owner}/") for owner in rule.paths
        ), f"seed path is outside {rule.category}/{rule.name} ownership"
        assert rule.pattern.search(rule.seed), f"seed did not exercise {rule.category}/{rule.name}"
    masked = mask_comments_and_literals('object.value = 1; "other.value = 2"; // hidden.x = 3')
    mutations = list(RUNTIME_MUTATION.finditer(masked))
    assert len(mutations) == 1


def check() -> int:
    self_test()
    corpus = json.loads((CORPUS / "manifest.json").read_text(encoding="utf-8"))
    if corpus.get("version") != 1:
        print("legacy migration corpus manifest is missing or unsupported", file=sys.stderr)
        return 1
    expected = json.loads(BASELINE.read_text(encoding="utf-8"))
    actual = snapshot()
    if expected.get("version") != 1 or expected.get("inventory") != actual:
        print("language identity cutover inventory changed", file=sys.stderr)
        expected_set = {json.dumps(item, sort_keys=True) for item in expected.get("inventory", [])}
        actual_set = {json.dumps(item, sort_keys=True) for item in actual}
        for item in sorted(actual_set - expected_set):
            print(f"  unexpected: {item}", file=sys.stderr)
        for item in sorted(expected_set - actual_set):
            print(f"  missing:    {item}", file=sys.stderr)
        print("review the path and update the checked-in inventory only when ownership changed", file=sys.stderr)
        return 1
    categories = {item["category"] for item in actual}
    required = {
        "source",
        "docs-source",
        "frozen-legacy-source",
        "generated-source",
        "parser-fixture-source",
        "syntax-ast",
        "sema-stable",
        "hir-emitter",
        "runtime",
        "extension",
        "private-runtime-mutation",
        "release-echo",
        "inert-build",
    }
    missing = sorted(required - categories)
    if missing:
        print(f"inventory lost required ownership categories: {', '.join(missing)}", file=sys.stderr)
        return 1
    print(f"language identity cutover inventory is stable ({len(actual)} reviewed file/rule entries)")
    return 0


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify the checked-in cutover inventory")
    parser.add_argument(
        "--print-inventory",
        action="store_true",
        help="print the observed inventory for reviewed baseline updates",
    )
    args = parser.parse_args()
    if args.check == args.print_inventory:
        parser.error("choose exactly one of --check or --print-inventory")
    if args.print_inventory:
        self_test()
        print(json.dumps({"version": 1, "inventory": snapshot()}, indent=2) + "\n", end="")
        return
    raise SystemExit(check())


if __name__ == "__main__":
    main()
