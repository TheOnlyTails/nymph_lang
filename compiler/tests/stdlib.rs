use std::ffi::OsStr;
use std::fs::read_to_string;
use assertables::{assert_is_empty, assert_some};

#[test]
fn stdlib_tests() {
	for file in glob::glob("../stdlib/src/**/*.nym")
		.unwrap()
		.filter_map(Result::ok)
	{
		let name = file.file_name().and_then(OsStr::to_str).unwrap();
		let source = read_to_string(file.clone()).unwrap();

		let (module, reports) = nymph_compiler::parse(name, source.as_str());
		assert_is_empty!(reports);
		assert_some!(module);
	}
}
