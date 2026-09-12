use anyhow::{Context, Result};
use chrono::Utc;
use encoding_rs::UTF_8;
use std::collections::HashSet;
use std::path::PathBuf;

/// Prefix used in every output entry. Length is used to extract the domain part.
const ENTRY_PREFIX: &str = "0.0.0.0 ";

/// Validates a domain label (single segment between dots).
/// Rules: non-empty, max 63 chars, alphanumeric + hyphens, no leading/trailing hyphen.
fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Validates a domain without regex — replaces DOMAIN_RE from the Python port.
/// Equivalent to: ^(?=.{1,253}$)(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,24}$
fn is_valid_domain(domain: &str) -> bool {
    // Total length 1–253 chars (mirrors the regex's `(?=.{1,253}$)` lookahead).
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }

    // Single pass: validate all labels, track last one for TLD check
    let mut iter = domain.split('.');
    let mut prev: Option<&str> = None;
    let mut label_count = 0;

    for label in iter.by_ref() {
        if let Some(p) = prev {
            // Validate all non-TLD labels as we go
            if !is_valid_label(p) {
                return false;
            }
        }
        prev = Some(label);
        label_count += 1;
    }

    // Need at least label + TLD (e.g. "example.com")
    if label_count < 2 {
        return false;
    }

    // Last label is TLD: only ASCII alpha, 2–24 chars (mirrors `[a-zA-Z]{2,24}`).
    match prev {
        Some(tld) => (2..=24).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_alphabetic()),
        None => false,
    }
}

