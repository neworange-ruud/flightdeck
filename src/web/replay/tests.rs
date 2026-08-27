//! Tests for [`super::ReplayBuffer`]: empty/partial/full/overflow writes,
//! wraparound, monotonic offsets, and all three [`super::Resume`] outcomes
//! including the documented edge cases (future offset, zero/one capacity).

use super::*;

#[test]
fn empty_buffer() {
    let buf = ReplayBuffer::new(16);
    assert_eq!(buf.len(), 0);
    assert!(buf.is_empty());
    assert_eq!(buf.capacity(), 16);
    assert_eq!(buf.current_offset(), 0);
    assert_eq!(buf.oldest_offset(), 0);

    let s = buf.attach();
    assert!(s.chunk.is_empty());
    assert_eq!(s.chunk.to_vec(), Vec::<u8>::new());
    assert_eq!(s.offset, 0);

    // Nothing has ever been written, so offset 0 is already current.
    assert_eq!(buf.resume(0), Resume::UpToDate);
}

#[test]
fn partial_write_below_capacity() {
    let mut buf = ReplayBuffer::new(16);
    buf.write(b"hello");
    assert_eq!(buf.len(), 5);
    assert_eq!(buf.current_offset(), 5);
    assert_eq!(buf.oldest_offset(), 0);
    assert_eq!(buf.attach().chunk.to_vec(), b"hello".to_vec());
}

#[test]
fn exactly_full() {
    let mut buf = ReplayBuffer::new(5);
    buf.write(b"hello");
    assert_eq!(buf.len(), 5);
    assert_eq!(buf.current_offset(), 5);
    assert_eq!(buf.oldest_offset(), 0);
    assert_eq!(buf.attach().chunk.to_vec(), b"hello".to_vec());
}

#[test]
fn one_byte_past_full_discards_oldest() {
    let mut buf = ReplayBuffer::new(5);
    buf.write(b"hello");
    buf.write(b"!");
    assert_eq!(buf.len(), 5);
    assert_eq!(buf.current_offset(), 6);
    assert_eq!(buf.oldest_offset(), 1);
    assert_eq!(buf.attach().chunk.to_vec(), b"ello!".to_vec());
}

#[test]
fn write_far_larger_than_capacity_keeps_newest_and_does_not_panic() {
    let mut buf = ReplayBuffer::new(4);
    let big: Vec<u8> = (0..100u8).collect();
    buf.write(&big);
    assert_eq!(buf.len(), 4);
    assert_eq!(buf.current_offset(), 100);
    assert_eq!(buf.oldest_offset(), 96);
    assert_eq!(buf.attach().chunk.to_vec(), vec![96, 97, 98, 99]);
}

#[test]
fn wrapped_read_spans_the_seam() {
    // Single-byte writes into a small ring: over many iterations the
    // underlying VecDeque's backing storage wraps, so `as_slices()` (and
    // therefore `ReplayChunk`) reports a genuinely two-piece read at least
    // once. Correctness of the reconstructed window is checked on every
    // iteration regardless of whether that particular read happened to be
    // split.
    let mut buf = ReplayBuffer::new(8);
    let mut saw_wrap = false;

    for i in 0..64u8 {
        buf.write(&[i]);

        let chunk = buf.attach().chunk;
        if !chunk.first.is_empty() && !chunk.second.is_empty() {
            saw_wrap = true;
        }

        let start = buf.oldest_offset();
        let end = buf.current_offset();
        let expected: Vec<u8> = (start..end).map(|x| x as u8).collect();
        assert_eq!(chunk.to_vec(), expected, "mismatch after writing byte {i}");
    }

    assert!(
        saw_wrap,
        "expected at least one wrapped (two-slice) read over 64 single-byte \
         writes into an 8-byte ring"
    );
}

#[test]
fn monotonic_offset_survives_many_wraps() {
    let mut buf = ReplayBuffer::new(3);
    let mut total: u64 = 0;

    // Mix of writes smaller than, equal to, and (twice) larger than
    // capacity, to exercise both the drain path and the full-replace path
    // repeatedly while checking the offset invariant every time.
    for &chunk_len in &[1usize, 2, 3, 4, 5, 2, 7, 1, 9] {
        let bytes = vec![0u8; chunk_len];
        buf.write(&bytes);
        total += chunk_len as u64;

        assert_eq!(buf.current_offset(), total);
        assert_eq!(buf.oldest_offset(), total.saturating_sub(buf.len() as u64));
        assert!(buf.len() <= buf.capacity());
    }
}

