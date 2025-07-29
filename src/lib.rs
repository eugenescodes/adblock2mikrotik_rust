use regex::Regex;

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
        let domain_re = match Regex::new(r"^[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$") {
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
