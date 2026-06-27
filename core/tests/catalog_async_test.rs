//! Regression: the catalog's blocking HTTP fetch must not panic when called
//! from inside bone's async runtime. `reqwest::blocking` builds a nested tokio
//! runtime, which panics on drop if created within an async context; the client
//! offloads the GET to a dedicated thread to avoid that. Reproduce by driving a
//! remote fetch from within a `tokio` runtime against an unreachable URL.

use bone_core::ext::catalog;

#[test]
fn fetch_index_from_async_context_does_not_panic() {
    // Point at an unreachable http endpoint so `fetch_remote` runs (the panicky
    // path) but returns quickly. SAFETY: single-test file, no other threads.
    unsafe {
        std::env::set_var("BONE_CATALOG_URL", "http://127.0.0.1:1");
        std::env::set_var(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("bone-catalog-async-cfg"),
        );
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Before the fix this panicked: "Cannot drop a runtime in a context where
    // blocking is not allowed." With no cache present, an unreachable URL yields
    // an empty index.
    let entries = rt.block_on(async { catalog::fetch_index() });
    assert!(entries.is_empty(), "unreachable catalog yields no entries");
}
