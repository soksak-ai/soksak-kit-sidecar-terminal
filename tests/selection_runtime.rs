#[test]
fn render_runtime_owns_the_versioned_selection_state_machine() {
    let mirror = include_str!("../src/mirror.rs");
    let session = include_str!("../src/render/session.rs");
    assert!(mirror.contains("selection_command"), "mirror has no selection command state machine");
    assert!(mirror.contains("STALE_GESTURE"), "mirror has no gesture ownership conflict rule");
    assert!(!session.contains("surface.selection\" | \"surface.hover"),
        "surface.selection is still grouped with the NOT_YET_SERVED placeholder");
}
