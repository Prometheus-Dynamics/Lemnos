//! The marks page's layout: a page of another layout is refused, by name, before anything else is
//! read from it, by the publisher (`HoldTable::add_over`) and by the reader (`Marks::over`).
//!
//! The layout 1 page below is built word for word from the layout 1 definition (`shm/README.md` at
//! 0aa30510: the version in word 0, one mark per buffer after it, `1 + b` words, no layout byte).

use std::sync::atomic::Ordering;

use shm_ring::{Heap, Mem, MemMut, RingError};
use shm_ring_buffers::{BufferIndex, Holders, Marker};
use shm_ring_holds::{HoldTable, LAYOUT, Limits, Marks};

const BUFFERS: u32 = 2;

/// A layout 1 marks page for two buffers, buffer 0 held: `[version, mark 0, mark 1]`.
fn layout_one_page() -> Heap {
    let page = Heap::new(1 + BUFFERS as usize);
    for (index, word) in [2_u64, 1, 0].into_iter().enumerate() {
        page.store(index, word, Ordering::Relaxed);
    }
    page
}

/// A page of some other layout, with the layout in the top byte of word 0.
fn page_of_layout(layout: u64) -> Heap {
    let page = Heap::new(1 + 2 * BUFFERS as usize);
    page.store(0, layout << 56, Ordering::Relaxed);
    page
}

/// The error an attach gave, which must have been a refusal.
fn refused<T>(attached: Result<T, RingError>) -> RingError {
    match attached {
        Err(error) => error,
        Ok(_) => panic!("a layout this build does not read was accepted"),
    }
}

fn assert_layout_error(error: &RingError, found: u64, text: &str) {
    match error {
        RingError::Layout {
            what: "this marks page",
            found: got,
            reads: LAYOUT,
        } if *got == found => {}
        other => panic!("not the layout error: {other:?}"),
    }
    assert_eq!(error.to_string(), text);
}

/// The publisher's role: a layout 1 reader's page is refused, and says both values.
#[test]
fn the_publisher_refuses_a_layout_one_page_naming_both_layouts() {
    let mut table: HoldTable<Heap> = HoldTable::new(BUFFERS);
    let error = refused(table.add_over(0, layout_one_page(), Limits::default()));
    assert_layout_error(
        &error,
        1,
        "this marks page is layout 1; this build reads layout 2",
    );
    assert_eq!(table.readers(), 0, "the page was not taken");
}

/// The reader's role: a layout 1 page is refused when it is opened, before any mark is written.
#[test]
fn the_reader_refuses_a_layout_one_page_naming_both_layouts() {
    let page = layout_one_page();
    let error = refused(Marks::over(&page, BUFFERS, 0, 0));
    assert_layout_error(
        &error,
        1,
        "this marks page is layout 1; this build reads layout 2",
    );
    assert_eq!(
        page.load(0, Ordering::Relaxed),
        2,
        "a refused page is left as it was"
    );
}

/// A page of a layout this build does not read, from a newer build, is refused the same way.
#[test]
fn a_newer_marks_page_is_refused_naming_both_layouts() {
    let mut table: HoldTable<Heap> = HoldTable::new(BUFFERS);
    let error = refused(table.add_over(0, page_of_layout(3), Limits::default()));
    assert_layout_error(
        &error,
        3,
        "this marks page is layout 3; this build reads layout 2",
    );
    let error = refused(Marks::over(page_of_layout(3), BUFFERS, 0, 0));
    assert_layout_error(
        &error,
        3,
        "this marks page is layout 3; this build reads layout 2",
    );
}

/// A layout 2 page made by the reader is taken by the publisher, and what the reader marks and
/// takes is what the publisher sees.
#[cfg(target_os = "linux")]
#[test]
fn a_layout_two_page_round_trips_between_reader_and_publisher() {
    let marks = Marks::create(BUFFERS, 0, 0).expect("marks");
    assert!(marks.mark(BufferIndex(0)));
    marks.taken(BufferIndex(1), 5);

    let mut table: HoldTable = HoldTable::new(BUFFERS);
    table
        .add(0, marks.page_fd().expect("page fd"), Limits::default())
        .expect("the publisher takes a layout 2 page");
    assert!(table.held(BufferIndex(0)));
    assert!(!table.held(BufferIndex(1)));
    // The stamp is one more than the last frame taken: frame 5 was taken, frame 6 was not.
    assert!(table.taken(BufferIndex(1), 5));
    assert!(!table.taken(BufferIndex(1), 6));
}
