//! Tests of the bundle writer and reader: a bundle verifies clean, is the same bytes for the same
//! inputs, and each way of breaking it is refused by a pinned sentence. The signature's arithmetic is
//! `lemnos-ctl`'s (it holds the Ed25519 code); here a signer is a stand-in that returns fixed bytes,
//! which shares nothing with a real signature but its length and its place in the record.

use super::*;
use crate::tar::write::archive;

const ID: &str = "os-2";
const PREFIX: &str = "os";
const SOURCE: &str = "rpi-boot-mode-1=/soc@107c000000/mmc@fff000";

fn erofs(blocks: usize) -> Vec<u8> {
    let mut image: Vec<u8> = (0..blocks * 4096).map(|i| (i % 249) as u8).collect();
    image[1024..1028].copy_from_slice(&EROFS_MAGIC);
    image[1024 + 12] = 12;
    image[1024 + 36..1024 + 40].copy_from_slice(&(blocks as u32).to_le_bytes());
    image
}

/// An image as the builder hands it over: erofs data with its hash tree appended, and the table
/// that describes it.
struct Img {
    whole: Vec<u8>,
    table: verity::Table,
}

fn full(blocks: usize) -> Img {
    let data = erofs(blocks);
    let salt = [0x5a; 32];
    let tree = verity::tree_of(salt, &data).unwrap();
    let mut whole = data;
    whole.extend_from_slice(&tree.tree);
    let table = verity::Table {
        root_hash: hex(&tree.root_hash),
        salt: hex(&salt),
        data_blocks: blocks as u64,
        hash_offset: blocks as u64 * 4096,
        tree_bytes: tree.tree.len() as u64,
        image_bytes: whole.len() as u64,
    };
    Img { whole, table }
}

fn rules(family: Family) -> Rules {
    Rules {
        board: "board-a".into(),
        prefix: PREFIX.into(),
        boot_sources: vec![SOURCE.into()],
        family,
        layout: Layout::new([8192, 4096, 4096, 4096, 4096]).unwrap(),
        overlay: Some(Overlay {
            board: "board-a".into(),
            sha256: "ab".repeat(32),
        }),
    }
}

fn cmdline(hole: &str) -> Vec<u8> {
    format!("console=tty1 rootwait {PREFIX}.slot={ID} {PREFIX}.roothash={hole} {PREFIX}.src.{SOURCE} panic=5\n")
        .into_bytes()
}

/// The five payloads: a raw kernel, the command line with the hole.
fn payloads() -> [Vec<u8>; 5] {
    [
        vec![0u8; 3000],
        b"initramfs".to_vec(),
        b"device tree".to_vec(),
        cmdline(ROOTHASH_HOLE),
        b"overlay".to_vec(),
    ]
}

fn fake_sign(_: &[u8]) -> [u8; 64] {
    [0xAB; 64]
}

fn make(rules: &Rules, payloads: [Vec<u8>; 5], image: &Img, sign: bool) -> Result<Built, String> {
    make_with(rules, payloads, &image.whole, &image.table, sign)
}

fn make_with(
    rules: &Rules,
    payloads: [Vec<u8>; 5],
    image: &[u8],
    table: &verity::Table,
    sign: bool,
) -> Result<Built, String> {
    let signer = Signer {
        key_id: [7; 32],
        sign: &fake_sign,
    };
    build(
        &Build {
            id: ID,
            product: "an-os",
            version: "2.0",
            variant: "",
            kernel: "6.12",
            display_name: "An OS",
            table,
            rules,
            signer: sign.then_some(signer),
        },
        payloads,
        &mut &image[..],
    )
}

fn bytes_of(built: &Built, image: &Img) -> Vec<u8> {
    let mut out = Vec::new();
    write_to(built, &mut io::Cursor::new(image.whole.clone()), &mut out).unwrap();
    out
}

fn ok_signature(manifest: &[u8], sig: &[u8]) -> (State, String) {
    if sig.is_empty() {
        return (State::Ok, "unsigned".into());
    }
    assert_eq!(sig, &signing::record_bytes(&[7; 32], &[0xAB; 64])[..]);
    let _ = manifest;
    (State::Ok, "stand-in".into())
}

fn check(bytes: &[u8], rules: Option<&Rules>) -> Vec<Finding> {
    verify(
        &mut &bytes[..],
        &Context {
            rules,
            signature: &ok_signature,
        },
    )
}

fn bad(findings: &[Finding]) -> Vec<&str> {
    findings
        .iter()
        .filter(|f| f.state == State::Bad)
        .map(|f| f.text.as_str())
        .collect()
}

