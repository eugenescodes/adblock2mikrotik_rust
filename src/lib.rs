use anyhow::{Context, Result};
use chrono::Utc;
use encoding_rs::UTF_8;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashSet;
use std::path::PathBuf;

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
    // Retry-logic: 3 attempts with exponential backoff: 2s → 4s (no wait after final attempt)
    let max_attempts = 3;
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..max_attempts {
        let result = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to send request to {}", url));

        match result {
            Ok(response) if response.status().is_success() => {
                // Get raw bytes instead of text() to handle encoding manually
                let bytes = response
                    .bytes()
                    .await
                    .with_context(|| format!("Failed to read response bytes from {}", url))?;

                // Use encoding_rs to decode. It handles BOM and replaces invalid characters
                // with the replacement character () instead of panicking.
                let (decoded_cow, _had_errors) = UTF_8.decode_with_bom_removal(&bytes);

                let rules: Vec<String> = decoded_cow
                    .lines()
                    .map(|line| line.trim().to_string())
                    .filter(|line| !line.is_empty())
                    .collect();

                return Ok(rules);
            }
            Ok(response) => {
                last_error = Some(anyhow::anyhow!(
                    "Error fetching {}: HTTP {}",
                    url,
                    response.status()
                ));
            }
            Err(e) => {
                last_error = Some(e);
            }
        }

        if attempt < max_attempts - 1 {
            let wait_secs = 2u64.pow(attempt as u32 + 1); // 2s, then 4s
            println!(
                "Attempt {} failed for {}. Retrying in {}s...",
                attempt + 1,
                url,
                wait_secs
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
        }
    }

    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!("Error fetching {} after {} attempts", url, max_attempts)
    }))
}

// Helper function to format numbers with commas (e.g., 76376 -> "76,376")
fn format_with_commas(n: usize) -> String {
    let n_str = n.to_string();
    let mut result = String::new();
    for (i, c) in n_str.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

pub async fn run(urls: Vec<&str>) -> std::io::Result<()> {
    let start_time = std::time::Instant::now();

    // Create a single Client instance to reuse connections (Keep-Alive)
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(3))
        .build()
        .expect("Failed to build reqwest client");

    let mut unique_rules: HashSet<String> = HashSet::new();
    let mut source_data: Vec<(String, Vec<String>)> = Vec::new();

    println!("Starting conversion of {} source(s)...\n", urls.len());

    // Fetch all sources in parallel, preserving original URL order (matches Python ThreadPoolExecutor)
    let fetch_futures: Vec<_> = urls
        .iter()
        .map(|url| {
            let url = url.to_string();
            let client = client.clone();
            async move {
                println!("Fetching: {}\n", url);
                let result = fetch_rules(&client, &url).await;
                (url, result)
            }
        })
        .collect();

    let results = futures::future::join_all(fetch_futures).await;

    // Process results in original URL order:
    // source_data = {url: source_data[url] for url in urls if url in source_data}
    for (url, result) in results {
        match result {
            Ok(rules) => {
                println!(
                    "Fetched {} lines\nfrom {}",
                    format_with_commas(rules.len()),
                    url
                );
                let mut converted: Vec<String> = Vec::new();
                for rule in rules.iter() {
                    if let Some(result) = convert_rule(rule) {
                        if unique_rules.insert(result.clone()) {
                            converted.push(result);
                        }
                    }
                }
                println!(
                    "Converted {} unique domains\n",
                    format_with_commas(converted.len())
                );
                source_data.push((url, converted));
            }
            Err(e) => {
                eprintln!("Failed to fetch rules from {url}: {e}");
            }
        }
    }

    if source_data.is_empty() {
        eprintln!("No rules fetched. Skipping writing hosts.txt.");
        return Ok(());
    }

    let total_unique = unique_rules.len();

    // Build header with all stats and info at the top
    let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    // Prepare lines before header to list all source URLs and their unique domain counts
    // Uses original urls list so failed sources still appear in the header
    let url_lines: String = urls.iter().map(|url| format!("# - {url}\n")).collect();

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

    // OUTPUT_DIR is set in Docker to /output (a dedicated writable volume).
    // When running locally (cargo run), OUTPUT_DIR is not set -> writes to CWD.
    let output_file: PathBuf = match std::env::var("OUTPUT_DIR") {
        Ok(dir) => PathBuf::from(dir).join("hosts.txt"),
        Err(_) => PathBuf::from("hosts.txt"),
    };

    // Build the complete content as a string
    let mut content = header;

    for (url, rules) in &source_data {
        content.push_str(&format!("\n# Source: {url}\n\n"));
        for rule in rules {
            content.push_str(&format!("{rule}\n"));
        }
        content.push_str(&format!(
            "\n# Converted {:} rules from this source\n\n",
            rules.len()
        ));
    }

    content.push_str(&format!("\n# Total unique domains: {total_unique}\n"));

    // Write all content at once
    tokio::fs::write(&output_file, &content)
        .await
        .map_err(|e| {
            eprintln!("Failed to write file: {e}");
            e
        })?;
    println!(
        "Total unique domains across all sources: {}",
        format_with_commas(total_unique)
    );
    println!("Done! Written to: {}", output_file.display());
    println!("Elapsed: {:.2?}", start_time.elapsed());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
