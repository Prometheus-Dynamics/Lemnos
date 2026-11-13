//! Parameter jobs (the IEP2's) on a claim, against the played kernel's `iep2_alloc_task`: the
//! structure copied whole, its descriptor words translated from `iep2_addr_rnum` with the
//! session's `REG_NO_OFFSET`, the results copied out at the poll.

use super::*;
use crate::played::{IEP2_ADDR_RNUM, IEP2_OUTPUT, IEP2_PARAMS};
use crate::{ParamAddress, ParamJob};

/// A structure the IEP2 takes: 160 by 40 (10 by 10 tiles), mode `I5O2`, with junk in every
/// descriptor word that a caller might try to send.
fn structure() -> [u8; IEP2_PARAMS] {
    let mut p = [0u8; IEP2_PARAMS];
    let mut set = |at: usize, value: u32| p[at..at + 4].copy_from_slice(&value.to_ne_bytes());
    set(16, 10); // tile_cols
    set(20, 10); // tile_rows
    set(24, 40); // strides, in words
    set(28, 40);
    set(32, 40);
    set(104, 1); // dil_mode
    set(116, 0x1234_5678); // md_theta: the caller's, carried as it is
    // A caller's descriptors, raw: never sent.
    for (i, register) in IEP2_ADDR_RNUM.iter().enumerate() {
        if *register != -1 {
            set(36 + 4 * i, 7);
        }
    }
    p
}

fn set(p: &mut [u8], at: usize, value: u32) {
    p[at..at + 4].copy_from_slice(&value.to_ne_bytes());
}

fn word(p: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(p[at..at + 4].try_into().unwrap())
}

/// The descriptor words, by byte offset, as the kernel's table has them.
fn descriptor_words() -> Vec<(u16, u16)> {
    IEP2_ADDR_RNUM
        .iter()
        .enumerate()
        .filter(|(_, r)| **r != -1)
        .map(|(i, r)| ((36 + 4 * i) as u16, *r as u16))
        .collect()
}

/// An address for every descriptor word: the frames in `frames`, the scratch in `scratch`, at
/// offsets that step by 4096.
fn addresses(frames: BufferId, scratch: BufferId) -> Vec<ParamAddress> {
    descriptor_words()
        .into_iter()
        .enumerate()
        .map(|(i, (word, _))| ParamAddress {
            word,
            buffer: if word >= 96 { scratch } else { frames },
            offset: if word >= 96 { 0 } else { 4096 * i as u32 },
        })
        .collect()
}

#[test]
fn the_iep2_facts_are_the_kernels_table() {
    let facts = Block::Iep2.params().unwrap();
    assert_eq!(facts.bytes, IEP2_PARAMS);
    assert_eq!(facts.results, IEP2_OUTPUT);
    let ours: Vec<_> = facts
        .descriptors
        .iter()
        .map(|d| (d.at, d.register))
        .collect();
    assert_eq!(ours, descriptor_words());
    assert!(facts.bytes <= 4 * MAX_REGISTERS && facts.results <= READ_BACK_BYTES);
    assert!(facts.descriptors.len() <= MAX_ADDRESSES);
    // No checked word is a descriptor word, and every one is inside the structure.
    for w in facts.words {
        assert!(usize::from(w.at) + 4 <= facts.bytes);
        assert!(facts.descriptors.iter().all(|d| d.at != w.at));
    }
}

