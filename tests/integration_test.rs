use adblock2mikrotik_rust::{fetch_rules, resolve_output_file, run, run_with_options, source_name};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Hard cap on simultaneously in-flight source fetches, mirroring the Python
/// port's `ThreadPoolExecutor(max_workers=min(len(urls), 3))`. Duplicated here
/// as a literal so the test asserts the public contract independently of the
/// library's internal constant.
const MAX_CONCURRENT_FETCHES: usize = 3;

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

    // fetch_rules returns validated domains: conversion happens during
    // streaming, so raw rule text is never returned or retained.
    assert_eq!(rules.len(), 2);
    assert!(rules.contains(&"example.com".to_string()));
    assert!(rules.contains(&"test.com".to_string()));
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
async fn test_run_with_partial_failure_fails_the_run() {
    // A configured source that fails must fail the entire run (no partial
    // hosts.txt published), mirroring the Python port's failed_sources
    // handling. start_paused = true: same technique as
    // test_fetch_rules_http_error — tokio's mock time auto-advances once
    // every task is blocked on a timer, so the retry backoff (2s + 4s)
    // against server2 runs in microseconds instead of ~6s of real wall-clock
    // time. Confirmed this still works correctly when a second, real-I/O task
    // (server1's fetch) runs concurrently in the same JoinSet inside run() —
    // the sleeping task's virtual time still advances once the I/O task
    // completes.
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
    assert!(
        result.is_err(),
        "a source that fails to fetch must fail the whole run"
    );
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
    assert!(rules.contains(&"example.com".to_string()));
    assert!(rules.contains(&"test.com".to_string()));
}

// ---------------------------------------------------------------------------
// Atomic-write invariant: a failed write must never damage the previous file
// ---------------------------------------------------------------------------

/// Names of any leftover `*.tmp` files in `dir`.
fn tmp_leftovers(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect()
}

#[tokio::test]
async fn test_run_write_failure_preserves_existing_file() {
    // Mirrors the Python port's test_write_output_preserves_existing_file_on_failure.
    // This is the core guarantee of the atomic write: RouterOS may be polling
    // this file over HTTP at any moment, so a failed run must leave the last
    // good list byte-for-byte intact rather than truncating or half-writing it.
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
    let output = temp_dir.path().join("hosts.txt");
    std::fs::write(&output, "previous good content\n").expect("seed output file");

    // Point OUTPUT_DIR at a path that cannot be written to: a *file* where the
    // temp file needs to live as a sibling. ENOTDIR fails the temp write at any
    // privilege level, whereas a chmod-based denial is bypassed under root.
    let blocked = temp_dir.path().join("blocked");
    std::fs::write(&blocked, "i am a file, not a directory\n").unwrap();

    // SAFETY: guarded by output_dir_lock() above.
    unsafe { std::env::set_var("OUTPUT_DIR", &blocked) };
    let result = run(vec![&url]).await;
    unsafe { std::env::remove_var("OUTPUT_DIR") };

    assert!(result.is_err(), "run() must surface the write failure");
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "previous good content\n",
        "a failed write must leave the previous hosts.txt untouched"
    );
    let leftovers = tmp_leftovers(temp_dir.path());
    assert!(
        leftovers.is_empty(),
        "no leftover .tmp file after a failed write, found: {leftovers:?}"
    );
}

// ---------------------------------------------------------------------------
// --dry-run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_run_dry_run_does_not_write() {
    // Mirrors the Python port's test_cli_dry_run_does_not_write: a dry run
    // performs the whole pipeline but must not touch the output file, so it can
    // validate a config before committing to it.
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
    let output = temp_dir.path().join("hosts.txt");
    std::fs::write(&output, "previous good content\n").unwrap();

    // SAFETY: guarded by output_dir_lock() above.
    unsafe { std::env::set_var("OUTPUT_DIR", temp_dir.path()) };
    let result = run_with_options(&[url.as_str()], output.clone(), true).await;
    unsafe { std::env::remove_var("OUTPUT_DIR") };

    assert!(result.is_ok(), "a dry run that fetched fine must succeed");
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "previous good content\n",
        "--dry-run must not write the output file"
    );
    let leftovers = tmp_leftovers(temp_dir.path());
    assert!(
        leftovers.is_empty(),
        "a dry run must not create a temp file, found: {leftovers:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn test_run_dry_run_still_fails_on_unfetchable_source() {
    // Mirrors the Python port's test_cli_dry_run_still_fails_on_unfetchable_source:
    // a dry run reports the same failures as a real run, so the documented
    // `--dry-run -c new.toml && mv new.toml config.toml` pattern is safe.
    let _guard = output_dir_lock().lock().await;

    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/rules")
        .with_status(500)
        .expect(3)
        .create_async()
        .await;
    let url = format!("{}/rules", server.url());

    let temp_dir = tempdir().unwrap();
    let output = temp_dir.path().join("hosts.txt");

    // SAFETY: guarded by output_dir_lock() above.
    unsafe { std::env::set_var("OUTPUT_DIR", temp_dir.path()) };
    let result = run_with_options(&[url.as_str()], output.clone(), true).await;
    unsafe { std::env::remove_var("OUTPUT_DIR") };

    assert!(
        result.is_err(),
        "an unfetchable source must fail a dry run too"
    );
    assert!(
        !output.exists(),
        "a failing dry run must not create the output file"
    );
}

