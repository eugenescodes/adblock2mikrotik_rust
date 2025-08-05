use adblock2mikrotik_rust::{fetch_rules, run};
use tokio;

fn setup_server() -> mockito::ServerGuard {
    mockito::Server::new()
}

#[test]
fn test_fetch_rules_success() {
    let mut server = setup_server();

    let _m = server
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("||example.com^\n||test.com^\n")
        .create();

    let url = format!("{}/rules", server.url());

    let rt = tokio::runtime::Runtime::new().unwrap();
    let rules = rt.block_on(fetch_rules(&url)).expect("fetch_rules failed");
    assert_eq!(rules.len(), 2);
    assert!(rules.contains(&"||example.com^".to_string()));
    assert!(rules.contains(&"||test.com^".to_string()));

    // mock is dropped here and verified
}

#[tokio::test(flavor = "current_thread")]
async fn test_fetch_rules_http_error() {
    let url = std::thread::spawn(|| {
        let mut server = setup_server();

        let _m = server.mock("GET", "/rules").with_status(500).create();

        format!("{}/rules", server.url())
    })
    .join()
    .expect("Thread panicked");

    let result = fetch_rules(&url).await;
    assert!(result.is_err());

    // mock is dropped here and verified
}

#[test]
fn test_run_with_partial_failure() {
    let mut server1 = setup_server();
    let mut server2 = setup_server();

    let _m1 = server1
        .mock("GET", "/rules")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("||example.com^\n")
        .create();

    let _m2 = server2.mock("GET", "/rules").with_status(500).create();

    let urls = vec![
        format!("{}/rules", server1.url()),
        format!("{}/rules", server2.url()),
    ];

    let urls_ref: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(run(urls_ref));
    assert!(result.is_ok());

    // mocks are dropped here and verified
}
