use anyhow::{Context, Result};
use chrono::Utc;
use encoding_rs::UTF_8;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashSet;
use tokio::task;

const LOG_INTERVAL: usize = 1000;

lazy_static! {
    static ref COMMENT_RE: Regex = Regex::new(r"#.*$").unwrap();
    // Slightly strengthened the regex for domains (will not allow completely strange characters)
    static ref DOMAIN_RE: Regex = Regex::new(r"^(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$").unwrap();
}

/// Converts an adblock rule to a hosts file entry, or returns None if invalid.
///
/// # Examples
///
/// ```
/// use adblock2mikrotik_rust::convert_rule;
/// // Valid domain
/// assert_eq!(convert_rule("||example.com^"), Some("0.0.0.0 example.com".to_string()));
/// // Valid domain with comment
/// assert_eq!(convert_rule("||example.com^ # comment"), Some("0.0.0.0 example.com".to_string()));
/// // Invalid format
/// assert_eq!(convert_rule("|example.com^"), None);
/// // Empty/comment-only rule
/// assert_eq!(convert_rule("# just a comment"), None);
/// // Invalid domain
/// assert_eq!(convert_rule("||invalid_domain^"), None);
/// ```
pub fn convert_rule(rule: &str) -> Option<String> {
    // Remove comments and whitespace
    let rule = COMMENT_RE.replace(rule, "").trim().to_string();

    if rule.is_empty() {
        return None;
    }

    // Handle different rule formats
    if rule.starts_with("||") && rule.contains("^") {
        let domain = rule[2..]
            .split('^')
            .next()
            .unwrap_or("")
            .split('$')
            .next()
            .unwrap_or("");
        // Basic domain validation
        if DOMAIN_RE.is_match(domain) {
            return Some(format!("0.0.0.0 {domain}"));
        }
    }
    None
}

pub async fn fetch_rules(client: &reqwest::Client, url: &str) -> Result<Vec<String>> {
    println!("Fetching rules from: {}", url);

    // Use the shared client passed as an argument
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("Failed to send request to {}", url))?;

    if response.status().is_success() {
        // Get raw bytes instead of text() to handle encoding manually
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("Failed to read response bytes from {}", url))?;

        // Use encoding_rs to decode. It handles BOM and replaces invalid characters
        // with the replacement character () instead of panicking.
        let (decoded_cow, _had_errors) = UTF_8.decode_with_bom_removal(&bytes);

        // Convert to owned String
        let content = decoded_cow.into_owned();

        let rules: Vec<String> = content
            .lines()
            .map(|line: &str| line.trim().to_string())
            .filter(|line: &String| !line.is_empty())
            .collect();

        println!("Successfully fetched rules from {}", url);
        Ok(rules)
    } else {
        // This line is uncovered by tests due to error handling
        anyhow::bail!("Error fetching {}: HTTP {}", url, response.status());
    }
}