// ---------------------------------------------------------------------------
// A source that answers but holds nothing convertible
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_run_source_without_supported_rules_fails() {
    // Mirrors the Python port's
    // test_main_source_without_supported_rules_exits_nonzero. A source that
    // replies 200 with no ||domain^ rule would otherwise produce an empty
    // hosts.txt and exit 0, silently replacing a good list with nothing.
    let _guard = output_dir_lock().lock().await;

    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("# only comments\n! unsupported syntax\n")
        .create_async()
        .await;
    let url = format!("{}/rules", server.url());

    let temp_dir = tempdir().unwrap();
    let output = temp_dir.path().join("hosts.txt");

    // SAFETY: guarded by output_dir_lock() above.
    unsafe { std::env::set_var("OUTPUT_DIR", temp_dir.path()) };
    let result = run(vec![&url]).await;
    unsafe { std::env::remove_var("OUTPUT_DIR") };

    assert!(result.is_err(), "a source with no valid rules must fail");
    assert!(
        !output.exists(),
        "no hosts.txt may be written when nothing could be converted"
    );
}

// ---------------------------------------------------------------------------
// Helpers now shared with the CLI
// ---------------------------------------------------------------------------

#[test]
fn test_source_name() {
    // Mirrors the Python port's test_source_name.
    assert_eq!(source_name("https://example.com/list1.txt"), "list1.txt");
    assert_eq!(
        source_name("https://example.com/a/b/list2.txt"),
        "list2.txt"
    );
    // A URL ending in "/" has an empty last segment, as with rpartition("/").
    assert_eq!(source_name("https://example.com/"), "");
}

#[test]
fn test_resolve_output_file_prefers_explicit_output() {
    // --output wins over $OUTPUT_DIR, which wins over the CWD default.
    let explicit = PathBuf::from("/tmp/explicit/blocklist.txt");
    assert_eq!(
        resolve_output_file(Some(&explicit)),
        explicit,
        "an explicit --output must take precedence over OUTPUT_DIR"
    );

    // This test only reads OUTPUT_DIR; the tests that mutate it hold
    // output_dir_lock(), and a read is unaffected by the ordering.
    match std::env::var("OUTPUT_DIR") {
        Ok(dir) => assert_eq!(
            resolve_output_file(None),
            PathBuf::from(dir).join("hosts.txt"),
            "without --output, OUTPUT_DIR decides"
        ),
        Err(_) => assert_eq!(
            resolve_output_file(None),
            PathBuf::from("hosts.txt"),
            "with neither --output nor OUTPUT_DIR, hosts.txt lands in the CWD"
        ),
    }
}

