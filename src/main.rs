use chrono::Utc;
use regex::Regex;
use reqwest;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, Write};

async fn fetch_rules(url: &str) -> Result<Vec<String>, reqwest::Error> {
    println!("Fetching rules from: {}", url);
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    if response.status().is_success() {
        let text = response.text().await?;
        let rules: Vec<String> = text.lines().map(String::from).collect();
        println!("Successfully fetched {} rules from {}", rules.len(), url);
        Ok(rules)
    } else {
        eprintln!("Error fetching {}: HTTP {}", url, response.status());
        Ok(vec![])
    }
}

fn convert_rule(rule: &str) -> Option<String> {
    // Remove comments and whitespace
    let comment_re = match Regex::new(r"#.*$") {
        Ok(re) => re,
        Err(e) => {
            eprintln!("Failed to create regex: {}", e);
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
        let domain_re = match Regex::new(r"^[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$") {
            Ok(re) => re,
            Err(e) => {
                eprintln!("Failed to create regex: {}", e);
                return None;
            }
        };
        if domain_re.is_match(domain) {
            return Some(format!("0.0.0.0 {}", domain));
        }
    }
    None
}

#[tokio::main]
async fn main() -> io::Result<()> {
    println!("Starting adblock rules conversion...");

    let urls = vec![
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.mini.txt",
        "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.mini.txt",
    ];

    let mut unique_rules = HashSet::new();

    // Write header with timestamp
    let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    println!("Current time: {}", current_time);

    let header = format!(
        r#"# Title: This filter compiled from trusted, verified sources and optimized for compatibility with DNS-level ad blocking by merging and simplifying multiple filters
#
# Homepage: https://github.com/eugenescodes/adblock2mikrotik
# License: https://github.com/eugenescodes/adblock2mikrotik/blob/main/LICENSE
#
# Last modified: {}
#  
# Sources:
# - AdGuard DNS filter
# - Hagezi DNS blocklist for syntax adblock
#
# Format: 0.0.0.0 domain.tld
#
"#,
        current_time
    );

    println!("Creating output file: hosts.txt");
    let file = File::create("hosts.txt").map_err(|e| {
        eprintln!("Failed to create file: {}", e);
        e
    })?;
    let mut writer = BufWriter::new(file);

    writer.write_all(header.as_bytes())?;
    println!("Header written successfully");

    for url in urls {
        writeln!(writer, "\n# Source: {}\n", url)?;
        let rules = fetch_rules(url).await.unwrap_or_else(|_| vec![]);
        let mut converted_count = 0;
        let total_rules = rules.len();

        println!("Processing rules from: {}", url);

        for (index, rule) in rules.iter().enumerate() {
            if index % 1000 == 0 && index > 0 {
                println!("Processed {}/{} rules...", index, total_rules);
            }

            if let Some(converted) = convert_rule(rule) {
                if unique_rules.insert(converted.clone()) {
                    writeln!(writer, "{}", converted)?;
                    converted_count += 1;
                }
            }
        }

        println!("Finished processing source: {}", url);
        println!("Converted {} rules from this source", converted_count);

        writeln!(
            writer,
            "\n# Converted {} rules from this source\n",
            converted_count
        )?;
    }

    // Write total count at the end
    let total_unique = unique_rules.len();
    println!("Total unique domains processed: {}", total_unique);
    writeln!(writer, "\n# Total unique domains: {}\n", total_unique)?;

    // Ensure all buffered data is written to the file
    writer.flush()?;
    println!("All data has been written to hosts.txt");
    println!("Program completed successfully!");

    Ok(())
}
