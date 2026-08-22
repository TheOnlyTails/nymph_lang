use std::sync::Arc;

use nymph_sema::{
	EntryMode, ModuleEnvironment, ModuleIdentity, ModuleOrigin, RangeDecision, RangeEvidence,
	RangeOperation, SemanticEnvironment,
};

fn analyzed(
	source: &str,
) -> (
	Arc<nymph_ast::decl::Module>,
	nymph_sema::SemanticCheckResult,
) {
	let parsed = nymph_syntax::parse_module(source, "range_proofs");
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	let module = Arc::new(parsed.tree);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("standalone".into()),
		project: "standalone".into(),
		path: "range_proofs".into(),
	};
	let environment =
		SemanticEnvironment::from_modules(identity.clone(), &[] as &[Arc<ModuleEnvironment>]).unwrap();
	let checked = nymph_sema::check_module_with_environment(
		module.clone(),
		identity,
		&environment,
		EntryMode::Library,
	);
	(module, checked)
}

fn checked(
	source: &str,
) -> (
	Arc<nymph_ast::decl::Module>,
	nymph_sema::SemanticCheckResult,
) {
	let result = analyzed(source);
	assert!(
		result.1.diagnostics.is_empty(),
		"{:?}",
		result.1.diagnostics
	);
	result
}

fn proofs(source: &str) -> Vec<nymph_sema::RangeProof> {
	let (_, checked) = checked(source);
	checked
		.analysis
		.checked
		.annotations
		.range_proofs()
		.map(|(_, proof)| proof.clone())
		.collect()
}

fn runtime_proofs(source: &str) -> Arc<[(nymph_sema::BodyNodeId, nymph_sema::RangeProof)]> {
	let (module, checked) = checked(source);
	let identity = ModuleIdentity {
		origin: ModuleOrigin::Project("standalone".into()),
		project: "standalone".into(),
		path: "range_proofs".into(),
	};
	let facts = nymph_sema::Checked {
		diags: vec![],
		facts: checked.analysis.checked.as_ref().clone(),
	};
	let headers = nymph_sema::declared_headers(identity.clone(), &module);
	let interface =
		nymph_sema::extract_module_interface(identity, &module, &facts, &headers).unwrap();
	let definitions = nymph_sema::runtime_definitions(&module, &facts.facts, &interface).unwrap();
	definitions
		.into_iter()
		.find_map(|definition| match definition.payload {
			nymph_sema::RuntimePayload::NymphBody(body) => Some(body.annotations.range_proofs),
			_ => None,
		})
		.expect("Nymph body range proofs")
}

#[test]
fn interval_exclusion_and_signed_pair_proofs_are_auditable() {
	let proofs = proofs(
		"interface Index<Key, Output> { func index(key: Key): Output }
		impl<T> Index<Key = int, Output = T> for #[T] { external func index(key: int): T }
		impl<T> #[T] { external(length) func length(): uint }
		func operations(values: #[int], index: uint, divisor: int): int = {
			let one = 1
			let safe = one + 2
			if (divisor != 0) {
				let quotient: float = 10 / divisor
				if (index >= 0 && index < values.length()) values[index as int] else safe
			} else safe
		}
		func negative(values: #[int], index: int): int = {
			if (index <= -1 && index >= -(values.length() as int)) values[index] else 0
		}
		func bounded_slice(values: #[int], start: int, end: int): #[int] = {
			if (start >= -(values.length() as int) && start <= values.length() as int
				&& end >= -(values.length() as int) && end < values.length() as int)
				values[start..=end]
			else #[]
		}",
	);
	assert!(
		proofs.iter().all(nymph_sema::RangeProof::audit),
		"{proofs:#?}"
	);
	assert!(proofs.iter().any(|proof| {
		proof.operation == RangeOperation::Arithmetic && proof.decision == RangeDecision::Safe
	}));
	assert!(proofs.iter().any(|proof| {
		proof.operation == RangeOperation::Division
			&& proof.decision == RangeDecision::Safe
			&& proof.evidence.contains(&RangeEvidence::Excluded {
				operand: 1,
				value: 0,
			})
	}));
	assert!(proofs.iter().any(|proof| {
		proof.operation == RangeOperation::Index
			&& proof.decision == RangeDecision::Safe
			&& proof
				.evidence
				.iter()
				.any(|evidence| matches!(evidence, RangeEvidence::SignedPairBound { .. }))
	}));
	assert!(proofs.iter().any(|proof| {
		proof.operation == RangeOperation::Index
			&& proof.decision == RangeDecision::Safe
			&& proof.evidence.iter().any(|evidence| {
				matches!(
					evidence,
					RangeEvidence::SignedPairBound {
						left_sign: -1,
						right_sign: -1,
						..
					}
				)
			})
	}));
	assert!(proofs.iter().any(|proof| {
		proof.operation == RangeOperation::SliceInclusive
			&& proof.decision == RangeDecision::Safe
			&& proof.evidence.iter().any(|evidence| {
				matches!(
					evidence,
					RangeEvidence::SymbolicSliceBound {
						lower: true,
						upper: true,
						..
					}
				)
			})
	}));
}

