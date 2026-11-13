//! Structs are blocks: a reader from before a field was appended reads the fields it knows and
//! skips the rest, and a reader that expects more than it got takes the defaults. The "old shape"
//! and "new shape" below are the same struct before and after an appended field.

use lemnos_core::{Decode, Encode, Reader, Writer};

mod v1 {
    use lemnos_core::{Decode, Encode};

    #[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
    pub struct Reading {
        pub a: u32,
        pub b: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
    pub struct Outer {
        pub reading: Reading,
        pub count: u8,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
    pub struct Named<'a> {
        pub text: &'a str,
        pub n: u8,
    }
}

mod v2 {
    use lemnos_core::{Decode, Encode};

    #[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
    pub struct Reading {
        pub a: u32,
        pub b: bool,
        /// Appended after `b`. A reader from before it never sees it.
        pub c: u16,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
    pub struct Outer {
        pub reading: Reading,
        pub count: u8,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
    pub struct Named<'a> {
        pub text: &'a str,
        pub n: u8,
        pub extra: u32,
    }
}

fn bytes_of<T: Encode>(value: &T) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let mut out = Writer::new(&mut buf);
    value.encode(&mut out).unwrap();
    out.written().to_vec()
}

#[test]
fn a_reader_from_before_an_appended_field_skips_it() {
    let newer = v2::Reading {
        a: 7,
        b: true,
        c: 0xabcd,
    };
    let bytes = bytes_of(&newer);
    let old: v1::Reading = Decode::decode(&mut Reader::new(&bytes)).unwrap();
    assert_eq!(old, v1::Reading { a: 7, b: true });
}

#[test]
fn a_reader_expecting_an_appended_field_takes_its_default() {
    let older = v1::Reading { a: 7, b: true };
    let bytes = bytes_of(&older);
    let new: v2::Reading = Decode::decode(&mut Reader::new(&bytes)).unwrap();
    assert_eq!(
        new,
        v2::Reading {
            a: 7,
            b: true,
            c: 0
        }
    );
}

#[test]
fn a_struct_is_written_as_a_length_then_its_fields() {
    let bytes = bytes_of(&v1::Reading { a: 1, b: false });
    let len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    assert_eq!(len, 5);
    assert_eq!(bytes.len(), 4 + len);
}

#[test]
fn a_nested_struct_skips_and_defaults_at_its_own_level() {
    let newer = v2::Outer {
        reading: v2::Reading {
            a: 3,
            b: false,
            c: 9,
        },
        count: 4,
    };
    let old: v1::Outer = Decode::decode(&mut Reader::new(&bytes_of(&newer))).unwrap();
    assert_eq!(
        old,
        v1::Outer {
            reading: v1::Reading { a: 3, b: false },
            count: 4,
        }
    );
    let older = v1::Outer {
        reading: v1::Reading { a: 3, b: false },
        count: 4,
    };
    let new: v2::Outer = Decode::decode(&mut Reader::new(&bytes_of(&older))).unwrap();
    assert_eq!(new.reading.c, 0);
    assert_eq!(new.count, 4);
}

#[test]
fn a_borrowed_struct_reads_both_ways() {
    let newer = v2::Named {
        text: "hello",
        n: 2,
        extra: 99,
    };
    let bytes = bytes_of(&newer);
    let old: v1::Named<'_> = Decode::decode(&mut Reader::new(&bytes)).unwrap();
    assert_eq!((old.text, old.n), ("hello", 2));
    let older = v1::Named { text: "hi", n: 1 };
    let bytes = bytes_of(&older);
    let new: v2::Named<'_> = Decode::decode(&mut Reader::new(&bytes)).unwrap();
    assert_eq!((new.text, new.n, new.extra), ("hi", 1, 0));
}

#[test]
fn a_field_cut_short_inside_its_block_is_refused_not_defaulted() {
    // A block that says four bytes but holds two: the u32 is cut short.
    let bytes = [4u8, 0, 0, 0, 1, 2];
    assert!(v1::Reading::decode(&mut Reader::new(&bytes)).is_err());
}

#[test]
fn a_block_longer_than_the_struct_is_refused_only_when_it_runs_past_the_buffer() {
    // Extra bytes inside the block are skipped; a block that runs past the buffer is refused.
    let mut bytes = bytes_of(&v2::Reading {
        a: 1,
        b: true,
        c: 2,
    });
    assert!(v1::Reading::decode(&mut Reader::new(&bytes)).is_ok());
    bytes.truncate(bytes.len() - 1);
    assert!(v1::Reading::decode(&mut Reader::new(&bytes)).is_err());
}
