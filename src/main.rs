use adblock2mikrotik_rust::run;
use serde::Deserialize;
use std::env;
use std::io;
use std::path::Path;

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

/// Default sources used as fallback if config.toml is not found, invalid,
/// or has no [sources] table. Parsed from the bundled config.toml.example
/// (see DEFAULT_CONFIG_TOML) rather than duplicated as a separate literal,
/// so the two never drift apart.
fn default_sources() -> Vec<String> {
    toml::from_str::<Config>(DEFAULT_CONFIG_TOML)
        .ok()
        .and_then(|config| config.sources)
        .and_then(|sources| sources.urls)
        .unwrap_or_default()
}

/// Load sources from a TOML config file at the given path.
///
/// Takes the path explicitly (rather than reading a hardcoded constant
/// internally) so tests can point it at an isolated temp file instead of
/// mutating a real config.toml in the project's working directory.
///
/// Console output and fallback structure:
/// config.toml missing, unreadable, invalid TOML, or with no [sources]
/// urls key all fall back to the embedded config.toml.example defaults,
/// with the same "Note: ... using default sources" / "Loaded N default
/// sources" messaging. An explicit `urls = []` is treated as an intentional
/// override (convert nothing), not a missing value, and is returned as-is.
fn load_config(config_path: &Path) -> Vec<String> {
    let urls: Option<Vec<String>> = std::fs::read_to_string(config_path)
        .ok()
        .and_then(|content| toml::from_str::<Config>(&content).ok())
        .and_then(|config| config.sources)
        .and_then(|sources| sources.urls);

    if let Some(urls) = urls {
        println!(
            "Loaded {} sources from {}",
            urls.len(),
            config_path.display()
        );
        return urls;
    }

    if config_path.exists() {
        println!(
            "Note: {} has no usable [sources] urls, using default sources from config.toml.example",
            config_path.display()
        );
    } else {
        println!(
            "Note: {} not found, using default sources from config.toml.example",
            config_path.display()
        );
    }

    let default_urls = default_sources();
    if default_urls.is_empty() {
        eprintln!("Error: config.toml.example is missing or has no [sources] urls.");
    } else {
        println!(
            "Loaded {} default sources from config.toml.example",
            default_urls.len()
        );
    }
    default_urls
}

#[tokio::main]
async fn main() -> io::Result<()> {
    // Check for version flag before loading config to avoid unnecessary file I/O
    let args: Vec<String> = env::args().collect();
    if args
        .iter()
        .any(|arg| arg == "--version" || arg == "-v" || arg == "-V")
    {
        println!("adblock2mikrotik_rust v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let urls = load_config(Path::new(CONFIG_PATH));
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
    run(url_refs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::OnceLock;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    // test_run_no_rules_no_file_written is the only test in this module that
    // mutates process-wide environment variables (OUTPUT_DIR is process
    // global, unlike everything else here). This lock guards that mutation
    // in case a future test in this module does the same concurrently. The
    // load_config tests below no longer need any locking or cleanup: each
    // uses its own isolated tempdir and passes the path directly to
    // load_config(), so there's no shared file for parallel test threads to
    // race on.
    //
    // tokio::sync::Mutex (not std::sync::Mutex) is used deliberately: its
    // guard is safe to hold across an .await point. A std Mutex guard held
    // across run().await would trip clippy::await_holding_lock — holding an
    // OS-level lock across an await can block the async executor's thread
    // while other tasks wait on it, which is exactly what that lint flags.
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn get_env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[tokio::test]
    async fn test_run_no_rules_no_file_written() {
        let _guard = get_env_lock().lock().await;
        let temp_dir = tempdir().unwrap();

        // SAFETY: The mutex guard ensures no other test thread in this module
        // mutates process environment variables concurrently.
        unsafe { std::env::set_var("OUTPUT_DIR", temp_dir.path()) };

        let result = run(vec![]).await;

        // SAFETY: same guard as above.
        unsafe { std::env::remove_var("OUTPUT_DIR") };

        assert!(result.is_ok());
        assert!(
            fs::metadata(temp_dir.path().join("hosts.txt")).is_err(),
            "hosts.txt should not be created when no rules fetched"
        );
    }

    #[test]
    fn test_load_config_fallback_when_no_config() {
        let dir = tempdir().unwrap();
        let urls = load_config(&dir.path().join("nonexistent_config.toml"));
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
        let urls = load_config(&config_path);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com/list1.txt");
        assert_eq!(urls[1], "https://example.com/list2.txt");
    }

    #[test]
    fn test_load_config_fallback_on_invalid_toml() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "this is not valid toml [[[").unwrap();
        let urls = load_config(&config_path);
        assert_eq!(urls, default_sources());
    }

    #[test]
    fn test_load_config_empty_array() {
        // Explicit `urls = []` is an intentional override — convert nothing —
        // and must NOT fall back to defaults. Contrast with the next test.
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let toml_content = r#"
[sources]
urls = []
"#;
        fs::write(&config_path, toml_content).unwrap();
        let urls = load_config(&config_path);
        assert_eq!(urls.len(), 0);
    }

    #[test]
    fn test_load_config_sources_present_without_urls_key_falls_back() {
        // [sources] present but no `urls` key at all is a missing value, not
        // an explicit override — must fall back to defaults, same as if
        // config.toml didn't exist. Previously this silently returned an
        // empty Vec instead of falling back, unlike the Python port.
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "[sources]\n# no urls key here\n").unwrap();
        let urls = load_config(&config_path);
        assert_eq!(urls, default_sources());
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
        let urls = load_config(&config_path);
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
        let urls = load_config(&config_path);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com/list1.txt");
    }

    #[test]
    fn test_load_config_duplicate_urls() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let toml_content = r#"
[sources]
urls = [
    "https://example.com/list.txt",
    "https://example.com/list.txt",
    "https://example.com/other.txt",
]
"#;
        fs::write(&config_path, toml_content).unwrap();
        let urls = load_config(&config_path);
        assert_eq!(urls.len(), 3, "Should preserve duplicate URLs from config");
    }
}
