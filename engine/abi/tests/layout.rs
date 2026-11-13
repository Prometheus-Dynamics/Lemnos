//! The layout hash lists every field of every type it covers. Read against the sources, since
//! nothing else can see a field a list leaves out.

/// The named fields of `struct name` in `source`, in order. `None` when it isn't declared with
/// named fields here.
fn named_fields(source: &str, name: &str) -> Option<Vec<String>> {
    let start = source.find(&format!("pub struct {name} {{"))?;
    let body = &source[start..];
    let end = body.find("\n}")?;
    Some(
        body[..end]
            .lines()
            .skip(1)
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("pub ")?;
                let (field, _) = rest.split_once(':')?;
                Some(field.trim().to_string())
            })
            .collect(),
    )
}

/// Every type the layout hash names must list every one of its fields, in order: a field that is
/// missing from the list would move the layout without moving the hash.
#[test]
fn every_field_of_a_hashed_type_is_in_the_layout() {
    let layout = include_str!("../src/version.rs");
    let sources = [
        include_str!("../src/version.rs"),
        include_str!("../src/desc.rs"),
        include_str!("../src/status.rs"),
        include_str!("../src/handle.rs"),
    ]
    .join("\n");
    let mut checked = 0;
    for call in layout.split("shape!(").skip(1) {
        let inside = &call[..call.find(')').unwrap()];
        let mut parts = inside.split(',').map(str::trim).filter(|p| !p.is_empty());
        // `V` is `AbiVersion` under its alias in version.rs.
        let name = match parts.next().unwrap() {
            "V" => "AbiVersion",
            other => other,
        };
        let listed: Vec<String> = parts.map(str::to_string).collect();
        match named_fields(&sources, name) {
            Some(declared) => assert_eq!(
                listed, declared,
                "{name}: the layout hash must list every field, in order"
            ),
            None => assert!(
                listed.is_empty(),
                "{name} has no named fields, so its layout lists none"
            ),
        }
        checked += 1;
    }
    assert!(checked >= 5, "the layout lists {checked} types");
}
