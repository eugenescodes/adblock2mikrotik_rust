use adblock2mikrotik_rust::run;
use std::io;

#[tokio::main]
async fn main() -> io::Result<()> {
    let urls = vec![
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.mini.txt",
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.mini.txt",
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/gambling.mini.txt",
        // add more URLs as needed
    ];
    run(urls).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_run_no_rules_no_file_written() {
        // Ensure OUTPUT_DIR is not set so the default path (CWD/hosts.txt) is used
        std::env::remove_var("OUTPUT_DIR");
        let _ = fs::remove_file("hosts.txt");

        let result = run(vec![]).await;

        assert!(result.is_ok());

        let file_exists = fs::metadata("hosts.txt").is_ok();
        assert!(
            !file_exists,
            "hosts.txt should not be created when no rules fetched"
        );
    }
}
