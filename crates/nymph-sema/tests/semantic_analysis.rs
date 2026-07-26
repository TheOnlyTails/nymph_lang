use std::sync::Arc;

use nymph_ast::decl::Declaration;
use nymph_sema::{ModuleAnnotations, SemanticAnalysis, check_module};
use nymph_syntax::parse_module;

#[test]
fn semantic_analysis_owns_source_and_node_annotations_without_diagnostics() {
	let parsed = parse_module("func value(): string = 1", "analysis.nymph");
	assert!(parsed.diagnostics.is_empty());
	let module = parsed.tree;
	let body_id = match &module.members[0] {
		Declaration::Func { body, .. } => body.id,
		other => panic!("expected function, got {other:?}"),
	};
	let checked = check_module(&module);
	let diagnostics = checked.diags;
	assert!(!diagnostics.is_empty());

	let analysis = SemanticAnalysis {
		source: Arc::new(module.clone()),
		annotations: ModuleAnnotations::from(checked.annotations.clone()),
	};
	let cloned = analysis.clone();

	assert_eq!(analysis, cloned);
	assert_eq!(analysis.source.as_ref(), &module);
	assert!(analysis.annotations.get(body_id).is_some());
	assert!(format!("{analysis:?}").contains("SemanticAnalysis"));
	assert!(
		!diagnostics.is_empty(),
		"diagnostics remain a separate result"
	);
}
