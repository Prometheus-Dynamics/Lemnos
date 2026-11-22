//! `lemnos-ctl validate` accepts the `[meta]` table of image-builder strings, and refuses a
//! top-level `generated_by` by pointing at `[meta]`.

use std::sync::atomic::{AtomicU32, Ordering};

fn validate(text: &str) -> Result<(), String> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-ctl-meta-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    let mut out = Vec::new();
    let args = [
        "validate".to_string(),
        "--no-live".to_string(),
        path.display().to_string(),
    ];
    let result = lemnos_ctl::run(&args, &mut out).map_err(|error| {
        let prefix = format!("{}: ", path.display());
        error.strip_prefix(&prefix).unwrap_or(&error).to_string()
    });
    std::fs::remove_dir_all(&dir).unwrap();
    result
}

const HEAD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [security]\nrecord = \"/nonexistent/lemnos-meta-test/record\"\n";

#[test]
fn validate_accepts_a_meta_table_of_strings() {
    validate(&format!(
        "{HEAD}[meta]\ngenerated_by = \"image-tool\"\nname = \"Example\"\n"
    ))
    .expect("[meta] of strings is accepted");
}

#[test]
fn validate_refuses_a_top_level_generated_by_and_points_at_meta() {
    let reason = validate(&HEAD.replacen("[board]", "generated_by = \"image-tool\"\n[board]", 1))
        .unwrap_err();
    assert!(reason.contains("generated_by"), "{reason}");
    assert!(reason.contains("[meta]"), "{reason}");
}

#[test]
fn validate_refuses_a_non_string_meta_value() {
    let reason = validate(&format!("{HEAD}[meta]\ncount = 3\n")).unwrap_err();
    assert!(reason.contains("[meta] count"), "{reason}");
    assert!(reason.contains("expected a string"), "{reason}");
}