// ---------------------------------------------------------------------------
// Concurrency cap
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_caps_concurrent_fetches() {
    // run() must never have more than MAX_CONCURRENT_FETCHES (3) sources in
    // flight at once, matching the Python port's
    // `ThreadPoolExecutor(max_workers=min(len(urls), 3))`. Without the cap a
    // config with many lists opens one connection per list simultaneously,
    // which invites rate-limiting into a failed run.
    const SOURCES: usize = 8;

    let mut server = mockito::Server::new_async().await;

    // Each mock body handler bumps an in-flight counter on entry and lowers it
    // on exit, holding the response open long enough for the other sources to
    // pile up if there is no cap. peak records the high-water mark.
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut mocks = Vec::new();

    for i in 0..SOURCES {
        let in_flight = Arc::clone(&in_flight);
        let peak = Arc::clone(&peak);
        mocks.push(
            server
                .mock("GET", format!("/list{i}").as_str())
                .with_status(200)
                .with_header("content-type", "text/plain")
                .with_chunked_body(move |w| {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    w.write_all(format!("||site{i}.com^\n").as_bytes())?;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .create_async()
                .await,
        );
    }

    let temp_dir = tempdir().unwrap();
    let urls: Vec<String> = (0..SOURCES)
        .map(|i| format!("{}/list{i}", server.url()))
        .collect();
    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();

    let output = temp_dir.path().join("hosts.txt");
    let result = run_with_options(&url_refs, output, false).await;

    assert!(result.is_ok(), "all sources are reachable: {result:?}");
    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        observed_peak <= MAX_CONCURRENT_FETCHES,
        "at most {MAX_CONCURRENT_FETCHES} fetches may be in flight at once, saw {observed_peak}"
    );
}

// ---------------------------------------------------------------------------
// Read timeout
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn test_fetch_rules_gives_up_on_a_stalled_source() {
    // A source that sends headers and then stalls forever must not hang the run.
    // connect_timeout only bounds establishing the connection, so without
    // read_timeout (the Python port's timeout=(3, 10)) the task would wait
    // indefinitely and a scheduled run would never finish.
    //
    // The upstream is a real listener (not mockito, which cannot stall), and
    // start_paused advances the clock automatically while the only task waits,
    // so the 10s read timeout is observed instantly instead of in real time.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // Send a valid response header promising a body, then never finish
            // it: the client blocks waiting for the declared Content-Length.
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n||stalled.com^\n")
                .await;
            std::mem::forget(socket);
        }
    });

    let url = format!("http://{addr}/rules");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Each attempt must end (via the read timeout) rather than hang; 3 attempts
    // plus backoff all run on the paused clock.
    let result = fetch_rules(&client, &url).await;
    assert!(
        result.is_err(),
        "a stalled source must end in an error, not an indefinite wait"
    );
}

// ---------------------------------------------------------------------------
// Streaming: chunk boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_fetch_rules_handles_rules_split_across_chunks() {
    // The body is written one byte at a time, so every multi-byte character and
    // every line is guaranteed to straddle a chunk boundary. This is the case
    // that distinguishes real streaming from buffering: a naive implementation
    // either corrupts split UTF-8 sequences or emits half a line twice.
    let mut server = mockito::Server::new_async().await;

    // The comment holds multi-byte text (é is 2 bytes, 例/え are 3) so the
    // decoder has to stitch split sequences back together. Valid domains are
    // ASCII-only by design (RFC 1123 subset, matching the Python port), so the
    // unicode *domains* below must be rejected — their rejection is itself the
    // assertion that the characters were decoded correctly rather than mangled
    // into something that happens to validate.
    let body = "||first.com^\n# café 例え header\n||café.com^\n||例え.jp^\n||last.com^";
    let bytes: Vec<u8> = body.as_bytes().to_vec();

    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain; charset=utf-8")
        .with_chunked_body(move |w| {
            for byte in &bytes {
                w.write_all(std::slice::from_ref(byte))?;
                // A yield point between bytes keeps hyper from coalescing the
                // whole body back into one chunk.
                std::thread::yield_now();
            }
            Ok(())
        })
        .create_async()
        .await;

    let url = format!("{}/rules", server.url());
    let domains = fetch_rules(&reqwest::Client::new(), &url)
        .await
        .expect("fetch_rules failed");

    assert_eq!(
        domains,
        vec!["first.com".to_string(), "last.com".to_string()],
        "ASCII domains must survive byte-by-byte chunking exactly once, and \
         non-ASCII domains must be rejected (never corrupted into a match)"
    );
}

#[tokio::test]
async fn test_fetch_rules_strips_crlf_and_indented_comments() {
    // A CRLF list (the common Windows case) must not leave a stray \r on the
    // rule, which would make it fail domain validation downstream.
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("||example.com^\r\n   # indented comment\r\n\r\n||test.com^\r\n")
        .create_async()
        .await;

    let url = format!("{}/rules", server.url());
    let rules = fetch_rules(&reqwest::Client::new(), &url)
        .await
        .expect("fetch_rules failed");

    assert_eq!(
        rules,
        vec!["example.com".to_string(), "test.com".to_string()]
    );
    assert!(
        rules.iter().all(|r| !r.contains('\r')),
        "CRLF line endings must not survive into the domain text"
    );
}
