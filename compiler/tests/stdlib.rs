use ariadne::Source;
use assertables::assert_some;
use nymph_compiler::types::type_error_to_report;
use std::ffi::OsStr;
use std::fs::read_to_string;

#[test]
fn stdlib_tests() {
	for file in glob::glob("../stdlib/src/**/*.nym")
		.unwrap()
		.filter_map(Result::ok)
	{
		let name = file.file_name().and_then(OsStr::to_str).unwrap();
		let source = read_to_string(file.clone()).unwrap();
		let abs_path = file.canonicalize().unwrap_or_else(|_| file.clone());

		let (module, reports) = nymph_compiler::parse(name.into(), source.as_str());
		if !reports.is_empty() {
			for report in reports {
				report
					.finish()
					.eprint((name.into(), Source::from(source.clone())))
					.unwrap();
			}
			panic!("Parse errors in {name}");
		}
		let module = assert_some!(module, "Failed to parse {name}");

		// Type-check the module with file path for import resolution
		if let Err(error) = nymph_compiler::types::type_check_with_path(module.inner(), abs_path) {
			let error_file: ecow::EcoString = error
				.file_path()
				.unwrap_or_else(|| name.into());
			let error_source = if error_file == name {
				source.clone()
			} else {
				read_to_string(error_file.as_str()).unwrap_or_default()
			};
			type_error_to_report(name.into(), &error)
				.eprint((error_file, Source::from(error_source)))
				.unwrap();
			panic!("Type check failed for {name}");
		}
	}
}
