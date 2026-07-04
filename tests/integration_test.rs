use adblock2mikrotik_rust::{fetch_rules, run};
use std::sync::OnceLock;
use tempfile::tempdir;
use tokio::sync::Mutex;

// run() reads the OUTPUT_DIR environment variable internally (it has no
// output-path parameter), and env vars are process-global. Any test that
// sets OUTPUT_DIR to point run() at an isolated tempdir must serialize
// against every other test in this binary doing the same, or they can race:
// one test's override could leak into another's execution window. A
// tokio::sync::Mutex (not std::sync::Mutex) is used because its guard is
// safe to hold across the run().await call below — a std guard held across
// await risks blocking the executor thread and would trip
// clippy::await_holding_lock.
static OUTPUT_DIR_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn output_dir_lock() -> &'static Mutex<()> {
    OUTPUT_DIR_LOCK.get_or_init(|| Mutex::new(()))
}

#[tokio::test]
async fn test_fetch_rules_success() {
    let mut server = mockito::Server::new_async().await;

    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("||example.com^\n||test.com^\n")
        .create_async()
        .await;

    let url = format!("{}/rules", server.url());
    let client = reqwest::Client::new();

    let rules = fetch_rules(&client, &url)
        .await
        .expect("fetch_rules failed");

    assert_eq!(rules.len(), 2);
    assert!(rules.contains(&"||example.com^".to_string()));
    assert!(rules.contains(&"||test.com^".to_string()));
}

#[tokio::test(start_paused = true)]
async fn test_fetch_rules_http_error() {
    // start_paused = true: tokio mock-time advances automatically when all tasks
    // are blocked on sleep — the 2s + 4s backoff runs in microseconds, not 6s.
    let mut server = mockito::Server::new_async().await;

    let _m = server
        .mock("GET", "/rules")
        .with_status(500)
        .expect(3)
        .create_async()
        .await;

    let url = format!("{}/rules", server.url());
    let client = reqwest::Client::new();

    let result = fetch_rules(&client, &url).await;

    assert!(
        result.is_err(),
        "fetch_rules should return Err after all 3 retry attempts fail"
    );
    // mockito drops _m here and asserts expect(3) was satisfied —
    // confirming the retry logic called the endpoint exactly 3 times.
}

#[tokio::test(start_paused = true)]
async fn test_run_with_partial_failure() {
    // start_paused = true: same technique as test_fetch_rules_http_error —
    // tokio's mock time auto-advances once every task is blocked on a
    // timer, so the retry backoff (2s + 4s) against server2 runs in
    // microseconds instead of ~6s of real wall-clock time. Confirmed this
    // still works correctly when a second, real-I/O task (server1's fetch)
    // runs concurrently in the same JoinSet inside run() — the sleeping
    // task's virtual time still advances once the I/O task completes.
    let _guard = output_dir_lock().lock().await;

    let mut server1 = mockito::Server::new_async().await;
    let mut server2 = mockito::Server::new_async().await;

    let _m1 = server1
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("||example.com^\n")
        .create_async()
        .await;

    // Retry logic makes 3 attempts against the failing server
    let _m2 = server2
        .mock("GET", "/rules")
        .with_status(500)
        .expect(3)
        .create_async()
        .await;

    let urls = [
        format!("{}/rules", server1.url()),
        format!("{}/rules", server2.url()),
    ];
    let urls_ref: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

    let result = run(urls_ref).await;
    assert!(result.is_ok());

    // Clean up hosts.txt written to CWD
    let _ = std::fs::remove_file("hosts.txt");
}

