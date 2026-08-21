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

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
