use adblock2mikrotik_rust::{resolve_output_file, run_with_options, set_quiet};
use clap::Parser;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Config {
    sources: Option<Sources>,
}

#[derive(Deserialize)]
struct Sources {
    urls: Option<Vec<String>>,
}

const CONFIG_PATH: &str = "config.toml";

/// config.toml.example embedded at compile time — the single source of
/// truth for default sources. No runtime file dependency: unlike a
/// filesystem fallback, this can't go missing at deploy time, doesn't care
/// where the binary is executed from, and needs nothing extra in the Docker
/// image.
const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml.example");

/// Collapse duplicate URLs while preserving first-seen order.
///
/// Mirrors the Python port's `list(dict.fromkeys(urls))`: run() keys fetched
/// results by URL, so a repeated URL would otherwise be fetched twice and
/// counted twice.
fn dedup_preserving_order(urls: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    urls.into_iter()
        .filter(|url| seen.insert(url.clone()))
        .collect()
}

/// Default sources used as fallback when config.toml is not found. Parsed
/// from the bundled config.toml.example (see DEFAULT_CONFIG_TOML) rather than
/// duplicated as a separate literal, so the two never drift apart.
fn default_sources() -> Vec<String> {
    toml::from_str::<Config>(DEFAULT_CONFIG_TOML)
        .ok()
        .and_then(|config| config.sources)
        .and_then(|sources| sources.urls)
        .map(dedup_preserving_order)
        .unwrap_or_default()
}

/// Load sources from a TOML config file at the given path.
///
/// Takes the path explicitly (rather than reading a hardcoded constant
/// internally) so tests can point it at an isolated temp file instead of
/// mutating a real config.toml in the project's working directory.
///
/// Semantics mirror the Python port's `load_config()`:
/// - A *missing* config file falls back to the embedded config.toml.example
///   defaults, logging a "not found" note.
/// - A config file that *exists* but is unusable (malformed TOML, no
///   `[sources] urls` list, wrong value types, or an empty list) is a
///   configuration error: it logs an error and returns an empty list, so a
///   typo in the user's own config is never silently replaced by the
///   defaults.
/// - If the bundled fallback is itself unusable, that is logged too and an
///   empty list is returned.
///
/// An empty result is treated by main() as fatal (non-zero exit).
///
/// `required` mirrors the Python port's `load_config(..., required=True)`:
/// when the caller named the file explicitly (`--config`), a missing file is an
/// error rather than a reason to fall back to the defaults — otherwise the run
/// would silently publish sources the user did not ask for.
fn load_config(config_path: &Path, required: bool) -> Vec<String> {
    if !config_path.exists() && required {
        eprintln!("Error: config file {} not found.", config_path.display());
        return Vec::new();
    }

    if config_path.exists() {
        let urls: Option<Vec<String>> = std::fs::read_to_string(config_path)
            .ok()
            .and_then(|content| toml::from_str::<Config>(&content).ok())
            .and_then(|config| config.sources)
            .and_then(|sources| sources.urls)
            .map(dedup_preserving_order);

        if let Some(urls) = urls
            && !urls.is_empty()
        {
            println!(
                "Loaded {} sources from {}",
                urls.len(),
                config_path.display()
            );
            return urls;
        }

        eprintln!(
            "Error: {} has no usable [sources] urls — refusing to fall back to defaults.",
            config_path.display()
        );
        return Vec::new();
    }

    println!(
        "\nNote: {} not found, using default sources from config.toml.example",
        config_path.display()
    );

    let default_urls = default_sources();
    if default_urls.is_empty() {
        eprintln!("Error: default source file config.toml.example is missing or invalid.");
    } else {
        println!(
            "Loaded {} default sources from config.toml.example",
            default_urls.len()
        );
    }
    default_urls
}

/// Command-line interface, mirroring the Python port's `argparse` parser.
#[derive(Parser, Debug)]
#[command(
    name = "adblock2mikrotik_rust",
    version,
    about = "Convert AdBlock-style filter lists (||domain^) into a hosts file \
             for the MikroTik RouterOS DNS adlist.",
    after_help = "Without --config, sources are read from ./config.toml if it exists, \
                  otherwise the bundled default sources are used. Without --output, \
                  hosts.txt is written to $OUTPUT_DIR if set, otherwise to the \
                  current directory."
)]
struct Cli {
    /// TOML file with the [sources] urls list (must exist)
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Where to write the hosts file
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Fetch, validate and deduplicate as usual, but don't write the hosts file
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Only print warnings and errors
    #[arg(short, long)]
    quiet: bool,
}

