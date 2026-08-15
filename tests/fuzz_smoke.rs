use bytes::Bytes;
use oas_rs::{App, Method, Path};

async fn echo_path(Path(value): Path<String>) -> String {
    value
}

#[tokio::test]
async fn deterministic_path_fuzz_smoke_never_panics() {
    let mut app = App::new();
    app.get("/fuzz/{value}", echo_path);

    let alphabet = b"abcXYZ0123456789%_-";
    let mut state = 0x9e3779b9_u32;
    for iteration in 0..20_000 {
        let mut value = String::with_capacity(24);
        for _ in 0..(state as usize % 24) {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            value.push(alphabet[(state as usize) % alphabet.len()] as char);
        }
        let uri = format!("/fuzz/{value}");
        let response = app
            .oneshot(Method::GET, &uri, &[], Some(Bytes::new()))
            .await;
        assert!(
            matches!(response.status().as_u16(), 200 | 400 | 404),
            "iteration {iteration}"
        );
    }
}
