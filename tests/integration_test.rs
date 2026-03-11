use adblock2mikrotik_rust::{fetch_rules, run};

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

#[tokio::test]
async fn test_fetch_rules_http_error() {
    let mut server = mockito::Server::new_async().await;

    // Retry logic makes 3 attempts (exponential backoff: 2s + 4s ≈ 6s total wait)
    let _m = server
        .mock("GET", "/rules")
        .with_status(500)
        .expect(3)
        .create_async()
        .await;

    let url = format!("{}/rules", server.url());
    let client = reqwest::Client::new();

    let result = fetch_rules(&client, &url).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_run_with_partial_failure() {
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