/// Converts an adblock rule to a hosts file entry, or returns None if invalid.
/// Uses manual string parsing instead of regex for better performance on large inputs.
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
/// // Uppercase domains are normalized to lowercase
/// assert_eq!(convert_rule("||Example.COM^"), Some("0.0.0.0 example.com".to_string()));
/// // The '^' anchor is required
/// assert_eq!(convert_rule("||example.com"), None);
/// ```
pub fn convert_rule(rule: &str) -> Option<String> {
    // Strip inline comment without allocation (replaces COMMENT_RE.replace())
    let rule = match rule.find('#') {
        Some(pos) => rule[..pos].trim(),
        None => rule.trim(),
    };

    if rule.is_empty() {
        return None;
    }

    // Must start with "||" — strip_prefix returns None otherwise
    let rest = rule.strip_prefix("||")?;

    // The Python port requires a '^' anchor ("||example.com" is rejected), so
    // do the same here instead of silently accepting rules without one.
    if !rest.contains('^') {
        return None;
    }

    // Domain is everything up to the first '^'. Normalize to lowercase, as the
    // Python port's extract_domain() returns domain.lower().
    let domain = rest.split('^').next()?.to_lowercase();

    if is_valid_domain(&domain) {
        Some(format!("{ENTRY_PREFIX}{domain}"))
    } else {
        None
    }
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

                // Filter before allocating: skip empty and comment-only lines early
                let rules: Vec<String> = decoded_cow
                    .lines()
                    .filter(|line| {
                        let t = line.trim_start();
                        !t.is_empty() && !t.starts_with('#')
                    })
                    .map(|line| line.trim().to_string())
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
    // An empty source list is fatal: there is nothing to convert, and exiting
    // 0 would leave a stale hosts.txt in place. Mirrors the Python port's
    // main(), which raises SystemExit(1) before fetching anything.
    if urls.is_empty() {
        eprintln!("Error: no sources configured (empty [sources] urls list).");
        return Err(std::io::Error::other("no sources configured"));
    }

    let start_time = std::time::Instant::now();

    // Create a single Client instance to reuse connections (Keep-Alive)
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(3))
        .build()
        .expect("Failed to build reqwest client");

    // HashSet<String> stores domain strings for uniqueness checking
    // Pre-allocate for expected ~300k domains to avoid rehashing
    let mut seen_domains: HashSet<String> = HashSet::with_capacity(300_000);
    let mut source_data: Vec<(String, Vec<String>)> = Vec::new();

    println!("Starting conversion of {} source(s)...\n", urls.len());

    // Fetch all sources in parallel using tokio::task::JoinSet (no extra crate needed)
    // Preserving original URL order via indexed results
    let mut join_set = tokio::task::JoinSet::new();
    for (i, url) in urls.iter().enumerate() {
        let url = url.to_string();
        let client = client.clone();
        join_set.spawn(async move {
            let t = std::time::Instant::now();
            let result = fetch_rules(&client, &url).await;
            let elapsed = t.elapsed();
            (i, url, result, elapsed)
        });
    }

    // Collect results indexed by original position, then sort to restore URL order
    let mut indexed_results = Vec::with_capacity(urls.len());
    while let Some(res) = join_set.join_next().await {
        let task_result = res.expect("task panicked");
        indexed_results.push(task_result);
    }
    indexed_results.sort_unstable_by_key(|(i, _, _, _)| *i);

    // A configured source that could not be fetched (error, or an empty
    // response) means the artifact would be narrower than the config promises,
    // so the whole run fails below instead of publishing a partial list.
    // Mirrors the Python port's failed_sources handling.
    let mut failed_sources: Vec<String> = Vec::new();

    for (_, url, result, fetch_elapsed) in indexed_results {
        // Short filename (last URL path segment) — matches Python's
        // _source_name(), used in progress output so long URLs don't clutter
        // the console.
        let short = url.split('/').next_back().unwrap_or(&url).to_string();

        match result {
            Ok(rules) if !rules.is_empty() => {
                println!(
                    "  - {}: {} lines ({:.2}s)",
                    short,
                    format_with_commas(rules.len()),
                    fetch_elapsed.as_secs_f64()
                );
                let mut converted: Vec<String> = Vec::new();
                for rule in rules.iter() {
                    if let Some(entry) = convert_rule(rule) {
                        // Extract domain part by stripping the fixed prefix
                        let domain = entry[ENTRY_PREFIX.len()..].to_string();
                        if seen_domains.insert(domain) {
                            converted.push(entry);
                        }
                    }
                }
                println!(
                    "  - {}: {} unique domains",
                    short,
                    format_with_commas(converted.len())
                );
                source_data.push((url, converted));
            }
            Ok(_) => {
                // fetch_rules returns no rules only when its retries left it
                // with an empty body, and an upstream list is never
                // legitimately empty.
                eprintln!("  - {short}: ERROR: no rules fetched");
                failed_sources.push(url);
            }
            Err(e) => {
                eprintln!("  - {short}: ERROR: {e}");
                failed_sources.push(url);
            }
        }
    }

    if !failed_sources.is_empty() {
        eprintln!(
            "\nError: {} of {} source(s) failed to fetch ({}) — refusing to publish a partial list.",
            failed_sources.len(),
            urls.len(),
            failed_sources.join(", ")
        );
        return Err(std::io::Error::other("one or more sources failed to fetch"));
    }

    if seen_domains.is_empty() {
        // Every source answered, but none contained a supported ||domain^
        // rule: fail instead of exiting 0 with a stale hosts.txt in place.
        eprintln!(
            "Error: no valid rules were converted from any source (sources empty or in an unsupported format)."
        );
        return Err(std::io::Error::other("no valid rules converted"));
    }

    let total_unique = seen_domains.len();

    // Build header with all stats and info at the top
    let current_time = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    let total_unique_display = format_with_commas(total_unique);

    // List only successfully fetched sources — every source must succeed to
    // reach this point, so this matches the configured order.
    let url_lines: String = source_data
        .iter()
        .map(|(url, _)| format!("# - {url}\n"))
        .collect();

    let source_lines: String = source_data
        .iter()
        .map(|(url, rules)| {
            let short = url.split('/').next_back().unwrap_or(url);
            format!(
                "# - {short} --> {} unique domains\n",
                format_with_commas(rules.len())
            )
        })
        .collect();

    let header = format!(
        "# Title: Unified DNS blocklist optimized for RouterOS\n\
         #\n\
         # URL to add in RouterOS:\n\
         # https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt\n\
         #\n\
         # Homepage: https://github.com/eugenescodes/adblock2mikrotik_rust\n\
         # License: https://github.com/eugenescodes/adblock2mikrotik_rust/blob/main/LICENSE\n\
         #\n\
         # Last modified: {current_time}\n\
         #\n\
         # This filter is generated from the following DNS blocklist sources:\n\
         {url_lines}\
         #\n\
         # Total unique domains: {total_unique_display}\n\
         {source_lines}\
         #\n"
    );

    // OUTPUT_DIR is set in Docker to /output (a dedicated writable volume).
    // When running locally (cargo run), OUTPUT_DIR is not set -> writes to CWD.
    let output_file: PathBuf = match std::env::var("OUTPUT_DIR") {
        Ok(dir) => PathBuf::from(dir).join("hosts.txt"),
        Err(_) => PathBuf::from("hosts.txt"),
    };

    // Pre-allocate content buffer: header + avg 35 bytes per domain entry
    let estimated_capacity = header.len() + total_unique * 35;
    let mut content = String::with_capacity(estimated_capacity);
    content.push_str(&header);

    for (url, rules) in &source_data {
        content.push_str("\n# Source: ");
        content.push_str(url);
        content.push_str("\n\n");
        for rule in rules {
            content.push_str(rule);
            content.push('\n'); // single char push — no format! allocation
        }
        content.push_str("\n# Converted ");
        content.push_str(&format_with_commas(rules.len()));
        content.push_str(" rules from this source\n\n");
    }

    content.push_str("\n# Total unique domains: ");
    content.push_str(&total_unique_display);
    content.push('\n');

    // Write atomically: content is first written to a hidden temp file in the
    // same directory as output_file, then moved into place with
    // tokio::fs::rename() — an atomic rename on POSIX and Windows, same
    // guarantee as Python's Path.replace() in the sibling project. This
    // ensures readers of output_file (RouterOS polling it over HTTP, or a
    // concurrent process) never observe a partially-written file, even if
    // this process is interrupted mid-write. On failure, the temp file is
    // removed and the error is returned; output_file is left untouched.
    let tmp_file_name = format!(
        ".{}.tmp",
        output_file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("hosts.txt")
    );
    let tmp_file = output_file.with_file_name(tmp_file_name);

    if let Err(e) = tokio::fs::write(&tmp_file, &content).await {
        eprintln!("Failed to write file: {e}");
        let _ = tokio::fs::remove_file(&tmp_file).await;
        return Err(e);
    }

    if let Err(e) = tokio::fs::rename(&tmp_file, &output_file).await {
        eprintln!("Failed to move temp file into place: {e}");
        let _ = tokio::fs::remove_file(&tmp_file).await;
        return Err(e);
    }
    println!(
        "Total unique domains across all sources: {}",
        format_with_commas(total_unique)
    );
    println!("Done! Written to: {}", output_file.display());
    println!("Elapsed: {:.2}s", start_time.elapsed().as_secs_f64());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_via_seen_domains() {
        // Mirrors the dedup logic in run(): convert each rule, insert domain into
        // seen_domains — dedup happens after parsing, not on raw rule strings.
        // Key case: "||example.com^" and "||example.com^ # comment" are different
        // raw strings but produce the same domain — only one must appear in output.
        let rules = vec![
            "||example.com^",
            "||example.com^ # comment", // same domain after parsing — must be deduped
            "||test.com^",
            "||test.com^ # comment", // same domain after parsing — must be deduped
            "||invalid_domain^",
            "# just a comment",
        ];
        let mut seen_domains: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut converted: Vec<String> = Vec::new();
        for rule in &rules {
            if let Some(entry) = convert_rule(rule) {
                let domain = entry[ENTRY_PREFIX.len()..].to_string();
                if seen_domains.insert(domain) {
                    converted.push(entry);
                }
            }
        }
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0], "0.0.0.0 example.com");
        assert_eq!(converted[1], "0.0.0.0 test.com");
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
    fn test_convert_rule_unicode_in_domain() {
        // Unicode chars are not ASCII alphanumeric — must be rejected
        assert_eq!(convert_rule("||café.com^"), None);
        assert_eq!(convert_rule("||例え.jp^"), None);
    }

    #[test]
    fn test_convert_rule_leading_trailing_hyphen() {
        assert_eq!(convert_rule("||-example.com^"), None);
        assert_eq!(convert_rule("||example-.com^"), None);
    }

    #[test]
    fn test_convert_rule_lowercases_domain() {
        // extract_domain() in the Python port returns domain.lower().
        assert_eq!(
            convert_rule("||Sub.DomAIN.ExAmPlE.cOm^"),
            Some("0.0.0.0 sub.domain.example.com".to_string())
        );
    }

    #[test]
    fn test_convert_rule_requires_caret() {
        // The Python port only accepts rules containing the '^' anchor.
        assert_eq!(convert_rule("||example.com"), None);
        assert_eq!(convert_rule("||example.com$third-party"), None);
    }

    #[test]
    fn test_convert_rule_rejects_overlong_label() {
        let label = "a".repeat(64);
        let rule = format!("||{label}.com^");
        assert_eq!(convert_rule(&rule), None);
    }

    #[test]
    fn test_convert_rule_rejects_overlong_tld() {
        let tld = "a".repeat(25);
        let rule = format!("||example.{tld}^");
        assert_eq!(convert_rule(&rule), None);
    }

    #[test]
    fn test_convert_rule_rejects_domain_longer_than_253() {
        // Four 63-char labels + a TLD exceed the RFC 1035 253-char limit.
        let label = "a".repeat(63);
        let rule = format!("||{label}.{label}.{label}.{label}.com^");
        assert_eq!(convert_rule(&rule), None);
    }
}