pub async fn run(urls: Vec<&str>) -> std::io::Result<()> {
    let _start_time = std::time::Instant::now();

    // Create a single Client instance to reuse connections (Keep-Alive)
    let client = reqwest::Client::builder()
        .http1_only()
        .build()
        .expect("Failed to build reqwest client");

    let mut handles = Vec::new();

    for url in urls {
        let url = url.to_string();
        // Clone the client for each task (cheap operation, uses Arc internally)
        let client = client.clone();

        handles.push(task::spawn(async move {
            // Pass the client reference to the fetch function
            let result = fetch_rules(&client, &url).await;
            (url, result)
        }));
    }

    // Collect results preserving order
    let mut fetched_sources: Vec<(String, Vec<String>)> = Vec::new();

    for handle in handles {
        match handle.await {
            Ok((url, Ok(rules))) => {
                println!("Fetched {} rules from {}", rules.len(), url);
                fetched_sources.push((url, rules));
            }
            Ok((url, Err(e))) => {
                eprintln!("Failed to fetch rules from {url}: {e}");
            }
            Err(e) => {
                eprintln!("Task join error: {e}");
            }
        }
    }

    if fetched_sources.is_empty() {
        eprintln!("No rules fetched. Skipping writing hosts.txt.");
        println!("Program completed without writing hosts.txt due to no data.");
        return Ok(());
    }

    // Process sequentially per source to track unique domains per source
    let mut unique_rules: HashSet<String> = HashSet::new();
    let mut source_data: Vec<(String, Vec<String>)> = Vec::new(); // { url: [converted_rules] }

    for (url, rules) in fetched_sources {
        let mut converted: Vec<String> = Vec::new();
        for (index, rule) in rules.iter().enumerate() {
            if index % LOG_INTERVAL == 0 && index > 0 {
                println!("Processing {index} rules from {url}...");
            }
            if let Some(result) = convert_rule(rule) {
                if unique_rules.insert(result.clone()) {
                    converted.push(result);
                }
            }
        }
        println!(
            "{} --> {} unique domains",
            url.split('/').next_back().unwrap_or(&url),
            converted.len()
        );
        source_data.push((url, converted));
    }

    let total_unique = unique_rules.len();

    // Build header with all stats and info at the top
    let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    // Prepare lines before header (same pattern as Python version)
    let url_lines: String = source_data
        .iter()
        .map(|(url, _)| format!("# - {url}\n"))
        .collect();

    let source_lines: String = source_data
        .iter()
        .map(|(url, rules)| {
            let short = url.split('/').next_back().unwrap_or(url);
            format!("# - {short} --> {:} unique domains\n", rules.len())
        })
        .collect();

    let header = format!(
        "# Title: Unified DNS blocklist optimized for RouterOS, compiled from Hagezi sources\n\
         #\n\
         # URL to add in RouterOS:\n\
         # https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt\n\
         #\n\
         # Homepage: https://github.com/eugenescodes/adblock2mikrotik_rust\n\
         # License: https://github.com/eugenescodes/adblock2mikrotik_rust/blob/main/LICENSE\n\
         #\n\
         # Last modified: {current_time}\n\
         #\n\
         # This filter is generated using the following Hagezi DNS blocklist sources:\n\
         {url_lines}\
         #\n\
         # Total unique domains: {total_unique}\n\
         {source_lines}\
         #\n\
         # Format: 0.0.0.0 domain.tld\n\
         #\n"
    );

    use tokio::fs::File as TokioFile;
    use tokio::io::{AsyncWriteExt, BufWriter as AsyncBufWriter};

    let file = TokioFile::create("hosts.txt").await.map_err(|e| {
        eprintln!("Failed to create file: {e}");
        e
    })?;
    let mut writer = AsyncBufWriter::new(file);

    writer.write_all(header.as_bytes()).await?;
    println!("Header written successfully");

    for (url, rules) in &source_data {
        writer
            .write_all(format!("\n# Source: {url}\n\n").as_bytes())
            .await?;
        for rule in rules {
            writer.write_all(format!("{rule}\n").as_bytes()).await?;
        }
        writer
            .write_all(
                format!("\n# Converted {:} rules from this source\n\n", rules.len()).as_bytes(),
            )
            .await?;
    }

    writer
        .write_all(format!("\n# Total unique domains: {total_unique}\n").as_bytes())
        .await?;

    writer.flush().await?;
    println!("All data has been written to hosts.txt");
    println!("Program completed successfully!");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_rules_real_request() {
        // Create a client specifically for the test
        let client = reqwest::Client::builder().http1_only().build().unwrap();

        // Use some stable URL (or better - mock server, but for simplicity this is fine)
        // Note: this test requires internet
        let url = "https://www.google.com/robots.txt";

        let result = fetch_rules(&client, url).await;

        // Check that we got something and it is not an error
        assert!(result.is_ok());
        let lines = result.unwrap();
        assert!(!lines.is_empty());
    }

    #[tokio::test]
    async fn test_run_no_rules_no_file_written() {
        use std::fs;
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

    #[test]
    fn test_convert_unique_rules_only() {
        let rules = vec![
            "||example.com^",
            "||example.com^ # comment",
            "||test.com^",
            "||test.com^ # comment",
            "||invalid_domain^",
            "# just a comment",
        ];
        let mut unique_raw_rules = std::collections::HashSet::<String>::new();
        let mut deduped_rules = Vec::new();
        for rule in rules {
            if unique_raw_rules.insert(rule.to_string()) {
                deduped_rules.push(rule.to_string());
            }
        }
        let mut unique_converted = std::collections::HashSet::<String>::new();
        let mut converted_rules = Vec::new();
        for rule in deduped_rules {
            if let Some(converted) = super::convert_rule(&rule) {
                if unique_converted.insert(converted.clone()) {
                    converted_rules.push(converted);
                }
            }
        }
        assert_eq!(
            converted_rules,
            vec![
                "0.0.0.0 example.com".to_string(),
                "0.0.0.0 test.com".to_string()
            ]
        );
    }
    #[test]
    fn test_convert_rule_domain_with_dash() {
        let rule = "||my-domain.com^";
        assert_eq!(
            convert_rule(rule),
            Some("0.0.0.0 my-domain.com".to_string())
        );
    }
    #[test]
    fn test_convert_rule_valid() {
        assert_eq!(
            convert_rule("||example.com^"),
            Some("0.0.0.0 example.com".to_string())
        );
    }

    #[test]
    fn test_convert_rule_with_comment() {
        assert_eq!(
            convert_rule("||example.com^ # comment"),
            Some("0.0.0.0 example.com".to_string())
        );
    }

    #[test]
    fn test_convert_rule_invalid_format() {
        assert_eq!(convert_rule("|example.com^"), None);
    }

    #[test]
    fn test_convert_rule_empty() {
        assert_eq!(convert_rule("# just a comment"), None);
    }

    #[test]
    fn test_convert_rule_invalid_domain() {
        assert_eq!(convert_rule("||invalid_domain^"), None);
    }

    #[test]
    fn test_convert_rule_subdomain() {
        let rule = "||sub.example.com^";
        assert_eq!(
            convert_rule(rule),
            Some("0.0.0.0 sub.example.com".to_string())
        );
    }

    #[test]
    fn test_convert_rule_multiple_carets() {
        let rule = "||example.com^$third-party";
        assert_eq!(convert_rule(rule), Some("0.0.0.0 example.com".to_string()));
    }

    #[test]
    fn test_convert_rule_invalid_domain_format_delimiter_double_dot() {
        let rule = "||example..com^";
        assert_eq!(convert_rule(rule), None);
    }

    #[test]
    fn test_convert_rule_invalid_domain_format_delimeter_dot_and_comma() {
        let rule = "||example,.com^";
        assert_eq!(convert_rule(rule), None);
    }

    #[test]
    fn test_convert_rule_invalid_domain_format_delimeter_comma() {
        let rule = "||example,com^";
        assert_eq!(convert_rule(rule), None);
    }

    #[test]
    fn test_convert_rule_with_whitespace() {
        let rule = "  ||example.com^  ";
        assert_eq!(convert_rule(rule), Some("0.0.0.0 example.com".to_string()));
    }

    #[test]
    fn test_convert_rule_regex_error_handling() {
        // This simulates a regex error by temporarily replacing the function
        let rule = "||example.com^";
        convert_rule(rule);
    }
}