#[test]
fn uncertain_operations_retain_unknown_decisions() {
	let proofs = proofs(
		"interface Index<Key, Output> { func index(key: Key): Output }
		impl<T> Index<Key = int, Output = T> for #[T] { external func index(key: int): T }
		func operations(values: #[int], index: int, divisor: int): float = {
			let item = values[index]
			item / divisor
		}",
	);
	assert!(proofs.iter().any(|proof| {
		proof.operation == RangeOperation::Index && proof.decision == RangeDecision::Unknown
	}));
	assert!(proofs.iter().any(|proof| {
		proof.operation == RangeOperation::Division && proof.decision == RangeDecision::Unknown
	}));
}

#[test]
fn shift_and_conversion_boundaries_publish_safe_and_unknown_decisions() {
	let proofs = proofs(
		"func operations(value: int, count: int): uint = {
			let shifted = 1 << 3
			let uncertain_shift = value << count
			let converted = 1 as uint
			let uncertain_conversion = value as uint
			uncertain_conversion
		}",
	);
	for operation in [RangeOperation::Shift, RangeOperation::Conversion] {
		assert!(
			proofs.iter().any(|proof| {
				proof.operation == operation && proof.decision == RangeDecision::Safe && proof.audit()
			}),
			"missing safe {operation:?}: {proofs:#?}"
		);
		assert!(
			proofs
				.iter()
				.any(|proof| { proof.operation == operation && proof.decision == RangeDecision::Unknown }),
			"missing unknown {operation:?}: {proofs:#?}"
		);
	}
}

#[test]
fn definite_boundary_failures_publish_auditable_invalid_decisions() {
	let (_, checked) = analyzed(
		"func overflow(): int = 9223372036854775807 + 1
		func zero(): float = 1 / 0
		func shift(): int = 1 << 64
		func conversion(): uint = (-1) as uint
		func index(): int = #[1, 2][2]
		func slice(): #[int] = #[1, 2][0..3]",
	);
	let proofs = checked
		.analysis
		.checked
		.annotations
		.range_proofs()
		.map(|(_, proof)| proof)
		.collect::<Vec<_>>();
	for operation in [
		RangeOperation::Arithmetic,
		RangeOperation::Division,
		RangeOperation::Shift,
		RangeOperation::Conversion,
		RangeOperation::Index,
		RangeOperation::SliceExclusive,
	] {
		assert!(
			proofs.iter().any(|proof| {
				proof.operation == operation && proof.decision == RangeDecision::Invalid && proof.audit()
			}),
			"missing invalid {operation:?}: {proofs:#?}"
		);
	}
}

#[test]
fn body_local_ids_key_stable_replayable_annotations() {
	let source = "func value(): int = { let one = 1 one + 2 }";
	let proofs = runtime_proofs(source);
	let (node, proof) = proofs
		.iter()
		.find(|(_, proof)| proof.operation == RangeOperation::Arithmetic)
		.expect("arithmetic proof");
	assert_eq!(proof.decision, RangeDecision::Safe);
	assert!(proof.audit());
	assert!(
		proofs
			.iter()
			.all(|(candidate, _)| candidate != &nymph_sema::BodyNodeId(node.0 + 100))
	);

	let formatted = runtime_proofs(
		"func value(): int = {\n\t\t// Formatting does not change stable body identity.\n\t\tlet one = 1\n\t\tone + 2\n\t}",
	);
	assert_eq!(proofs, formatted);
}

#[test]
fn audit_rejects_a_tampered_decision_without_supporting_evidence() {
	let proof = nymph_sema::RangeProof {
		operation: RangeOperation::Arithmetic,
		decision: RangeDecision::Invalid,
		evidence: Arc::from([]),
	};
	assert!(!proof.audit());
}
