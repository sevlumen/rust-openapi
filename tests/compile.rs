#[test]
fn openapi_derive_compile_contract() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/openapi_pass.rs");
    tests.compile_fail("tests/ui/openapi_fail.rs");
    tests.pass("examples/hello.rs");
    tests.pass("examples/users-api.rs");
}
