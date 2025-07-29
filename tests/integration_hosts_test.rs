use std::fs;
use tempfile::tempdir;
// Import the function to convert adblock rules to hosts file entries
use adblock2mikrotik_rust::convert_rule;

#[test]
fn test_hosts_file_generation() {
    // Prepare test rules
    let rules = vec![
        "||example.com^",
        "||test.com^",
        "||invalid_domain^",
        "# just a comment",
        "||example.com^ # comment",
    ];

    // Simulate conversion
    let mut unique_rules = std::collections::HashSet::new();
    let mut converted = Vec::new();
    for rule in &rules {
        if let Some(c) = crate::convert_rule(rule) {
            if unique_rules.insert(c.clone()) {
                converted.push(c);
            }
        }
    }

    // Write to temp file
    let temp_dir = tempdir().unwrap();
    let file_path = temp_dir.path().join("hosts.txt");
    fs::write(&file_path, converted.join("\n")).unwrap();

    // Read and check output
    let content = fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("0.0.0.0 example.com"));
    assert!(content.contains("0.0.0.0 test.com"));
    assert!(!content.contains("invalid_domain"));
    assert!(!content.contains("# just a comment"));
}
