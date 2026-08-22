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
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
