use anyhow::{Context, Result};
use chrono::Utc;
use encoding_rs::UTF_8;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncWriteExt;

/// Prefix used in every output entry. Length is used to extract the domain part.
const ENTRY_PREFIX: &str = "0.0.0.0 ";

/// Mirror of the Python port's `ThreadPoolExecutor(max_workers=min(len(urls), 3))`:
/// cap the number of sources fetched concurrently, no matter how many are
/// configured. Without a cap, a config with a dozen lists opens a dozen
/// simultaneous connections to GitHub, which is both rude and a good way to be
/// rate-limited into a failed run.
const MAX_CONCURRENT_FETCHES: usize = 3;

/// Connect timeout, mirroring the Python port's `timeout=(3, 10)` first element.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Read timeout, mirroring the Python port's `timeout=(3, 10)` second element.
/// Without it a slow or stalled upstream would hang the task forever: `connect_timeout`
/// only covers establishing the connection, not receiving the body.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// When set, progress reporting is suppressed (`--quiet`). Mirrors the Python
/// port's `--quiet`, which lowers the log level to WARNING so only warnings and
/// errors are printed. A plain flag rather than a parameter because the printing
/// happens deep inside `run()`, next to the work it describes.
///
/// Errors and warnings are NOT gated on this: like `logging.WARNING`, quiet only
/// removes informational output, never the reason a run failed.
static QUIET: AtomicBool = AtomicBool::new(false);

/// Enable or disable informational output (see [`QUIET`]).
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Whether informational output is currently suppressed.
fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Print progress information unless `--quiet` was given.
///
/// Errors and warnings must use `eprintln!` directly so they are always shown.
macro_rules! info {
    ($($arg:tt)*) => {
        if !is_quiet() {
            println!($($arg)*);
        }
    };
}

/// Return the file name portion of a source URL (for logs/headers).
///
/// Mirrors the Python port's `_source_name()`. Returns an empty string for a URL
/// ending in `/`, which is what `rpartition("/")` yields there.
pub fn source_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// Resolve the output file path: an explicit `--output` wins, then the
/// `OUTPUT_DIR` environment variable, then the current directory.
///
/// Mirrors the Python port's `--output` taking precedence over `_get_output_file()`.
/// `OUTPUT_DIR` is set to /output in Docker (a dedicated writable volume);
/// when running locally it is unset, so hosts.txt lands in the CWD.
pub fn resolve_output_file(explicit: Option<&Path>) -> PathBuf {
    match explicit {
        Some(path) => path.to_path_buf(),
        None => match std::env::var("OUTPUT_DIR") {
            Ok(dir) => PathBuf::from(dir).join("hosts.txt"),
            Err(_) => PathBuf::from("hosts.txt"),
        },
    }
}

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
    extract_domain(rule).map(|domain| format!("{ENTRY_PREFIX}{domain}"))
}

/// Extract the validated, lowercase domain from one AdBlock rule, or `None` if
/// the line is not a supported rule.
///
/// This is the body of [`convert_rule`] without the `0.0.0.0 ` prefix, so the
/// streaming path can store bare domains instead of decorated entries. The
/// prefix is added at write time instead — mirroring the Python port, which
/// appends `0.0.0.0 {domain}` while writing and keeps only domains in memory.
///
/// # Examples
///
/// ```
/// use adblock2mikrotik_rust::extract_domain;
///
/// assert_eq!(extract_domain("||example.com^"), Some("example.com".to_string()));
/// assert_eq!(extract_domain("||Example.COM^"), Some("example.com".to_string()));
/// assert_eq!(extract_domain("||invalid_domain^"), None);
/// ```
pub fn extract_domain(rule: &str) -> Option<String> {
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
        Some(domain)
    } else {
        None
    }
}

