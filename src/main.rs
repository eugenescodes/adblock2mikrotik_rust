use adblock2mikrotik_rust::run;
use serde::Deserialize;
use std::env;
use std::io;

#[derive(Deserialize)]
struct Config {
    sources: Option<Sources>,
}

#[derive(Deserialize)]
struct Sources {
    urls: Option<Vec<String>>,
}

const CONFIG_PATH: &str = "config.toml";

/// Default sources used as fallback if config.toml is not found.
const SOURCES: &[&str] = &[
    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.mini.txt",
    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.mini.txt",
    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/gambling.mini.txt",
    // add more URLs as needed
];

/// Load sources and optional output path from config.toml if it exists.
fn load_config() -> Vec<String> {
    match std::fs::read_to_string(CONFIG_PATH) {
        Ok(content) => match toml::from_str::<Config>(&content) {
            Ok(config) => {
                println!("Loaded sources from {CONFIG_PATH}");
                // If sources section exists, use its URLs (even if empty)
                // If sources section doesn't exist, use defaults
                let urls = match config.sources {
                    Some(sources) => sources.urls.unwrap_or_default(),
                    None => SOURCES.iter().map(|s| s.to_string()).collect(),
                };
                urls
            }
            Err(e) => {
                eprintln!("Warning: failed to parse {CONFIG_PATH}: {e}. Using default sources.");
                SOURCES.iter().map(|s| s.to_string()).collect()
            }
        },
        Err(_) => {
            // File not found — silent fallback, no warning needed
            SOURCES.iter().map(|s| s.to_string()).collect()
        }
    }
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

    let urls = load_config();
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
    run(url_refs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn get_lock() -> &'static Mutex<()> {
        CONFIG_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn cleanup_config() {
        let _ = fs::remove_file(CONFIG_PATH);
        let _ = fs::remove_file("hosts.txt");
    }

    #[tokio::test]
    async fn test_run_no_rules_no_file_written() {
        {
            let _guard = get_lock().lock().unwrap();
            cleanup_config();
            std::env::remove_var("OUTPUT_DIR");
        }
        let result = run(vec![]).await;

        assert!(result.is_ok());
        assert!(
            fs::metadata("hosts.txt").is_err(),
            "hosts.txt should not be created when no rules fetched"
        );

        cleanup_config();
    }

    #[test]
    fn test_load_config_fallback_when_no_config() {
        let _guard = get_lock().lock().unwrap();
        cleanup_config();
        let urls = load_config();
        assert_eq!(urls, SOURCES);
        cleanup_config();
    }

    #[test]
    fn test_load_config_sources_only() {
        let _guard = get_lock().lock().unwrap();
        cleanup_config();
        let toml_content = r#"
[sources]
urls = [
    "https://example.com/list1.txt",
    "https://example.com/list2.txt",
]
"#;
        fs::write(CONFIG_PATH, toml_content).unwrap();
        let urls = load_config();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com/list1.txt");
        assert_eq!(urls[1], "https://example.com/list2.txt");
        cleanup_config();
    }

    #[test]
    fn test_load_config_fallback_on_invalid_toml() {
        let _guard = get_lock().lock().unwrap();
        cleanup_config();
        fs::write(CONFIG_PATH, "this is not valid toml [[[").unwrap();
        let urls = load_config();
        assert_eq!(urls, SOURCES);
        cleanup_config();
    }

    #[test]
    fn test_load_config_empty_array() {
        let _guard = get_lock().lock().unwrap();
        cleanup_config();
        let toml_content = r#"
[sources]
urls = []
"#;
        fs::write(CONFIG_PATH, toml_content).unwrap();
        let urls = load_config();
        assert_eq!(urls.len(), 0);
        cleanup_config();
    }

    #[test]
    fn test_load_config_overwrites_defaults() {
        let _guard = get_lock().lock().unwrap();
        cleanup_config();
        let toml_content = r#"
[sources]
urls = ["https://custom.com/blocklist.txt"]
"#;
        fs::write(CONFIG_PATH, toml_content).unwrap();
        let urls = load_config();
        assert_ne!(urls, SOURCES, "Config should override default sources");
        assert_eq!(urls[0], "https://custom.com/blocklist.txt");
        cleanup_config();
    }

    #[test]
    fn test_load_config_with_comments() {
        let _guard = get_lock().lock().unwrap();
        cleanup_config();
        let toml_content = r#"
# This is a comment
[sources]
urls = [
    "https://example.com/list1.txt", # First list
    "https://example.com/list2.txt", # Second list
]
"#;
        fs::write(CONFIG_PATH, toml_content).unwrap();
        let urls = load_config();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com/list1.txt");
        cleanup_config();
    }

    #[test]
    fn test_load_config_duplicate_urls() {
        let _guard = get_lock().lock().unwrap();
        cleanup_config();
        let toml_content = r#"
[sources]
urls = [
    "https://example.com/list.txt",
    "https://example.com/list.txt",
    "https://example.com/other.txt",
]
"#;
        fs::write(CONFIG_PATH, toml_content).unwrap();
        let urls = load_config();
        assert_eq!(urls.len(), 3, "Should preserve duplicate URLs from config");
        cleanup_config();
    }
}