/// The members of a built bundle in the order it is written, to be taken apart and put back together.
fn members(built: &Built, image: &Img) -> Vec<(String, Vec<u8>)> {
    let root = image.whole.clone();
    let mut out = vec![("slot.json".to_string(), built.manifest_bytes.clone())];
    if let Some(sig) = &built.signature {
        out.push(("slot.sig".to_string(), sig.clone()));
    }
    out.push(("rootfs.erofs".to_string(), root));
    out.push(("boot/".to_string(), Vec::new()));
    for (role, bytes) in ROLES.iter().zip(&built.payloads) {
        out.push((format!("boot/{}", role.path), bytes.clone()));
    }
    out
}

fn pack(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let borrowed: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    archive(&borrowed)
}

#[test]
fn a_bundle_made_by_the_writer_verifies_clean_on_both_forms() {
    let image = full(300);
    for family in [Family::Tryboot, Family::UbootBlock] {
        for sign in [false, true] {
            let r = rules(family);
            let built = make(&r, payloads(), &image, sign).unwrap();
            let bytes = bytes_of(&built, &image);
            let findings = check(&bytes, Some(&r));
            assert!(bad(&findings).is_empty(), "{findings:#?}");
            assert!(
                findings.iter().all(|f| f.state == State::Ok),
                "{findings:#?}"
            );
            let names: Vec<&str> = findings
                .iter()
                .map(|f| f.text.split(':').next().unwrap())
                .collect();
            assert_eq!(
                names,
                [
                    "members",
                    "manifest",
                    "signature",
                    "image",
                    "verity",
                    "boot files",
                    "roles",
                    "board",
                    "kernel",
                    "bays",
                    "cmdline",
                    "overlay"
                ]
            );
            // The same bytes the members list says the writer makes.
            assert_eq!(bytes, pack(&members(&built, &image)));
        }
    }
}

#[test]
fn without_a_board_the_board_checks_are_skipped_not_passed() {
    let image = full(2);
    let r = rules(Family::Tryboot);
    let built = make(&r, payloads(), &image, false).unwrap();
    let findings = check(&bytes_of(&built, &image), None);
    let board = findings.last().unwrap();
    assert_eq!(board.state, State::Skipped);
    assert!(
        board.text.starts_with("board: not checked"),
        "{}",
        board.text
    );
}

#[test]
fn the_same_inputs_give_the_same_bytes_and_other_inputs_do_not() {
    let image = full(5);
    let r = rules(Family::Tryboot);
    let one = bytes_of(&make(&r, payloads(), &image, true).unwrap(), &image);
    let two = bytes_of(&make(&r, payloads(), &image, true).unwrap(), &image);
    assert_eq!(one, two);
    let mut other = payloads();
    other[1] = b"another initramfs".to_vec();
    assert_ne!(
        one,
        bytes_of(&make(&r, other, &image, true).unwrap(), &image)
    );
}

#[test]
fn the_root_hash_is_filled_into_the_hole_and_nothing_else_changes() {
    let image = full(3);
    let built = make(&rules(Family::Tryboot), payloads(), &image, false).unwrap();
    let hash = &built.manifest.verity.root_hash;
    assert_eq!(built.payloads[3], cmdline(hash));
    // A line that already carries the right hash is taken as it is; a wrong one is refused by name.
    let mut given = payloads();
    given[3] = cmdline(hash);
    assert_eq!(
        make(&rules(Family::Tryboot), given, &image, false)
            .unwrap()
            .payloads[3],
        cmdline(hash)
    );
    let mut wrong = payloads();
    wrong[3] = cmdline(&"cd".repeat(32));
    let why = make(&rules(Family::Tryboot), wrong, &image, false).unwrap_err();
    assert!(
        why.contains(&format!("it needs {PREFIX}.roothash={hash}")),
        "{why}"
    );
    // A hole that is part of another word is not a hole.
    assert_eq!(
        fill_roothash(
            b"os.roothash=@roothash@x os.roothash=@roothash@",
            "os",
            "ff"
        )
        .unwrap(),
        b"os.roothash=@roothash@x os.roothash=ff"
    );
    assert!(
        fill_roothash(&[0xff, 0xfe], "os", "ff")
            .unwrap_err()
            .contains("not text")
    );
}

