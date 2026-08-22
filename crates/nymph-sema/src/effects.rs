//! Canonical checked-effect rows and their separate subset solver.

use std::collections::VecDeque;

use crate::{DefinitionId, GenericParameterId};
use nymph_hir::ty::{EffectAtom as HirEffectAtom, EffectRow as HirEffectRow, Interner, Ty, TyKind};
use rustc_hash::FxHashMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum EffectAtom {
	Nominal(DefinitionId),
	Parameter(GenericParameterId),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct EffectRow(Vec<EffectAtom>);

impl EffectRow {
	#[must_use]
	pub fn new(mut atoms: Vec<EffectAtom>) -> Self {
		atoms.sort();
		atoms.dedup();
		Self(atoms)
	}

	#[must_use]
	pub fn pure() -> Self {
		Self::default()
	}

	#[must_use]
	pub fn atoms(&self) -> &[EffectAtom] {
		&self.0
	}

	#[must_use]
	pub fn union(&self, other: &Self) -> Self {
		Self::new(self.0.iter().chain(&other.0).cloned().collect())
	}

	#[must_use]
	pub fn is_subset_of(&self, upper: &Self) -> bool {
		self
			.0
			.iter()
			.all(|atom| upper.0.binary_search(atom).is_ok())
	}

	#[must_use]
	pub fn difference(&self, other: &Self) -> Self {
		Self::new(
			self
				.0
				.iter()
				.filter(|atom| other.0.binary_search(atom).is_err())
				.cloned()
				.collect(),
		)
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EffectVar(u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectBoundError {
	pub variable: EffectVar,
	pub inferred: EffectRow,
	pub upper: EffectRow,
	pub excess: EffectRow,
}

#[derive(Clone, Debug, Default)]
pub struct EffectSolution {
	rows: Vec<EffectRow>,
}

impl EffectSolution {
	#[must_use]
	pub fn row(&self, variable: EffectVar) -> &EffectRow {
		&self.rows[variable.0 as usize]
	}
}

#[derive(Clone, Debug, Default)]
pub struct EffectSolver {
	lower: Vec<EffectRow>,
	upper: Vec<Option<EffectRow>>,
	edges: Vec<(EffectVar, EffectVar)>,
}

impl EffectSolver {
	#[must_use]
	pub fn variable(&mut self) -> EffectVar {
		let variable = EffectVar(self.lower.len() as u32);
		self.lower.push(EffectRow::pure());
		self.upper.push(None);
		variable
	}

	pub fn require_row(&mut self, row: EffectRow, variable: EffectVar) {
		let lower = &mut self.lower[variable.0 as usize];
		*lower = lower.union(&row);
	}

	pub fn require_subset(&mut self, lower: EffectVar, upper: EffectVar) {
		self.edges.push((lower, upper));
	}

	pub fn set_upper_bound(&mut self, variable: EffectVar, upper: EffectRow) {
		let slot = &mut self.upper[variable.0 as usize];
		*slot = Some(match slot.take() {
			Some(existing) => EffectRow::new(
				existing
					.atoms()
					.iter()
					.filter(|atom| upper.atoms().binary_search(atom).is_ok())
					.cloned()
					.collect(),
			),
			None => upper,
		});
	}

	pub fn solve(&self) -> Result<EffectSolution, Vec<EffectBoundError>> {
		let mut rows = self.lower.clone();
		let mut outgoing = vec![Vec::new(); rows.len()];
		for &(lower, upper) in &self.edges {
			outgoing[lower.0 as usize].push(upper);
		}

		let mut queued = vec![true; rows.len()];
		let mut queue = (0..rows.len()).collect::<VecDeque<_>>();
		while let Some(index) = queue.pop_front() {
			queued[index] = false;
			for &target in &outgoing[index] {
				let target = target.0 as usize;
				let joined = rows[target].union(&rows[index]);
				if joined != rows[target] {
					rows[target] = joined;
					if !queued[target] {
						queued[target] = true;
						queue.push_back(target);
					}
				}
			}
		}

		let errors = self
			.upper
			.iter()
			.enumerate()
			.filter_map(|(index, upper)| {
				let upper = upper.as_ref()?;
				let inferred = &rows[index];
				(!inferred.is_subset_of(upper)).then(|| EffectBoundError {
					variable: EffectVar(index as u32),
					inferred: inferred.clone(),
					upper: upper.clone(),
					excess: inferred.difference(upper),
				})
			})
			.collect::<Vec<_>>();
		if errors.is_empty() {
			Ok(EffectSolution { rows })
		} else {
			Err(errors)
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub enum RecoveredEffectRow {
	Known(EffectRow),
	Poison,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectedEffectContract<'a> {
	Concrete(&'a EffectRow),
	Interface(&'a EffectRow),
	Generic(&'a EffectRow),
}

impl SelectedEffectContract<'_> {
	#[must_use]
	pub fn charged_row(&self) -> &EffectRow {
		match self {
			Self::Concrete(row) | Self::Interface(row) | Self::Generic(row) => row,
		}
	}
}

#[must_use]
pub fn implementation_effects_are_valid(implementation: &EffectRow, contract: &EffectRow) -> bool {
	implementation.is_subset_of(contract)
}

#[derive(Clone, Debug)]
pub(crate) enum EffectCharge {
	Callable(DefinitionId),
	Row(EffectRow),
}

impl crate::check::Checker<'_> {
	pub(crate) fn charge_effect_target(&mut self, target: Option<DefinitionId>) {
		if self.captured_effects.is_some() {
			if let Some(effects) = target
				.as_ref()
				.and_then(|target| self.callable_effect_row(target))
				&& !effects.atoms().is_empty()
			{
				self.captured_effects.as_mut().unwrap().push(effects);
			}
			return;
		}
		let Some(caller) = self.current_effect_caller.clone() else {
			return;
		};
		if let Some(target) = target {
			if self.callable_returns_task(&target) {
				return;
			}
			self
				.effect_charges
				.push((caller, EffectCharge::Callable(target)));
		}
	}

	pub(crate) fn charge_local_effects(&mut self, effects: &nymph_hir::ty::EffectRow) {
		if let Some(captured) = &mut self.captured_effects {
			if !effects.atoms().is_empty() {
				captured.push(effects.clone());
			}
			return;
		}
		let Some(caller) = self.current_effect_caller.clone() else {
			return;
		};
		let row = self.canonical_effect_row(&caller, effects);
		if !row.atoms().is_empty() {
			self.effect_charges.push((caller, EffectCharge::Row(row)));
		}
	}

	pub(crate) fn charge_effect_call(
		&mut self,
		target: Option<DefinitionId>,
		parameters: &[Ty],
		arguments: &[Ty],
	) {
		let Some(target) = target else {
			return;
		};
		let Some(effects) = self.callable_effect_row(&target) else {
			self.charge_effect_target(Some(target));
			return;
		};
		if !effects
			.atoms()
			.iter()
			.any(|effect| matches!(effect, HirEffectAtom::Parameter(_)))
		{
			self.charge_effect_target(Some(target));
			return;
		}

		let mut substitutions = FxHashMap::default();
		for (&parameter, &argument) in parameters.iter().zip(arguments) {
			collect_effect_substitutions(&self.interner, parameter, argument, &mut substitutions);
		}
		let mut instantiated = Vec::new();
		for effect in effects.atoms() {
			match effect {
				HirEffectAtom::Nominal(_) => instantiated.push(effect.clone()),
				HirEffectAtom::Parameter(parameter) => {
					let Some(row) = substitutions.get(parameter) else {
						self.charge_effect_target(Some(target));
						return;
					};
					instantiated.extend(row.atoms().iter().cloned());
				}
			}
		}
		self.charge_local_effects(&HirEffectRow::new(instantiated));
	}

	pub(crate) fn charge_generic_bound_effects(
		&mut self,
		parameter: crate::ParamIdx,
		interface: crate::DefId,
		target: Option<&DefinitionId>,
		parameters: &[Ty],
		arguments: &[Ty],
	) -> bool {
		let Some(target) = target else { return false };
		let Some(effects) = self.callable_effect_row(target) else {
			return false;
		};
		let details = self
			.param_bound_details
			.get(&parameter)
			.or_else(|| self.synthetic_bound_details.get(&parameter));
		let Some(definition) = self.interfaces.get(&interface) else {
			return false;
		};
		let effect_args = if let Some(bound) =
			details.and_then(|bounds| bounds.iter().find(|bound| bound.interface == interface))
		{
			bound.effect_args.clone()
		} else {
			let Some(caller) = &self.current_effect_caller else {
				return false;
			};
			let crate::DeclarationKey::Member { owner, .. } = &caller.key else {
				return false;
			};
			if self.defs.stable(interface) != Some(owner.as_ref()) {
				return false;
			}
			definition
				.generics
				.iter()
				.zip(&definition.generic_kinds)
				.filter_map(|(name, kind)| {
					if *kind != crate::GenericParameterKind::Effect {
						return None;
					}
					let binding = self.params.iter().rev().find_map(|scope| scope.get(name))?;
					Some((
						name.clone(),
						crate::ty::EffectRow::new(vec![HirEffectAtom::Parameter(binding.index)]),
					))
				})
				.collect()
		};
		let mut substitutions = FxHashMap::default();
		for (&parameter, &argument) in parameters.iter().zip(arguments) {
			collect_effect_substitutions(&self.interner, parameter, argument, &mut substitutions);
		}
		let mut instantiated = Vec::new();
		for atom in effects.atoms() {
			match atom {
				HirEffectAtom::Nominal(_) => instantiated.push(atom.clone()),
				HirEffectAtom::Parameter(index) => {
					let row = definition
						.generics
						.get(index.0 as usize)
						.and_then(|name| {
							effect_args
								.iter()
								.find(|(candidate, _)| candidate == name)
								.map(|(_, row)| row)
						})
						.or_else(|| substitutions.get(index));
					let Some(row) = row else { return false };
					instantiated.extend(row.atoms().iter().cloned());
				}
			}
		}
		self.charge_local_effects(&HirEffectRow::new(instantiated));
		true
	}

	fn callable_effect_row(&self, target: &DefinitionId) -> Option<HirEffectRow> {
		self
			.callable_effect_facts(target)
			.map(|(effects, _)| effects)
	}

	fn callable_returns_task(&self, target: &DefinitionId) -> bool {
		self
			.callable_effect_facts(target)
			.is_some_and(|(_, ret)| matches!(self.interner.kind(ret), TyKind::Task { .. }))
	}

	fn callable_effect_facts(&self, target: &DefinitionId) -> Option<(HirEffectRow, Ty)> {
		self
			.defs
			.by_stable(target)
			.and_then(|definition| self.sigs.funcs.get(&definition))
			.map(|signature| (signature.effects.clone(), signature.ret))
			.or_else(|| {
				self
					.inherent
					.impls
					.iter()
					.flat_map(|implementation| implementation.methods.values())
					.find(|method| method.definition.as_ref() == Some(target))
					.map(|method| (method.effects.clone(), method.ret))
			})
			.or_else(|| {
				self
					.impls
					.impls
					.iter()
					.flat_map(|implementation| implementation.methods.values())
					.find(|method| method.definition.as_ref() == Some(target))
					.map(|method| (method.effects.clone(), method.ret))
			})
			.or_else(|| {
				self
					.interfaces
					.values()
					.flat_map(|interface| interface.methods.values())
					.find(|method| method.definition.as_ref() == Some(target))
					.map(|method| (method.effects.clone(), method.ret))
			})
	}

	pub(crate) fn canonical_effect_row(
		&self,
		caller: &DefinitionId,
		effects: &nymph_hir::ty::EffectRow,
	) -> EffectRow {
		let binder_scope = match &caller.key {
			crate::DeclarationKey::Member { .. } => crate::BinderScope::Member,
			_ => crate::BinderScope::Definition,
		};
		EffectRow::new(
			effects
				.atoms()
				.iter()
				.filter_map(|atom| match atom {
					nymph_hir::ty::EffectAtom::Nominal(definition) => self
						.defs
						.stable(*definition)
						.cloned()
						.map(EffectAtom::Nominal),
					nymph_hir::ty::EffectAtom::Parameter(parameter) => Some(EffectAtom::Parameter(
						GenericParameterId::new(caller.binder(binder_scope, 0), parameter.0),
					)),
				})
				.collect(),
		)
	}
}

fn collect_effect_substitutions(
	interner: &Interner,
	expected: Ty,
	actual: Ty,
	substitutions: &mut FxHashMap<crate::ParamIdx, HirEffectRow>,
) {
	match (interner.kind(expected), interner.kind(actual)) {
		(
			TyKind::Fn {
				params: expected_parameters,
				ret: expected_return,
				effects: expected_effects,
			},
			TyKind::Fn {
				params: actual_parameters,
				ret: actual_return,
				effects: actual_effects,
			},
		) => {
			for effect in expected_effects.atoms() {
				if let HirEffectAtom::Parameter(parameter) = effect {
					let existing = substitutions.entry(*parameter).or_default();
					*existing = HirEffectRow::new(
						existing
							.atoms()
							.iter()
							.chain(actual_effects.atoms())
							.cloned()
							.collect(),
					);
				}
			}
			for (&expected, &actual) in expected_parameters.iter().zip(actual_parameters) {
				collect_effect_substitutions(interner, expected, actual, substitutions);
			}
			collect_effect_substitutions(interner, *expected_return, *actual_return, substitutions);
		}
		(TyKind::List(expected), TyKind::List(actual)) => {
			collect_effect_substitutions(interner, *expected, *actual, substitutions);
		}
		(
			TyKind::Task {
				output: expected_output,
				effects: expected_effects,
			},
			TyKind::Task {
				output: actual_output,
				effects: actual_effects,
			},
		) => {
			for effect in expected_effects.atoms() {
				if let HirEffectAtom::Parameter(parameter) = effect {
					let existing = substitutions.entry(*parameter).or_default();
					*existing = HirEffectRow::new(
						existing
							.atoms()
							.iter()
							.chain(actual_effects.atoms())
							.cloned()
							.collect(),
					);
				}
			}
			collect_effect_substitutions(interner, *expected_output, *actual_output, substitutions);
		}
		(TyKind::Tuple(expected), TyKind::Tuple(actual)) => {
			for (&expected, &actual) in expected.iter().zip(actual) {
				collect_effect_substitutions(interner, expected, actual, substitutions);
			}
		}
		(TyKind::Map(expected_key, expected_value), TyKind::Map(actual_key, actual_value)) => {
			collect_effect_substitutions(interner, *expected_key, *actual_key, substitutions);
			collect_effect_substitutions(interner, *expected_value, *actual_value, substitutions);
		}
		_ => {}
	}
}
