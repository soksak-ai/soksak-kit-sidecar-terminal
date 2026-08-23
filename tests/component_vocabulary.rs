use std::{fs, path::Path};

#[test]
fn source_names_sidecars_directly() {
    let forbidden = [
        ["unit", "_name"].concat(),
        ["unit", "_endpoints"].concat(),
        ["_", "units"].concat(),
        ["installed ", "units"].concat(),
        ["terminal-state ", "units"].concat(),
    ];
    for entry in fs::read_dir("src").expect("read src") {
        let path = entry.expect("source entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read Rust source");
        for term in &forbidden {
            assert!(!source.contains(term), "{} contains {term}", display(&path));
        }
    }
}

#[test]
fn repository_owns_public_metadata() {
    let manifest = fs::read_to_string("Cargo.toml").expect("read Cargo.toml");
    assert!(manifest
        .contains("repository = \"https://github.com/soksak-ai/soksak-kit-sidecar-terminal\""));
    assert!(Path::new("README.md").is_file());
    assert!(Path::new("LICENSE").is_file());
    let kit: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("kit.json").expect("read kit.json"))
            .expect("parse kit.json");
    assert_eq!(kit["id"], "soksak-kit-sidecar-terminal");
    assert_eq!(kit["version"], "0.0.5");
    assert!(manifest.contains(r#"version = "0.0.5""#));
    let release_files = fs::read_to_string("release-files.json").expect("read release files");
    assert!(release_files.contains("\"kit.json\""));
    assert!(release_files.contains("\"README.ko.md\""));
    assert!(release_files.contains("\"src/checkpoint.rs\""));
    assert!(release_files.contains("\"src/transport_name.rs\""));
    assert!(release_files.contains("\"scripts/install_pty_release.py\""));
    assert!(release_files.contains("\"scripts/test_install_pty_release.py\""));
    let workflow =
        fs::read_to_string(".github/workflows/release.yml").expect("read release workflow");
    assert!(workflow.contains("ref: 4adfa80cb0596a9380723fe1cae62b9a14ed6e28"));
    assert!(workflow.contains("owner-enforced immutable releases must be enabled"));
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