/// Every refusal of the writer, by its text: nothing is written for any of them.
#[test]
fn the_writer_refuses_each_mistake_by_name() {
    let image = full(4);
    let tryboot = rules(Family::Tryboot);
    let uboot = rules(Family::UbootBlock);
    let refused = |r: &Rules, p: [Vec<u8>; 5], image: &Img| make(r, p, image, false).unwrap_err();

    // The kernel's form: gzip is stored as shipped on the Pi form and refused on the U-Boot form.
    let mut gzip = payloads();
    gzip[0][..2].copy_from_slice(&[0x1f, 0x8b]);
    assert!(make(&tryboot, gzip.clone(), &image, false).is_ok());
    assert!(refused(&uboot, gzip, &image).contains("the kernel is gzip-compressed"));

    // A role that does not fit its bay.
    let mut big = payloads();
    big[2] = vec![1; 5000];
    let why = refused(&tryboot, big, &image);
    assert!(
        why.contains("the device-tree payload is 5000 bytes but its bay holds 4096"),
        "{why}"
    );

    // A command line without its slot, its boot source, or with a second line on the U-Boot form.
    let mut no_slot = payloads();
    no_slot[3] =
        format!("{PREFIX}.roothash={ROOTHASH_HOLE} {PREFIX}.src.{SOURCE} panic=5\n").into_bytes();
    assert_eq!(
        refused(&tryboot, no_slot, &image),
        "boot/cmdline.txt doesn't name its slot: it needs os.slot=os-2"
    );
    let mut no_source = payloads();
    no_source[3] =
        format!("{PREFIX}.slot={ID} {PREFIX}.roothash={ROOTHASH_HOLE} panic=5\n").into_bytes();
    assert!(refused(&tryboot, no_source, &image).contains(
        "doesn't list the board's boot source rpi-boot-mode-1: it needs os.src.rpi-boot-mode-1="
    ));
    let mut two_lines = payloads();
    two_lines[3].extend_from_slice(b"second\n");
    assert_eq!(
        refused(&uboot, two_lines, &image),
        "the command line has more than one line"
    );

    // The root image against the table: its length, its superblock, its tree, its root hash.
    let table_for = |whole: &[u8], t: &verity::Table| {
        make_with(&tryboot, payloads(), whole, t, false).unwrap_err()
    };
    let t = &image.table;
    let why = table_for(&image.whole[..image.whole.len() - 1], t);
    assert!(
        why.contains("the root image is 16799 bytes, and the verity table's image_bytes is 16800")
            || why.contains("bytes, and the verity table's image_bytes is"),
        "{why}"
    );
    let blank = Img {
        whole: vec![0u8; image.whole.len()],
        table: t.clone(),
    };
    assert!(refused(&tryboot, payloads(), &blank).contains("not an erofs image"));
    let mut small = full(4);
    small.whole[1024 + 12] = 9;
    assert!(refused(&tryboot, payloads(), &small).contains("block size is 2^9"));
    let mut wrong_blocks = full(4);
    wrong_blocks.whole[1024 + 36] = 3;
    assert!(
        refused(&tryboot, payloads(), &wrong_blocks)
            .contains("superblock says 3 blocks (12288 bytes)")
    );
    let mut flipped = image.whole.clone();
    flipped[5000] ^= 1;
    assert!(table_for(&flipped, t).contains("data hashes to root hash"));
    let mut bad_tree = image.whole.clone();
    let last = bad_tree.len() - 1;
    bad_tree[last] ^= 1;
    assert!(table_for(&bad_tree, t).contains("is not the tree of that data"));
    let mut lie = t.clone();
    lie.root_hash = "00".repeat(32);
    assert!(table_for(&image.whole, &lie).contains("data hashes to root hash"));
    let mut lie = t.clone();
    lie.tree_bytes += 1;
    assert!(table_for(&image.whole, &lie).contains("verity table: tree_bytes is"));

    // The builder's words.
    let mut r = rules(Family::Tryboot);
    r.board = "Board_A".into();
    assert!(refused(&r, payloads(), &image).contains("[board] id \"Board_A\""));
    let named = |id: &'static str, product: &'static str, version: &'static str| {
        build(
            &Build {
                id,
                product,
                version,
                variant: "",
                kernel: "",
                display_name: "",
                table: &image.table,
                rules: &tryboot,
                signer: None,
            },
            payloads(),
            &mut &image.whole[..],
        )
        .unwrap_err()
    };
    assert!(named("Bad Id", "p", "1").contains("--slot \"Bad Id\" isn't a slot id"));
    assert!(named(ID, "P_", "1").contains("--product \"P_\" isn't lowercase"));
    assert_eq!(named(ID, "p", ""), "--version is empty");
}

