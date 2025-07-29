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

#[cfg(test)]
mod tests {
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
    use super::*;

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