#[test]
fn a_parameter_job_reaches_the_block_with_every_descriptor_word_a_held_buffer() {
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Iep2, 0).unwrap();
    assert_eq!(kernel.borrow().client, Some(client::IEP2));
    let (frames_file, scratch_file) = (memfd(1 << 20), memfd(65536));
    let frames_fd = frames_file.as_raw_fd() as u32;
    let scratch_fd = scratch_file.as_raw_fd() as u32;
    let frames = claim.add_buffer(frames_file).unwrap();
    let scratch = claim.add_buffer(scratch_file).unwrap();
    for (i, byte) in kernel.borrow_mut().output.iter_mut().enumerate() {
        *byte = i as u8;
    }

    let params = structure();
    let addresses = addresses(frames, scratch);
    let job = ParamJob {
        identity: 9,
        params: &params,
        addresses: &addresses,
    };
    claim.submit_params(&job).unwrap();
    {
        let k = kernel.borrow();
        // One call: the structure, the offsets, the results; each saying no offset rides in a
        // descriptor word.
        assert_eq!(k.last_len, 3);
        assert_eq!(
            k.last[..3].iter().map(|(cmd, _)| *cmd).collect::<Vec<_>>(),
            [
                Command::SetRegWrite.code(),
                Command::SetRegAddrOffset.code(),
                Command::SetRegRead.code()
            ]
        );
        assert!(
            k.last[..3]
                .iter()
                .all(|(_, f)| f & flags::REG_NO_OFFSET != 0)
        );
        // Only the claim's own descriptors were translated: never the caller's 7.
        let taken = &k.translated[..k.translated_len];
        assert_eq!(taken.len(), 15);
        assert!(taken.iter().all(|fd| *fd == frames_fd || *fd == scratch_fd));
        let task = &k.tasks[k.head];
        let mut bytes = [0u8; IEP2_PARAMS];
        for (b, r) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(&task.regs) {
            *b = r.to_ne_bytes();
        }
        for a in &addresses {
            let fd = if a.buffer == frames {
                frames_fd
            } else {
                scratch_fd
            };
            assert_eq!(word(&bytes, a.word.into()), iova(fd) + a.offset, "{a:?}");
        }
        // Every other word is the caller's, as it was.
        assert_eq!(word(&bytes, 116), 0x1234_5678);
        assert_eq!(word(&bytes, 16), 10);
        assert_eq!(word(&bytes, 80), 0);
    }
    let mut results = [0u32; IEP2_OUTPUT / 4];
    let done = claim.complete(&mut results).unwrap();
    assert_eq!(
        done,
        Completion {
            identity: 9,
            registers: IEP2_OUTPUT / 4
        }
    );
    assert_eq!(results[0], u32::from_ne_bytes([0, 1, 2, 3]));
    assert_eq!(results[102], u32::from_ne_bytes([152, 153, 154, 155]));
}

#[test]
fn every_descriptor_word_holds_a_buffer_the_claim_holds_named_once() {
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Iep2, 0).unwrap();
    let frames = claim.add_buffer(memfd(1 << 20)).unwrap();
    let scratch = claim.add_buffer(memfd(65536)).unwrap();
    let gone = claim.add_buffer(memfd(4096)).unwrap();
    claim.remove_buffer(gone).unwrap();
    let params = structure();
    let calls = kernel.borrow().calls;
    let refused = |claim: &mut crate::Claim<'_, PlayedDevice>, addresses: &[ParamAddress]| {
        let job = ParamJob {
            identity: 0,
            params: &params,
            addresses,
        };
        match claim.submit_params(&job) {
            Err(SubmitError::Job(error)) => error,
            other => panic!("taken: {other:?}"),
        }
    };

    // One word left out: its caller's 7 would otherwise stay, or a 0 give the block address 0.
    let mut some = addresses(frames, scratch);
    let missing = some.remove(4).word;
    assert_eq!(
        refused(&mut claim, &some),
        JobError::MissingDescriptor { word: missing }
    );
    // A word the kernel doesn't translate: the outputs' third plane, a size.
    for word in [80, 92, 16, 0, 104, 652, 656] {
        let mut all = addresses(frames, scratch);
        all[0].word = word;
        assert_eq!(
            refused(&mut claim, &all),
            JobError::NotDescriptor { word },
            "{word}"
        );
    }
    // Named twice.
    let mut twice = addresses(frames, scratch);
    twice[1].word = twice[0].word;
    assert_eq!(
        refused(&mut claim, &twice),
        JobError::DuplicateAddress { register: 24 }
    );
    // A buffer the claim gave back.
    let mut stale = addresses(frames, scratch);
    stale[3].buffer = gone;
    assert_eq!(
        refused(&mut claim, &stale),
        JobError::UnknownBuffer { register: 25 }
    );
    // An offset at the end of its buffer.
    let mut past = addresses(frames, scratch);
    past[14].offset = 65536;
    assert_eq!(
        refused(&mut claim, &past),
        JobError::OffsetOutside {
            register: 33,
            offset: 65536,
            size: 65536
        }
    );
    // More addresses than words.
    let mut more = addresses(frames, scratch);
    more.push(more[0]);
    assert_eq!(refused(&mut claim, &more), JobError::TooManyAddresses);
    // None of it reached the kernel.
    assert_eq!(kernel.borrow().calls, calls);
    assert_eq!(kernel.borrow().queued, 0);
}