/// Run the CLI and return the process exit code.
///
/// Returns the code instead of calling `exit()` directly, so the binary's
/// `main()` stays a one-liner and the exit-code contract is explicit.
///
/// Exit codes mirror the Python port: `0` on success, `1` for an unusable
/// configuration or a source that could not be fetched (nothing is written in
/// either case), and clap's own `2` for invalid command-line arguments.
async fn run_cli() -> i32 {
    let cli = Cli::parse();
    set_quiet(cli.quiet);

    // A named --config must exist; the implicit ./config.toml may be absent,
    // which is what selects the bundled defaults.
    let (config_path, required) = match &cli.config {
        Some(path) => (path.as_path(), true),
        None => (Path::new(CONFIG_PATH), false),
    };

    let urls = load_config(config_path, required);
    if urls.is_empty() {
        // load_config has already reported why the source list is unusable;
        // fail loudly instead of leaving a stale hosts.txt in place.
        return 1;
    }

    let output_file = resolve_output_file(cli.output.as_deref());
    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();

    // run_with_options has already printed the reason and written nothing.
    match run_with_options(&url_refs, output_file, cli.dry_run).await {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(run_cli().await as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adblock2mikrotik_rust::run;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_run_no_rules_no_file_written() {
        let temp_dir = tempdir().unwrap();

        // SAFETY: this is the only test in this binary that mutates OUTPUT_DIR.
        unsafe { std::env::set_var("OUTPUT_DIR", temp_dir.path()) };

        let result = run(vec![]).await;

        // SAFETY: no other test in this binary reads or writes OUTPUT_DIR.
        unsafe { std::env::remove_var("OUTPUT_DIR") };

        // An empty source list is fatal (non-zero exit), mirroring the Python
        // port — but no partial or empty hosts.txt must be left behind.
        assert!(result.is_err());
        assert!(
            fs::metadata(temp_dir.path().join("hosts.txt")).is_err(),
            "hosts.txt should not be created when no rules fetched"
        );
    }

    #[test]
    fn test_load_config_fallback_when_no_config() {
        let dir = tempdir().unwrap();
        let urls = load_config(&dir.path().join("nonexistent_config.toml"), false);
        assert_eq!(urls, default_sources());
    }

    #[test]
    fn test_load_config_sources_only() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let toml_content = r#"
[sources]
urls = [
    "https://example.com/list1.txt",
    "https://example.com/list2.txt",
]
"#;
        fs::write(&config_path, toml_content).unwrap();
        let urls = load_config(&config_path, false);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com/list1.txt");
        assert_eq!(urls[1], "https://example.com/list2.txt");
    }

    #[test]
    fn test_load_config_fallback_on_invalid_toml() {
        // A config.toml that *exists* but is malformed is a configuration
        // error, not a reason to silently use the defaults.
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "this is not valid toml [[[").unwrap();
        let urls = load_config(&config_path, false);
        assert!(urls.is_empty());
        assert_ne!(urls, default_sources());
    }

    #[test]
    fn test_load_config_empty_array_is_an_error() {
        // Explicit `urls = []` leaves nothing to convert; it is unusable, not
        // an intentional override that should be accepted silently.
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let toml_content = r#"
[sources]
urls = []
"#;
        fs::write(&config_path, toml_content).unwrap();
        let urls = load_config(&config_path, false);
        assert!(urls.is_empty());
        assert_ne!(urls, default_sources());
    }

    #[test]
    fn test_load_config_sources_present_without_urls_key_is_an_error() {
        // [sources] present but no `urls` key at all is unusable, not a
        // missing-file fallback.
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "[sources]\n# no urls key here\n").unwrap();
        let urls = load_config(&config_path, false);
        assert!(urls.is_empty());
        assert_ne!(urls, default_sources());
    }

    #[test]
    fn test_load_config_rejects_non_string_urls() {
        // A structurally wrong list must be rejected instead of taken at face
        // value (and then iterated character by character).
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "[sources]\nurls = [1, 2]\n").unwrap();
        let urls = load_config(&config_path, false);
        assert!(urls.is_empty());
        assert_ne!(urls, default_sources());
    }

    #[test]
    fn test_load_config_overwrites_defaults() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let toml_content = r#"
[sources]
urls = ["https://custom.com/blocklist.txt"]
"#;
        fs::write(&config_path, toml_content).unwrap();
        let urls = load_config(&config_path, false);
        assert_ne!(
            urls,
            default_sources(),
            "Config should override default sources"
        );
        assert_eq!(urls[0], "https://custom.com/blocklist.txt");
    }

    #[test]
    fn test_default_sources_embedded_and_non_empty() {
        // Sanity check for the include_str! embedding: config.toml.example
        // must parse into at least one URL, or every fallback path in
        // load_config silently degrades to an empty source list.
        let urls = default_sources();
        assert!(
            !urls.is_empty(),
            "config.toml.example must define at least one [sources] url"
        );
        assert!(urls.iter().all(|u| u.starts_with("https://")));
    }

    #[test]
    fn test_load_config_with_comments() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let toml_content = r#"
# This is a comment
[sources]
urls = [
    "https://example.com/list1.txt", # First list
    "https://example.com/list2.txt", # Second list
]
"#;
        fs::write(&config_path, toml_content).unwrap();
        let urls = load_config(&config_path, false);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com/list1.txt");
    }

    #[test]
    fn test_load_config_deduplicates_urls_preserving_order() {
        // Duplicate source URLs collapse to a single entry, keeping the order
        // of first appearance — mirrors the Python port and avoids fetching
        // and counting the same list twice.
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let toml_content = r#"
[sources]
urls = [
    "https://example.com/list.txt",
    "https://example.com/other.txt",
    "https://example.com/list.txt",
]
"#;
        fs::write(&config_path, toml_content).unwrap();
        let urls = load_config(&config_path, false);
        assert_eq!(
            urls,
            vec![
                "https://example.com/list.txt".to_string(),
                "https://example.com/other.txt".to_string(),
            ]
        );
    }
}
