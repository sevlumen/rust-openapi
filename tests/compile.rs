#[test]
fn openapi_derive_compile_contract() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/openapi_pass.rs");
    tests.compile_fail("tests/ui/openapi_fail.rs");
    tests.compile_fail("tests/ui/schema_name_fail.rs");
    tests.compile_fail("tests/ui/generic_fail.rs");
    tests.compile_fail("tests/ui/rename_split_fail.rs");
    tests.compile_fail("tests/ui/rename_all_split_fail.rs");
    tests.compile_fail("tests/ui/variant_attr_fail.rs");
}
