use adblock2mikrotik_rust::convert_rule;
use chrono::Utc;
use regex::Regex;
use reqwest;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use tokio::task;

async fn fetch_rules(url: &str) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
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

async fn run(urls: Vec<&str>) -> io::Result<()> {
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

#[tokio::main]
async fn main() -> io::Result<()> {
    let urls = vec![
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.mini.txt",
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.mini.txt",
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
