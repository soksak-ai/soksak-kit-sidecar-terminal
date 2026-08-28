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
fn installation_exclusively_owns_the_sidecar_process_name() {
    let runtime = fs::read_to_string("src/runtime.rs").expect("read runtime");
    for forbidden in ["setprogname", "getprogname", "proc_name", "apply_darwin_process_name"] {
        assert!(!runtime.contains(forbidden), "runtime rewrites process name with {forbidden}");
    }
    let manifest = fs::read_to_string("Cargo.toml").expect("read Cargo.toml");
    assert!(!manifest.lines().any(|line| line.starts_with("libc =")), "process-name libc dependency remains");
}

#[test]
fn repository_owns_public_metadata() {
    let manifest = fs::read_to_string("Cargo.toml").expect("read Cargo.toml");
    assert!(manifest.lines().any(|line| line == r#"edition = "2024""#));
    assert!(
        manifest
            .contains("repository = \"https://github.com/soksak-ai/soksak-kit-sidecar-terminal\"")
    );
    assert!(Path::new("README.md").is_file());
    assert!(Path::new("LICENSE").is_file());
    let kit: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("kit.json").expect("read kit.json"))
            .expect("parse kit.json");
    assert_eq!(kit["id"], "soksak-kit-sidecar-terminal");
    let version = kit["version"].as_str().expect("kit version");
    assert!(
        manifest
            .lines()
            .any(|line| line == format!("version = \"{version}\""))
    );
    assert!(manifest.contains("rev = \"2dbce68a373cffc5ecdd46de0637bd0722231545\""));
    let release_files = fs::read_to_string("release-files.json").expect("read release files");
    assert!(release_files.contains("\"kit.json\""));
    assert!(release_files.contains("\"README.ko.md\""));
    assert!(release_files.contains("\"src/checkpoint.rs\""));
    assert!(release_files.contains("\"src/transport_name.rs\""));
    assert!(release_files.contains("\"scripts/install_pty_release.py\""));
    assert!(release_files.contains("\"scripts/test_install_pty_release.py\""));
    let workflow =
        fs::read_to_string(".github/workflows/release.yml").expect("read release workflow");
    let makefile = fs::read_to_string("Makefile").expect("read Makefile");
    for target in ["preflight:", "lock:", "prepare:", "build:", "verify:"] {
        assert!(makefile.contains(target), "Makefile omits {target}");
    }
    assert!(
        makefile.contains("cargo metadata --format-version 1"),
        "Makefile lock target does not project Cargo.toml into Cargo.lock"
    );
    assert!(
        fs::read_to_string("README.md")
            .expect("read README")
            .contains("make lock"),
        "README omits make lock"
    );
    assert!(Path::new("rust-toolchain.toml").is_file());
    assert!(Path::new(".python-version").is_file());
    for required in [
        "spec_url:",
        "spec_sha256:",
        "rust-toolchain.toml",
        "python-version-file: component/.python-version",
        "node-version-file: component/.dependency/spec-package/package.json",
        "make verify",
        ".dependency/spec-package/release-template/build-portable-release.mjs",
    ] {
        assert!(workflow.contains(required), "workflow omits {required}");
    }
    for forbidden in [
        "repository: soksak-ai/soksak-spec",
        "toolchain: \"1.96.0\"",
        "node-version: \"24.19.0\"",
        "version: \"10.30.3\"",
        "pnpm install --frozen-lockfile",
        "cargo test --locked",
    ] {
        assert!(
            !workflow.contains(forbidden),
            "workflow duplicates {forbidden}"
        );
    }
    assert!(workflow.contains("owner-enforced immutable releases must be enabled"));
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