/// Fetch a remote filter list and return its validated domains.
///
/// The body is consumed **as it arrives** (mirroring the Python port's
/// `stream=True` + `iter_lines()`), and each line is converted to its final
/// domain form *during* streaming, exactly as the Python port's
/// `fetch_domains()` does. Raw rule lines are therefore never retained: the only
/// thing kept per line is the finished domain, so a 6 MB list does not cost
/// 6 MB of raw text plus the parsed result.
///
/// Two boundaries have to be handled explicitly, because network chunks and
/// file lines do not line up:
///
/// * **UTF-8 splits.** A multi-byte character can straddle two chunks. A
///   streaming [`encoding_rs::Decoder`] is fed `last = false` and carries the
///   incomplete sequence over to the next chunk, so characters are never
///   corrupted by a chunk boundary.
/// * **Line splits.** A newline can land mid-chunk. Remainder text is carried
///   in `pending` and only complete lines are emitted.
///
/// Retries up to 3 times with exponential backoff (2s → 4s, no wait after the
/// final attempt), mirroring the Python port's `fetch_domains()`.
pub async fn fetch_rules(client: &reqwest::Client, url: &str) -> Result<Vec<String>> {
    // Retry-logic: 3 attempts with exponential backoff: 2s → 4s (no wait after final attempt)
    let max_attempts = 3;
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..max_attempts {
        // One whole attempt (request + streamed body) is a single Result, so a
        // failure part-way through the body is retried just like a failed
        // request. A bare `?` inside the match below would instead return
        // early and skip the remaining attempts.
        let result = fetch_once(client, url).await;

        match result {
            Ok(rules) => return Ok(rules),
            Err(e) => {
                last_error = Some(e);
            }
        }

        if attempt < max_attempts - 1 {
            let wait_secs = 2u64.pow(attempt as u32 + 1); // 2s, then 4s
            // Warnings are never suppressed by --quiet (only info! output is),
            // so a retry in progress stays visible — it is why the run is slow.
            eprintln!(
                "Attempt {} failed for {}. Retrying in {}s...",
                attempt + 1,
                url,
                wait_secs
            );
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
        }
    }

    let error = last_error.unwrap_or_else(|| {
        anyhow::anyhow!("Error fetching {} after {} attempts", url, max_attempts)
    });
    // Match the Python port's fetch_domains(): report the final failure and let
    // run() report the per-source "no rules fetched" line, instead of surfacing
    // a raw error. eprintln! so --quiet never hides why a run failed.
    eprintln!("Error fetching {url} after {max_attempts} attempts: {error}");
    Err(error)
}

/// Convert one streamed line and append its domain to `domains`.
///
/// Blank lines and comments are dropped here, and conversion to the final
/// domain happens here too — so the raw rule text is never stored. This is the
/// Python port's `domain = extract_domain(line); if domain: domains.append(...)`
/// inside its streaming loop.
fn push_domain(domains: &mut Vec<String>, line: &str) {
    // Strip the trailing newline (and a stray \r from CRLF lists) before trimming.
    let line = line.trim_end_matches(['\n', '\r']);
    let trimmed_start = line.trim_start();
    if trimmed_start.is_empty() || trimmed_start.starts_with('#') {
        return;
    }
    if let Some(domain) = extract_domain(line) {
        domains.push(domain);
    }
}