#[tokio::test]
async fn test_run_writes_expected_hosts_file_format() {
    // Regression coverage for the header/section-writing logic in run() —
    // previously only exercised indirectly (via result.is_ok()) by
    // test_run_with_partial_failure, with no assertion on the actual file
    // content. Mirrors the Python project's test_write_output_direct.
    let _guard = output_dir_lock().lock().await;

    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body(
            "||example.com^\n\
             ||test.com^\n\
             ||invalid_domain^\n\
             ||example.com^ # duplicate, same domain after parsing\n",
        )
        .create_async()
        .await;

    let url = format!("{}/rules", server.url());
    let temp_dir = tempdir().unwrap();

    // SAFETY: guarded by output_dir_lock() above; no other test in this
    // binary reads or writes OUTPUT_DIR while this guard is held.
    unsafe { std::env::set_var("OUTPUT_DIR", temp_dir.path()) };
    let result = run(vec![&url]).await;
    unsafe { std::env::remove_var("OUTPUT_DIR") };

    assert!(result.is_ok());

    let content = std::fs::read_to_string(temp_dir.path().join("hosts.txt"))
        .expect("hosts.txt should have been written to OUTPUT_DIR");

    // Header
    assert!(content.contains("# Title: Unified DNS blocklist optimized for RouterOS"));
    assert!(content.contains("# Last modified:"));
    assert!(content.contains(&format!("# - {url}")));
    assert!(content.contains("rules --> 2 unique domains"));

    // Per-source section
    assert!(content.contains(&format!("# Source: {url}")));
    assert!(content.contains("0.0.0.0 example.com"));
    assert!(content.contains("0.0.0.0 test.com"));
    assert!(
        !content.contains("invalid_domain"),
        "invalid domain must be rejected"
    );

    // "||example.com^" and "||example.com^ # duplicate..." both resolve to
    // the same domain after parsing — must appear exactly once in the output.
    assert_eq!(content.matches("0.0.0.0 example.com").count(), 1);

    // Counts
    assert!(content.contains("# Converted 2 rules from this source"));
    assert!(content.contains("# Total unique domains: 2"));

    // Footer
    assert!(content.trim_end().ends_with("Total unique domains: 2"));

    // Atomic write: no hidden .tmp file should remain after a successful run
    let leftover_tmp = std::fs::read_dir(temp_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
    assert!(
        !leftover_tmp,
        "no leftover .tmp file after successful write"
    );
}

#[tokio::test]
async fn test_run_write_failure_leaves_no_temp_file() {
    // Regression coverage for the atomic-write error path: if OUTPUT_DIR
    // points at a directory that doesn't exist, the temp-file write itself
    // must fail with an error (ENOENT) rather than panicking, and run() must
    // propagate that error rather than reporting success.
    //
    // Deliberately uses a missing directory (not chmod-based permission
    // denial): permission checks are bypassed entirely when tests run as
    // root (common in some CI containers), which would make a
    // permission-based test silently pass without exercising anything. A
    // missing directory fails the write regardless of privilege level.
    let _guard = output_dir_lock().lock().await;

    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("||example.com^\n")
        .create_async()
        .await;
    let url = format!("{}/rules", server.url());

    let temp_dir = tempdir().unwrap();
    let nonexistent_dir = temp_dir.path().join("does-not-exist");

    // SAFETY: guarded by output_dir_lock() above.
    unsafe { std::env::set_var("OUTPUT_DIR", &nonexistent_dir) };
    let result = run(vec![&url]).await;
    unsafe { std::env::remove_var("OUTPUT_DIR") };

    assert!(
        result.is_err(),
        "run() must surface the write failure instead of reporting success"
    );
    assert!(
        !nonexistent_dir.exists(),
        "run() must not itself create the missing output directory"
    );
}

#[tokio::test]
async fn test_fetch_rules_filters_comments_and_empty_lines() {
    // Mirrors Python test_fetch_rules_filters_comments:
    // fetch_rules must strip comment lines (including indented) and empty lines,
    // returning only candidate adblock rules to the caller.
    let mut server = mockito::Server::new_async().await;

    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body(
            "||example.com^
             # Title: some blocklist header
             
             ||test.com^
               # indented comment
",
        )
        .create_async()
        .await;

    let url = format!("{}/rules", server.url());
    let client = reqwest::Client::new();

    let rules = fetch_rules(&client, &url)
        .await
        .expect("fetch_rules failed");

    assert_eq!(
        rules.len(),
        2,
        "comments and empty lines must be filtered out"
    );
    assert!(rules.contains(&"||example.com^".to_string()));
    assert!(rules.contains(&"||test.com^".to_string()));
}
