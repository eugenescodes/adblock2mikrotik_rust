use adblock2mikrotik_rust::run;
use std::io;

#[tokio::main]
async fn main() -> io::Result<()> {
    let urls = vec![
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.mini.txt",
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.mini.txt",
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/gambling.mini.txt",
    ];
    run(urls).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tokio;

    #[tokio::test]
    async fn test_run_no_rules_no_file_written() {
        // Remove hosts.txt if exists
        let _ = fs::remove_file("hosts.txt");

        // Run with empty URLs to simulate no fetching
        let result = run(vec![]).await;

        // Assert run completed successfully
        assert!(result.is_ok());

        // Assert hosts.txt does not exist
        let file_exists = fs::metadata("hosts.txt").is_ok();
        assert!(
            !file_exists,
            "hosts.txt should not be created when no rules fetched"
        );
    }
}
