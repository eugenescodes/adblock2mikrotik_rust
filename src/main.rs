use chrono::Utc;
use regex::Regex;
use reqwest;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, Write};

async fn fetch_rules(url: &str) -> Result<Vec<String>, reqwest::Error> {
    println!("Fetching rules from: {url}");
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

async fn run(urls: Vec<&str>) -> io::Result<()> {
    println!("Starting adblock rules conversion...");

    let mut unique_rules = HashSet::new();

    // Write header with timestamp
    let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    println!("Current time: {current_time}");

    let mut source_stats = Vec::new();
    let mut rules_by_source = Vec::new();

    let mut fetch_stats = Vec::new();
    for url in &urls {
        let rules = fetch_rules(url).await.unwrap_or_else(|_| vec![]);
        let mut converted_count = 0;
        let mut converted_rules = Vec::new();
        let total_rules = rules.len();

        println!("Processing rules from: {url}");

        for (index, rule) in rules.iter().enumerate() {
            if index % 1000 == 0 && index > 0 {
                println!("Processed {index}/{total_rules} rules...");
            }
            if let Some(converted) = convert_rule(rule) {
                if unique_rules.insert(converted.clone()) {
                    converted_rules.push(converted);
                    converted_count += 1;
                }
            }
        }

        println!("Finished processing source: {url}");
        println!("Converted {converted_count} rules from this source");
        fetch_stats.push(((*url).to_string(), total_rules));
        source_stats.push(((*url).to_string(), converted_count));
        rules_by_source.push(((*url).to_string(), converted_rules));
    }
    let total_unique = unique_rules.len();

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
    for ((url, fetched_count), (_, converted_count)) in fetch_stats.iter().zip(source_stats.iter())
    {
        header.push_str(&format!("#\n# Source: {url}\n"));
        header.push_str(&format!("# Successfully fetched {fetched_count} domains\n"));
        header.push_str(&format!(
            "# Converted {converted_count} domains from this source\n"
        ));
    }
    header.push_str(&format!("#\n# Total unique: {total_unique} domains\n#\n"));

    let file = File::create("hosts.txt").map_err(|e| {
        eprintln!("Failed to create file: {e}");
        e
    })?;
    let mut writer = BufWriter::new(file);

    writer.write_all(header.as_bytes())?;
    println!("Header written successfully");

    for (_url, rules) in &rules_by_source {
        for rule in rules {
            writeln!(writer, "{rule}")?;
        }
    }

    // Ensure all buffered data is written to the file
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