#[test]
fn resume_at_oldest_offset_is_not_truncated() {
    let mut buf = ReplayBuffer::new(4);
    buf.write(b"abcdef"); // retains "cdef"; oldest = 2, current = 6

    match buf.resume(buf.oldest_offset()) {
        Resume::Tail(slice) => {
            assert_eq!(slice.chunk.to_vec(), b"cdef".to_vec());
            assert_eq!(slice.offset, 6);
        }
        other => panic!("expected Tail at the exact oldest offset, got {other:?}"),
    }
}

#[test]
fn resume_one_byte_before_oldest_is_truncated() {
    let mut buf = ReplayBuffer::new(4);
    buf.write(b"abcdef"); // retains "cdef"; oldest = 2, current = 6
    let one_before = buf.oldest_offset() - 1;

    match buf.resume(one_before) {
        Resume::Truncated(slice) => {
            assert_eq!(slice.chunk.to_vec(), b"cdef".to_vec());
            assert_eq!(slice.offset, 6);
        }
        other => panic!("expected Truncated one byte before the oldest offset, got {other:?}"),
    }
}

#[test]
fn resume_at_current_offset_is_up_to_date() {
    let mut buf = ReplayBuffer::new(4);
    buf.write(b"abcd");
    assert_eq!(buf.resume(buf.current_offset()), Resume::UpToDate);
}

#[test]
fn resume_from_future_offset_is_truncated() {
    let mut buf = ReplayBuffer::new(4);
    buf.write(b"abcd");

    match buf.resume(buf.current_offset() + 1_000) {
        Resume::Truncated(slice) => {
            assert_eq!(slice.chunk.to_vec(), b"abcd".to_vec());
            assert_eq!(slice.offset, 4);
        }
        other => panic!("expected Truncated for a future offset, got {other:?}"),
    }
}

#[test]
fn resume_somewhere_in_the_middle_of_the_retained_window() {
    let mut buf = ReplayBuffer::new(10);
    buf.write(b"abcdefghij"); // offsets 0..10, all retained

    match buf.resume(4) {
        Resume::Tail(slice) => {
            assert_eq!(slice.chunk.to_vec(), b"efghij".to_vec());
            assert_eq!(slice.offset, 10);
        }
        other => panic!("expected Tail, got {other:?}"),
    }
}

#[test]
fn zero_length_write_is_a_no_op() {
    let mut buf = ReplayBuffer::new(4);
    buf.write(b"ab");
    let offset_before = buf.current_offset();
    let len_before = buf.len();

    buf.write(&[]);

    assert_eq!(buf.current_offset(), offset_before);
    assert_eq!(buf.len(), len_before);
    assert_eq!(buf.attach().chunk.to_vec(), b"ab".to_vec());
}

#[test]
fn zero_capacity_discards_everything_but_still_tracks_offset() {
    let mut buf = ReplayBuffer::new(0);
    buf.write(b"hello");

    assert_eq!(buf.len(), 0);
    assert!(buf.is_empty());
    assert_eq!(buf.current_offset(), 5);
    assert_eq!(buf.oldest_offset(), 5);
    assert!(buf.attach().chunk.is_empty());

    // Nothing is retained, so any offset other than "current" is truncated.
    match buf.resume(0) {
        Resume::Truncated(slice) => assert!(slice.chunk.is_empty()),
        other => panic!("expected Truncated, got {other:?}"),
    }
    assert_eq!(buf.resume(5), Resume::UpToDate);
}

#[test]
fn one_byte_capacity_keeps_only_the_latest_byte() {
    let mut buf = ReplayBuffer::new(1);
    buf.write(b"ab");

    assert_eq!(buf.attach().chunk.to_vec(), b"b".to_vec());
    assert_eq!(buf.current_offset(), 2);
    assert_eq!(buf.oldest_offset(), 1);

    match buf.resume(1) {
        Resume::Tail(slice) => assert_eq!(slice.chunk.to_vec(), b"b".to_vec()),
        other => panic!("expected Tail at the boundary offset, got {other:?}"),
    }
}
