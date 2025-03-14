use mockito::Server;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_simple_request() {
    // mock server
    let mut server = Server::new();

    // temporary directory
    let temp_dir = tempdir().unwrap();
    let test_file_path = temp_dir.path().join("test.txt");

    // mock endpoint
    let mock = server
        .mock("GET", "/api/data")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("test data")
        .create();

    // mock server URL
    let url = format!("{}/api/data", server.url());

    // request using blocking client
    let response = reqwest::blocking::get(&url).unwrap();
    assert_eq!(response.status(), 200);

    let response_text = response.text().unwrap();

    // response to temp file
    fs::write(&test_file_path, &response_text).unwrap();

    // verify file contents
    let content = fs::read_to_string(&test_file_path).unwrap();
    assert_eq!(content, "test data");

    // verify that the mock was called
    mock.assert();
}