/// One attempt of [`fetch_rules`]: send the request, check the status, then
/// stream the body into a rule list.
///
/// Split out so that every failure mode — a transport error, a non-success
/// status, or a body read that dies part-way — surfaces as a single `Err` and
/// is therefore retried by the caller.
async fn fetch_once(client: &reqwest::Client, url: &str) -> Result<Vec<String>> {
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to send request to {url}"))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Error fetching {}: HTTP {}",
            url,
            response.status()
        ));
    }

    // Read the body chunk by chunk, decoding and emitting complete lines as
    // they arrive, so the whole response is never resident in memory.
    let mut decoder = UTF_8.new_decoder_with_bom_removal();
    let mut domains: Vec<String> = Vec::new();
    // Text after the last newline seen so far: a rule split across two chunks
    // must not be emitted twice or truncated.
    let mut pending = String::new();
    let mut decoded = String::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("Failed to read response body from {url}"))?
    {
        // decode_to_string treats the String's *capacity* as its output limit
        // and never reallocates, so the buffer must be reserved up front for
        // this chunk (worst case: one replacement character per input byte).
        // Reserve once outside the loop and reuse, keeping the decode
        // allocation-free.
        decoded.reserve(chunk.len() + 3);

        // last = false: a multi-byte character may be split across this chunk
        // and the next one, and the decoder carries the incomplete sequence
        // over. Invalid bytes become U+FFFD rather than an error, matching the
        // previous decode_with_bom_removal behaviour.
        let (coder_result, _read, _had_errors) =
            decoder.decode_to_string(&chunk, &mut decoded, false);
        debug_assert_eq!(
            coder_result,
            encoding_rs::CoderResult::InputEmpty,
            "decode_to_string consumed the chunk into the reserved buffer"
        );

        if decoded.is_empty() {
            continue;
        }
        pending.push_str(&decoded);
        decoded.clear();

        // Convert and keep only the complete lines; the remainder stays pending.
        while let Some(nl) = pending.find('\n') {
            let line: String = pending.drain(..=nl).collect();
            push_domain(&mut domains, &line);
        }
    }

    // Flush any truncated multi-byte sequence, then treat trailing text that has
    // no newline as the final line.
    let (flush_result, _, _) = decoder.decode_to_string(&[], &mut decoded, true);
    debug_assert_eq!(
        flush_result,
        encoding_rs::CoderResult::InputEmpty,
        "flushing with last = true finishes the stream"
    );
    if !decoded.is_empty() {
        pending.push_str(&decoded);
    }
    if !pending.is_empty() {
        push_domain(&mut domains, &pending);
    }

    Ok(domains)
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

/// Fetch every source, convert and deduplicate, then write `hosts.txt`.
///
/// Thin wrapper around [`run_with_options`] using the default output path
/// (`$OUTPUT_DIR/hosts.txt`, else `hosts.txt`) and no dry run. Kept as the
/// simple entry point for callers that don't need CLI options.
pub async fn run(urls: Vec<&str>) -> std::io::Result<()> {
    let output_file = resolve_output_file(None);
    run_with_options(&urls, output_file, false).await
}