/// A flipped byte in each member of a signed bundle is found, by the check that owns the member.
#[test]
fn a_flipped_byte_in_each_member_is_refused() {
    let image = full(300);
    let r = rules(Family::Tryboot);
    let built = make(&r, payloads(), &image, true).unwrap();
    let good = members(&built, &image);
    let flip = |name: &str, at: usize| {
        let mut m = good.clone();
        let entry = m.iter_mut().find(|(n, _)| n == name).unwrap();
        entry.1[at] ^= 1;
        check(&pack(&m), Some(&r))
    };
    // The image's data, and its tree (the last block).
    let f = flip("rootfs.erofs", 5000);
    let texts = bad(&f);
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("image: rootfs.erofs doesn't match slot.json's sha256")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("verity: the image's root hash is")),
        "{texts:?}"
    );
    let tree_at = 300 * 4096 + 17;
    let f = flip("rootfs.erofs", tree_at);
    let texts = bad(&f);
    assert!(texts.iter().any(|t| t.starts_with("image: ")), "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|t| t.contains("verity: ") && t.contains("not the tree of the image")),
        "{texts:?}"
    );
    // Each boot file.
    for role in ROLES {
        let name = format!("boot/{}", role.path);
        let f = flip(&name, 0);
        let texts = bad(&f);
        assert!(
            texts.contains(
                &format!(
                    "boot files: boot file {} doesn't match slot.json",
                    role.path
                )
                .as_str()
            ),
            "{name}: {texts:?}"
        );
    }
    // The manifest: a byte of its text is a different manifest, or none (here the id's letter).
    let f = flip("slot.json", 12);
    assert!(!bad(&f).is_empty(), "{f:#?}");
}

#[test]
fn the_members_must_come_in_the_enforced_order_and_be_the_right_ones() {
    let image = full(2);
    let r = rules(Family::Tryboot);
    let built = make(&r, payloads(), &image, true).unwrap();
    let good = members(&built, &image);
    type Edit = dyn Fn(&mut Vec<(String, Vec<u8>)>);
    let with = |edit: &Edit| {
        let mut m = good.clone();
        edit(&mut m);
        bad_texts(check(&pack(&m), Some(&r)))
    };
    // slot.json not first.
    let t = with(&|m| m.swap(0, 2));
    assert!(
        t[0].contains(
            "the bundle starts with rootfs.erofs, and slot.json must be its first member"
        ),
        "{t:?}"
    );
    // slot.sig not second.
    let t = with(&|m| m.swap(1, 2));
    assert!(
        t[0].contains("slot.sig must be the bundle's second member"),
        "{t:?}"
    );
    // A member twice.
    let t = with(&|m| m.insert(3, ("rootfs.erofs".into(), vec![])));
    assert_eq!(t, ["members: the bundle has rootfs.erofs twice"]);
    // A member that is not allowed.
    let t = with(&|m| m.push(("extra.txt".into(), vec![1])));
    assert_eq!(
        t,
        ["members: the bundle has extra.txt: only slot.json, slot.sig, rootfs.erofs and boot/"]
    );
    // A boot file the manifest does not list.
    let t = with(&|m| m.push(("boot/overlays/second.dtbo".into(), vec![1])));
    assert_eq!(
        t,
        [
            "members: boot file overlays/second.dtbo isn't in slot.json: a file the manifest does not list is refused"
        ]
    );
    // A listed boot file the bundle lacks.
    let t = with(&|m| m.retain(|(n, _)| n != "boot/board.dtb"));
    assert!(
        t.contains(&"boot files: the bundle has no boot file board.dtb".to_string()),
        "{t:?}"
    );
    // No image.
    let t = with(&|m| m.retain(|(n, _)| n != "rootfs.erofs"));
    assert!(
        t.contains(&"image: not a slot bundle: no rootfs.erofs".to_string()),
        "{t:?}"
    );
    // A bare 64-byte signature.
    let t = with(&|m| m[1].1 = vec![0; 64]);
    assert!(
        t[0].starts_with("signature: slot.sig is not a signature record: 108 bytes"),
        "{t:?}"
    );
    // Cut short, and trailing bytes.
    let bytes = pack(&good);
    let cut = &bytes[..bytes.len() - 1024 - 100];
    assert_eq!(
        bad_texts(check(cut, Some(&r))),
        ["members: the bundle ends in the middle"]
    );
    let mut after = bytes.clone();
    after.push(1);
    assert_eq!(
        bad_texts(check(&after, Some(&r))),
        ["members: bytes after the end of the bundle"]
    );
    assert_eq!(
        bad_texts(check(&[], Some(&r))),
        ["members: the bundle ends in the middle"]
    );
    assert_eq!(
        bad_texts(check(&archive(&[]), Some(&r))),
        ["members: not a slot bundle: no slot.json"]
    );
}

