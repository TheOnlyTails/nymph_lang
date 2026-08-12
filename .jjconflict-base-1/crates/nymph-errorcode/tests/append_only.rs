use nymph_diagnostics::ErrorCode;
use nymph_errorcode::ErrorCode;

#[derive(ErrorCode)]
#[error_code(8)]
enum StableCatalog {
	First,
	Structured { _value: usize },
	Tuple(#[allow(dead_code)] usize),
}

#[test]
fn catalog_positions_allocate_append_only_codes() {
	assert_eq!(StableCatalog::First.code(), 8000);
	assert_eq!(StableCatalog::Structured { _value: 1 }.code(), 8001);
	assert_eq!(StableCatalog::Tuple(1).code(), 8002);
}
