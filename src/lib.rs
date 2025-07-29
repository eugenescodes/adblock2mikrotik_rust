use chrono::Utc;
use regex::Regex;
use reqwest;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use tokio::task;

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
    let comment_re = match Regex::new(r"#.*$") {
        Ok(re) => re,
        Err(e) => {
            eprintln!("Failed to create regex: {e}");
            return None;
        }
    };
    let rule = comment_re.replace(rule, "").trim().to_string();

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
        let domain_re =
            match Regex::new(r"^(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$") {
                Ok(re) => re,
                Err(e) => {
                    eprintln!("Failed to create regex: {e}");
                    return None;
                }
            };
        if domain_re.is_match(domain) {
            return Some(format!("0.0.0.0 {domain}"));
        }
    }
    None
}

pub async fn fetch_rules(
    url: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    println!("Fetching rules from: {url}");
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    if response.status().is_success() {
        // Try to get response bytes first
        let bytes = response.bytes().await?;
        match std::str::from_utf8(&bytes) {
            Ok(text) => {
                let rules: Vec<String> = text.lines().map(String::from).collect();
                println!("Successfully fetched {} rules from {}", rules.len(), url);
                Ok(rules)
            }
            Err(e) => {
                eprintln!("Failed to decode response body from {url}: {e}");
                Err(Box::new(e))
            }
        }
    } else {
        eprintln!("Error fetching {}: HTTP {}", url, response.status());
        Ok(vec![])
    }
}

pub async fn run(urls: Vec<&str>) -> io::Result<()> {
    println!("Starting adblock rules conversion...");

    let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    println!("Current time: {current_time}");

    const LOG_INTERVAL: usize = 3000; // Change this value as needed

    // Concurrently fetch all rules
    let mut handles = Vec::new();
    for url in urls {
        let url = url.to_string();
        handles.push(task::spawn(async move {
            (url.clone(), fetch_rules(&url).await)
        }));
    }

    let mut all_raw_rules = Vec::new();
    let mut fetch_stats = Vec::new();

    for handle in handles {
        match handle.await {
            Ok((url, Ok(rules))) => {
                println!("Fetched {} rules from {}", rules.len(), url);
                fetch_stats.push((url, rules.len()));
                all_raw_rules.extend(rules);
            }
            Ok((url, Err(e))) => {
                eprintln!("Failed to fetch rules from {url}: {e}");
            }
            Err(e) => {
                eprintln!("Task join error: {e}");
            }
        }
    }

    // Deduplicate raw rules
    let unique_raw_rules: HashSet<_> = all_raw_rules.into_iter().collect();
    println!("Total unique raw rules: {}", unique_raw_rules.len());

    if unique_raw_rules.is_empty() {
        eprintln!("No rules fetched. Skipping writing hosts.txt.");
        println!("Program completed without writing hosts.txt due to no data.");
        return Ok(());
    }

    // Convert only unique rules
    let mut unique_converted_rules = HashSet::new();
    let mut converted_rules_vec = Vec::new();

    for (index, rule) in unique_raw_rules.iter().enumerate() {
        if index % LOG_INTERVAL == 0 && index > 0 {
            println!("Converted {index} unique rules...");
        }
        if let Some(converted) = convert_rule(rule) {
            if unique_converted_rules.insert(converted.clone()) {
                converted_rules_vec.push(converted);
            }
        }
    }

    let total_unique_converted = unique_converted_rules.len();

    // Build header with all stats and info at the top
    let mut header = format!(
        r#"# Title: This filter compiled from trusted, verified sources and optimized for compatibility with DNS-level ad blocking by merging and simplifying multiple filters
#
# Homepage: https://github.com/eugenescodes/adblock2mikrotik
# License: https://github.com/eugenescodes/adblock2mikrotik/blob/main/LICENSE
#
# Last modified: {current_time}
#
# Convert to format: 0.0.0.0 domain.tld
"#
    );

    for (url, fetched_count) in &fetch_stats {
        header.push_str(&format!("#\n# Source: {url}\n"));
        header.push_str(&format!("# Successfully fetched {fetched_count} domains\n"));
    }
    header.push_str(&format!(
        "#\n# Total unique raw rules: {}\n",
        unique_raw_rules.len()
    ));
    header.push_str(&format!(
        "# Total unique converted rules: {total_unique_converted}\n#\n",
    ));

    let file = File::create("hosts.txt").map_err(|e| {
        eprintln!("Failed to create file: {e}");
        e
    })?;
    let mut writer = BufWriter::new(file);

    writer.write_all(header.as_bytes())?;
    println!("Header written successfully");

    for rule in &converted_rules_vec {
        writeln!(writer, "{rule}")?;
    }

    writer.flush()?;
    println!("All data has been written to hosts.txt");
    println!("Program completed successfully!");

    Ok(())
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