fn bad_texts(findings: Vec<Finding>) -> Vec<String> {
    findings
        .into_iter()
        .filter(|f| f.state == State::Bad)
        .map(|f| f.text)
        .collect()
}

/// A bundle that frames and parses but is wrong for the board is refused by the board's checks, each
/// by its name; `boot.files` is held to the five role files.
#[test]
fn the_board_and_the_role_files_are_held_to_their_rules() {
    let image = full(2);
    let uboot = rules(Family::UbootBlock);
    // Built for the Pi form (gzip allowed), checked against the U-Boot board.
    let tryboot = rules(Family::Tryboot);
    let mut gzip = payloads();
    gzip[0][..2].copy_from_slice(&[0x1f, 0x8b]);
    let built = make(&tryboot, gzip, &image, false).unwrap();
    let t = bad_texts(check(&bytes_of(&built, &image), Some(&uboot)));
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(
        t[0].starts_with("kernel: the kernel is gzip-compressed"),
        "{t:?}"
    );

    // Another board, another overlay marker, a smaller bay.
    let mut other = rules(Family::Tryboot);
    other.board = "board-b".into();
    other.overlay = None;
    other.layout = Layout::new([8192, 4096, 4096, 4096, 512]).unwrap();
    let built = make(&tryboot, payloads(), &image, false).unwrap();
    let mut big_overlay = built.payloads.clone();
    big_overlay[4] = vec![9; 600];
    let t = bad_texts(check(&bytes_of(&built, &image), Some(&other)));
    assert!(
        t.contains(
            &"board: this slot is built for board board-a, and this is board board-b".to_string()
        ),
        "{t:?}"
    );
    assert!(t.contains(&"overlay: boot.overlay is not the board's overlay marker: build the bundle with this board file".to_string()), "{t:?}");
    let _ = big_overlay;

    // Another boot_args prefix on the board: the command line does not name its slot for it.
    let mut prefixed = rules(Family::Tryboot);
    prefixed.prefix = "lemnos".into();
    let t = bad_texts(check(&bytes_of(&built, &image), Some(&prefixed)));
    assert_eq!(
        t,
        ["cmdline: boot/cmdline.txt doesn't name its slot: it needs lemnos.slot=os-2"]
    );

    // An extra boot file in the manifest, and one role missing from it.
    for (edit, want) in [
        (
            true,
            "roles: boot.files lists overlays/second.dtbo, which is not one of the five boot files",
        ),
        (
            false,
            "roles: boot.files has no board.dtb: a slot's boot files are the five",
        ),
    ] {
        let mut manifest = built.manifest.clone();
        if edit {
            manifest
                .boot_files
                .push(("overlays/second.dtbo".into(), "00".repeat(32)));
        } else {
            manifest.boot_files.retain(|(n, _)| n != "board.dtb");
        }
        let mut m = members(&built, &image);
        m[0].1 = manifest.to_json().into_bytes();
        if edit {
            m.push(("boot/overlays/second.dtbo".into(), vec![]));
        } else {
            m.retain(|(n, _)| n != "boot/board.dtb");
        }
        let findings = check(&pack(&m), Some(&tryboot));
        let t = bad_texts(findings);
        assert!(t.iter().any(|t| t.starts_with(want)), "{t:?}");
    }
}

#[test]
fn a_slot_id_the_installer_would_refuse_is_refused_at_the_manifest() {
    let image = full(2);
    let r = rules(Family::Tryboot);
    let built = make(&r, payloads(), &image, false).unwrap();
    let mut m = members(&built, &image);
    m[0].1 = String::from_utf8(m[0].1.clone())
        .unwrap()
        .replace("\"os-2\"", "\"Os 2\"")
        .into_bytes();
    assert_eq!(
        bad_texts(check(&pack(&m), Some(&r))),
        ["manifest: slot.json: `Os 2` isn't a slot id"]
    );
}

#[test]
fn a_tar_of_a_big_image_names_its_size_in_base_256() {
    let size: u64 = 9 << 30;
    let header = write::header("rootfs.erofs", b'0', size);
    assert_eq!(header[124], 0x80);
    let mut reader_saw = 0u64;
    struct Size<'a>(&'a mut u64);
    impl Sink for Size<'_> {
        fn begin(&mut self, _: &str, _: Kind, size: u64) -> Result<(), String> {
            *self.0 = size;
            Ok(())
        }
        fn data(&mut self, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn end(&mut self) -> Result<(), String> {
            Ok(())
        }
    }
    Tar::default()
        .feed(&header, &mut Size(&mut reader_saw))
        .unwrap();
    assert_eq!(reader_saw, size);
}
