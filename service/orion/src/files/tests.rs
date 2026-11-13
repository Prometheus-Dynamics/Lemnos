use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

static NEXT: AtomicU32 = AtomicU32::new(0);

fn dir() -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lo-files-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn yes(_: u64) -> Result<(), String> {
    Ok(())
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_file_sent_in_order_is_checked_and_put_in_place_under_its_name() {
    let dir = dir();
    let mut r = Receiver::new(&dir);
    let bytes: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
    assert_eq!(
        r.begin(7, "b.tar", 10_000, sha(&bytes), &mut yes).received,
        0
    );
    for (at, piece) in bytes.chunks(3000).enumerate() {
        let a = r.chunk(7, (at * 3000) as u64, piece);
        assert_eq!(a.state, FileState::Receiving);
    }
    // Nothing under its name until it is whole and checked.
    assert!(!dir.join("b.tar").exists());
    let a = r.commit(7);
    assert_eq!(a.state, FileState::Committed, "{}", a.detail);
    assert_eq!(fs::read(dir.join("b.tar")).unwrap(), bytes);
    assert_eq!(names(&dir), ["b.tar"]);
    assert_eq!(r.open(), 0);
}

#[test]
fn a_chunk_out_of_order_is_left_out_and_the_answer_says_where_to_go_on() {
    let dir = dir();
    let mut r = Receiver::new(&dir);
    let bytes = b"abcdefgh";
    r.begin(1, "f", 8, sha(bytes), &mut yes);
    assert_eq!(r.chunk(1, 0, b"abcd").received, 4);
    assert_eq!(r.chunk(1, 6, b"gh").received, 4);
    assert_eq!(r.chunk(1, 0, b"abcd").received, 4);
    // Beginning again resumes where it is.
    assert_eq!(r.begin(1, "f", 8, sha(bytes), &mut yes).received, 4);
    assert_eq!(r.chunk(1, 4, b"efgh").received, 8);
    assert_eq!(r.commit(1).state, FileState::Committed);
}

#[test]
fn a_file_whose_digest_differs_or_that_is_short_is_deleted() {
    let dir = dir();
    let mut r = Receiver::new(&dir);
    r.begin(1, "f", 4, sha(b"abcd"), &mut yes);
    r.chunk(1, 0, b"abcx");
    let a = r.commit(1);
    assert_eq!(a.state, FileState::Rejected);
    assert!(a.detail.contains("SHA-256"), "{}", a.detail);
    r.begin(2, "g", 4, sha(b"abcd"), &mut yes);
    r.chunk(2, 0, b"ab");
    assert_eq!(r.commit(2).state, FileState::Rejected);
    r.begin(3, "h", 2, sha(b"ab"), &mut yes);
    assert_eq!(r.chunk(3, 0, b"abc").state, FileState::Rejected);
    assert!(names(&dir).is_empty(), "{:?}", names(&dir));
}

#[test]
fn the_device_saying_no_room_refuses_the_file_before_a_byte_is_written() {
    let dir = dir();
    let mut r = Receiver::new(&dir);
    let mut asked = Vec::new();
    let mut full = |size: u64| {
        asked.push(size);
        Err("slots doesn't take 9 bytes now: no space".to_owned())
    };
    let a = r.begin(1, "f", 9, [0; 32], &mut full);
    assert_eq!(a.state, FileState::Rejected);
    assert!(a.detail.contains("no space"));
    assert_eq!(asked, [9]);
    assert!(names(&dir).is_empty());
    assert_eq!(r.chunk(1, 0, b"x").state, FileState::Rejected);
}

#[test]
fn only_a_plain_name_is_taken() {
    let dir = dir();
    let mut r = Receiver::new(&dir);
    for name in [
        "",
        ".",
        "..",
        ".hidden",
        "a/b",
        "a b",
        "a:b",
        &"x".repeat(129),
    ] {
        assert_eq!(
            r.begin(1, name, 1, [0; 32], &mut yes).state,
            FileState::Rejected,
            "{name:?}"
        );
    }
    assert!(names(&dir).is_empty());
}

#[test]
fn a_file_that_stops_arriving_and_leftovers_from_before_are_deleted() {
    let dir = dir();
    fs::write(dir.join(".orion-00000000000000aa.part"), b"old").unwrap();
    fs::write(dir.join("kept"), b"not ours").unwrap();
    let mut r = Receiver::new(&dir);
    assert_eq!(names(&dir), ["kept"]);
    r.begin(5, "f", 4, sha(b"abcd"), &mut yes);
    r.chunk(5, 0, b"ab");
    r.expire(Duration::from_secs(3600));
    assert_eq!(r.open(), 1);
    r.expire(Duration::ZERO);
    assert_eq!(r.open(), 0);
    assert_eq!(names(&dir), ["kept"]);
}
