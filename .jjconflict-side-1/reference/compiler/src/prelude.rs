#[derive(Clone, Copy)]
pub(crate) struct ImplicitPreludeModule {
	pub path: &'static [&'static str],
	pub names: &'static [&'static str],
}

const OPS_PRELUDE_NAMES: &[&str] = &[
	"Plus",
	"Minus",
	"Times",
	"Divide",
	"Remainder",
	"Power",
	"Negate",
	"LeftShift",
	"RightShift",
	"BitAnd",
	"BitOr",
	"BitXor",
	"BitNot",
	"And",
	"Or",
	"Not",
	"Order",
	"Comparable",
	"Equals",
	"Contains",
	"Unwrap",
	"Index",
	"Into",
];

pub(crate) const IMPLICIT_PRELUDE_MODULES: &[ImplicitPreludeModule] = &[
	ImplicitPreludeModule {
		path: &["default"],
		names: &["Default"],
	},
	ImplicitPreludeModule {
		path: &["option"],
		names: &["Option"],
	},
	ImplicitPreludeModule {
		path: &["result"],
		names: &["Result"],
	},
	ImplicitPreludeModule {
		path: &["ops"],
		names: OPS_PRELUDE_NAMES,
	},
];