/// Fetch every source, convert and deduplicate, writing to `output_file`
/// unless `dry_run` is set.
///
/// Mirrors the Python port's `main()` + `write_output()`. `dry_run` performs the
/// entire pipeline (fetch, validate, deduplicate, report counts) but leaves the
/// output file untouched, and reports the same failures with the same exit
/// status as a real run — so a config can be validated before switching to it.
pub async fn run_with_options(
    urls: &[&str],
    output_file: PathBuf,
    dry_run: bool,
) -> std::io::Result<()> {
    // An empty source list is fatal: there is nothing to convert, and exiting
    // 0 would leave a stale hosts.txt in place. Mirrors the Python port's
    // main(), which raises SystemExit(1) before fetching anything.
    if urls.is_empty() {
        eprintln!("Error: no sources configured (empty [sources] urls list).");
        return Err(std::io::Error::other("no sources configured"));
    }

    let start_time = std::time::Instant::now();

    // Create a single Client instance to reuse connections (Keep-Alive).
    // Both timeouts mirror the Python port's `timeout=(3, 10)`: connect_timeout
    // bounds establishing the connection, read_timeout bounds waiting for body
    // data. Without the read timeout a stalled upstream would hang forever.
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .expect("Failed to build reqwest client: builder only fails on invalid TLS or unusable proxy config");

    // HashSet<String> stores domain strings for uniqueness checking
    // Pre-allocate for expected ~300k domains to avoid rehashing
    let mut seen_domains: HashSet<String> = HashSet::with_capacity(300_000);
    let mut source_data: Vec<(String, Vec<String>)> = Vec::new();

    info!("\nFetching {} source(s)...", urls.len());

    // Fetch sources in parallel using tokio::task::JoinSet (no extra crate
    // needed). Results are reported as each source completes, mirroring the
    // Python port's ThreadPoolExecutor + as_completed() progress output.
    //
    // All sources are spawned at once, but a semaphore caps how many actually
    // fetch concurrently at MAX_CONCURRENT_FETCHES — the equivalent of the
    // Python port's `max_workers=min(len(urls), 3)`. Without it, a config with
    // a dozen lists opens a dozen simultaneous connections and is a good way to
    // be rate-limited into a failed run.
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(
        MAX_CONCURRENT_FETCHES.min(urls.len().max(1)),
    ));
    let mut join_set = tokio::task::JoinSet::new();
    for (i, url) in urls.iter().enumerate() {
        let url = url.to_string();
        let client = client.clone();
        let permits = std::sync::Arc::clone(&permits);
        join_set.spawn(async move {
            // Acquire before fetching; released when this task ends. A closed
            // semaphore (run() dropped it) must not cancel the fetch, so an
            // acquire error falls back to proceeding unthrottled rather than
            // silently dropping a configured source.
            let _permit = permits.acquire().await;
            let t = std::time::Instant::now();
            let result = fetch_rules(&client, &url).await;
            let elapsed = t.elapsed();
            (i, url, result, elapsed)
        });
    }

    // A configured source that could not be fetched (error, or an empty
    // response) means the artifact would be narrower than the config promises,
    // so the whole run fails below instead of publishing a partial list.
    // Mirrors the Python port's failed_sources handling.
    let mut failed_sources: Vec<String> = Vec::new();
    let mut indexed_results = Vec::with_capacity(urls.len());

    while let Some(res) = join_set.join_next().await {
        // A panicking task means a bug in fetch_rules, but it must not be
        // swallowed: propagate it as a failed run instead of panicking the
        // whole process, so the exit status stays the documented 1.
        let (i, url, result, fetch_elapsed) = match res {
            Ok(ok) => ok,
            Err(join_err) => {
                eprintln!("Error: a fetch task failed unexpectedly: {join_err}");
                return Err(std::io::Error::other("fetch task failed"));
            }
        };
        // Short filename (last URL path segment) — matches Python's
        // _source_name(), used in progress output so long URLs don't clutter
        // the console.
        let short = source_name(&url).to_string();

        match &result {
            Ok(domains) if !domains.is_empty() => {
                info!(
                    "  - {}: {} domains ({:.2}s)",
                    short,
                    format_with_commas(domains.len()),
                    fetch_elapsed.as_secs_f64()
                );
            }
            _ => {
                // fetch_rules returns no domains only when its retries left it
                // with an empty body, and an upstream list is never
                // legitimately empty.
                eprintln!("  - {short}: ERROR: no rules fetched");
                failed_sources.push(url.clone());
            }
        }

        indexed_results.push((i, url, result));
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

    // Stage 2: deduplicate strictly in configured order, exactly like the
    // Python port's sequential pass. Conversion already happened during the
    // fetch, so this only filters out domains an earlier source already has.
    info!("\nDeduplicating...");

    indexed_results.sort_unstable_by_key(|(i, _, _)| *i);

    for (_, url, result) in indexed_results {
        let short = source_name(&url).to_string();
        // Any Err/empty case was turned into a failed source and returned
        // above, so this is always a successful, non-empty domain list.
        let domains = result.unwrap_or_default();

        let mut unique: Vec<String> = Vec::new();
        for domain in &domains {
            // insert() clones because the domain is also needed in `unique`;
            // the set is the authority on "seen", the Vec keeps config order.
            if seen_domains.insert(domain.clone()) {
                unique.push(domain.clone());
            }
        }
        info!(
            "  - {}: {} unique domains",
            short,
            format_with_commas(unique.len())
        );
        source_data.push((url, unique));
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
        .map(|(url, domains)| {
            let short = source_name(url);
            format!(
                "# - {short} --> {} unique domains\n",
                format_with_commas(domains.len())
            )
        })
        .collect();

    let header = format!(
        "# Title: Unified DNS blocklist optimized for RouterOS\n\
         #\n\
         # URL to add in RouterOS:\n\
         # https://eugenescodes.github.io/adblock2mikrotik_rust/hosts.txt\n\
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

    // Printed before the write, matching the Python port's ordering.
    info!("\nTotal unique domains across all sources: {total_unique_display}");

    if dry_run {
        // Everything above already fetched, validated, deduplicated and counted —
        // the only thing skipped is serializing and writing the file. Reporting
        // this before opening the writer keeps a dry run cheap enough to use as
        // a config check.
        info!("Dry run: {} was not written.", output_file.display());
        info!("Elapsed: {:.2}s\n", start_time.elapsed().as_secs_f64());
        return Ok(());
    }

    // Write atomically: content goes to a hidden temp file in the same directory
    // as output_file, then gets moved into place with tokio::fs::rename() — an
    // atomic rename on POSIX and Windows, the same guarantee as Python's
    // Path.replace() in the sibling project. This ensures readers of output_file
    // (RouterOS polling it over HTTP, or a concurrent process) never observe a
    // partially-written file, even if this process is interrupted mid-write. On
    // failure, the temp file is removed and the error is returned; output_file
    // is left untouched.
    let tmp_file_name = format!(
        ".{}.tmp",
        output_file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("hosts.txt")
    );
    let tmp_file = output_file.with_file_name(tmp_file_name);

    // Streamed through a BufWriter rather than assembled into one big String:
    // the whole hosts file is ~6.6 MB of text that would otherwise sit in RAM
    // alongside the deduplicated domains. Domains are written straight through
    // with the 0.0.0.0 prefix added here — the same approach the Python port
    // takes in write_output().
    let write_result = write_hosts(&tmp_file, &header, &source_data, &total_unique_display).await;

    if let Err(e) = write_result {
        eprintln!("Failed to write file: {e}");
        let _ = tokio::fs::remove_file(&tmp_file).await;
        return Err(e);
    }

    if let Err(e) = tokio::fs::rename(&tmp_file, &output_file).await {
        eprintln!("Failed to move temp file into place: {e}");
        let _ = tokio::fs::remove_file(&tmp_file).await;
        return Err(e);
    }
    info!("Done! Written to: {}", output_file.display());
    info!("Elapsed: {:.2}s\n", start_time.elapsed().as_secs_f64());

    Ok(())
}

/// Stream the hosts file into `path`, one buffered chunk at a time.
///
/// Mirrors the Python port's `write_output()`, which also writes each domain as
/// it loops rather than building the file in memory. The prefix is added here
/// rather than during conversion, so only bare domains are kept in memory.
async fn write_hosts(
    path: &Path,
    header: &str,
    source_data: &[(String, Vec<String>)],
    total_unique_display: &str,
) -> std::io::Result<()> {
    let file = tokio::fs::File::create(path).await?;
    let mut writer = tokio::io::BufWriter::with_capacity(64 * 1024, file);

    writer.write_all(header.as_bytes()).await?;

    for (url, domains) in source_data {
        writer
            .write_all(format!("\n# Source: {url}\n\n").as_bytes())
            .await?;
        for domain in domains {
            writer.write_all(ENTRY_PREFIX.as_bytes()).await?;
            writer.write_all(domain.as_bytes()).await?;
            writer.write_all(b"\n").await?;
        }
        writer
            .write_all(
                format!(
                    "\n# Converted {} rules from this source\n\n",
                    format_with_commas(domains.len())
                )
                .as_bytes(),
            )
            .await?;
    }

    writer
        .write_all(format!("\n# Total unique domains: {total_unique_display}\n").as_bytes())
        .await?;

    // Must flush before the rename, or the final bytes may not reach the file.
    writer.flush().await?;
    writer.shutdown().await?;
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