#[test]
fn the_callers_indices_sizes_counts_and_modes_are_in_range() {
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Iep2, 0).unwrap();
    let frames = claim.add_buffer(memfd(1 << 20)).unwrap();
    let scratch = claim.add_buffer(memfd(65536)).unwrap();
    let addresses = addresses(frames, scratch);
    let mut run = |at: usize, value: u32| {
        let mut params = structure();
        set(&mut params, at, value);
        let job = ParamJob {
            identity: 0,
            params: &params,
            addresses: &addresses,
        };
        let result = claim.submit_params(&job);
        if result.is_ok() {
            claim.complete(&mut [0; IEP2_OUTPUT / 4]).unwrap();
        }
        result
    };
    // (word, refused values, taken values)
    let cases: [(usize, &[u32], &[u32]); 11] = [
        (16, &[0, 121, u32::MAX], &[1, 120]), // tile_cols
        (20, &[0, 273], &[1, 272]),           // tile_rows
        (24, &[0x1_0000], &[0, 0xffff]),      // strides
        (28, &[0x1_0000], &[0xffff]),
        (32, &[0x1_0000], &[0xffff]),
        (80, &[1, 7], &[0]), // the outputs' third plane
        (92, &[1], &[0]),
        (104, &[5, 10, u32::MAX], &[0, 4, 6, 9]), // dil_mode, never I1O1T
        (108, &[1], &[0]),                        // dil_out_mode
        (132, &[9, u32::MAX], &[0, 8]),           // osd_area_num
        (488, &[1], &[0]),                        // roi_en
    ];
    for (at, refused, taken) in cases {
        for &value in refused {
            match run(at, value) {
                Err(SubmitError::Job(JobError::Word { at: w, value: v }))
                    if usize::from(w) == at && v == value => {}
                other => panic!("word {at} = {value}: {other:?}"),
            }
        }
        for &value in taken {
            assert!(run(at, value).is_ok(), "word {at} = {value}");
        }
    }
    // The caller's other words are carried, whatever they hold.
    for at in [116, 168, 324, 416, 484, 492, 624, 652] {
        assert!(run(at, u32::MAX).is_ok(), "word {at}");
    }
    assert_eq!(kernel.borrow().queued, 0);
}

#[test]
fn a_parameter_job_is_exactly_the_structure_and_only_on_a_parameter_block() {
    let (mut service, _kernel) = service();
    let mut claim = service.claim(Block::Iep2, 0).unwrap();
    let frames = claim.add_buffer(memfd(1 << 20)).unwrap();
    let scratch = claim.add_buffer(memfd(65536)).unwrap();
    let addresses = addresses(frames, scratch);
    let long = [0u8; IEP2_PARAMS + 4];
    for len in [0, IEP2_PARAMS - 4, IEP2_PARAMS + 4] {
        let job = ParamJob {
            identity: 0,
            params: &long[..len],
            addresses: &addresses,
        };
        assert!(
            matches!(
                claim.submit_params(&job),
                Err(SubmitError::Job(JobError::ParamsSize { len: l })) if l == len
            ),
            "{len}"
        );
    }
    // A register-file job on the parameter block runs past its registers.
    let regs = regs();
    let job = Job {
        identity: 0,
        first: 0,
        registers: &regs,
        more: &[],
        addresses: &[],
        read: None,
    };
    assert!(matches!(
        claim.submit(&job),
        Err(SubmitError::Job(JobError::WriteOutside))
    ));
    drop(claim);
    // A parameter job on a register-file block.
    let mut claim = service.claim(Block::Vepu2, 0).unwrap();
    let params = structure();
    let job = ParamJob {
        identity: 0,
        params: &params,
        addresses: &[],
    };
    assert!(matches!(
        claim.submit_params(&job),
        Err(SubmitError::Job(JobError::NotParamBlock))
    ));
}

#[test]
fn steady_state_parameter_jobs_allocate_nothing() {
    let (mut service, _kernel) = service();
    let mut claim = service.claim(Block::Iep2, 0).unwrap();
    let frames = claim.add_buffer(memfd(1 << 20)).unwrap();
    let scratch = claim.add_buffer(memfd(65536)).unwrap();
    let addresses = addresses(frames, scratch);
    let params = structure();
    let job = ParamJob {
        identity: 1,
        params: &params,
        addresses: &addresses,
    };
    let mut results = [0u32; IEP2_OUTPUT / 4];
    let ((), n) = allocations(|| {
        for _ in 0..50 {
            claim.submit_params(&job).unwrap();
            claim.submit_params(&job).unwrap();
            claim.complete(&mut results).unwrap();
            claim.complete(&mut results).unwrap();
        }
    });
    assert_eq!(n, 0);
}
