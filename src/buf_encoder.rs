// SPDX-License-Identifier: CC0-1.0

//! Implements a buffered encoder.
//!
//! This is a low-level module, most uses should be satisfied by the `display` module instead.
//!
//! The main type in this module is [`BufEncoder`] which provides buffered hex encoding.
//! `BufEncoder` is faster than the usual `write!(f, "{02x}", b)?` in a for loop because it reduces
//! dynamic dispatch and decreases the number of allocations if a `String` is being created.

use core::borrow::Borrow;
use std::mem::MaybeUninit;

use super::{Case, Table};

/// Hex-encodes bytes into the provided buffer.
///
/// This is an important building block for fast hex-encoding. Because string writing tools
/// provided by `core::fmt` involve dynamic dispatch and don't allow reserving capacity in strings
/// buffering the hex and then formatting it is significantly faster.
pub struct BufEncoder<const CAP: usize> {
    buf: [MaybeUninit<u8>; CAP],
    ptr: usize,
    table: &'static Table,
}

impl<const CAP: usize> BufEncoder<CAP> {
    const _CHECK_EVEN_CAPACITY: () = [(); 1][CAP % 2];

    /// Creates an empty `BufEncoder` that will encode bytes to hex characters in the given case.
    #[inline]
    pub fn new(case: Case) -> Self {
        BufEncoder { buf: [MaybeUninit::uninit(); CAP], ptr: 0, table: case.table() }
    }

    /// Encodes `byte` as hex and appends it to the buffer.
    ///
    /// ## Panics
    ///
    /// The method panics if the buffer is full.
    #[inline]
    #[track_caller]
    pub fn put_byte(&mut self, byte: u8) {
        assert!(self.ptr + 2 <= CAP);
        let ascii_bytes = self.table.byte_to_array(byte);
        unsafe {
            core::ptr::copy_nonoverlapping(
                ascii_bytes.as_ptr(),
                self.buf.as_ptr().add(self.ptr) as *mut u8,
                2,
            );
        }
        self.ptr += 2;
    }

    /// Encodes `bytes` as hex and appends them to the buffer.
    ///
    /// ## Panics
    ///
    /// The method panics if the bytes wouldn't fit the buffer.
    #[inline]
    #[track_caller]
    pub fn put_bytes<I>(&mut self, bytes: I)
    where
        I: IntoIterator,
        I::Item: Borrow<u8>,
    {
        self.put_bytes_inner(bytes.into_iter())
    }

    #[inline]
    #[track_caller]
    fn put_bytes_inner<I>(&mut self, bytes: I)
    where
        I: Iterator,
        I::Item: Borrow<u8>,
    {
        // May give the compiler better optimization opportunity
        if let Some(max) = bytes.size_hint().1 {
            assert!(max <= self.space_remaining());
        }
        for byte in bytes {
            self.put_byte(*byte.borrow());
        }
    }

    /// Encodes as many `bytes` as fit into the buffer as hex and return the remainder.
    ///
    /// This method works just like `put_bytes` but instead of panicking it returns the unwritten
    /// bytes. The method returns an empty slice if all bytes were written
    #[must_use = "this may write only part of the input buffer"]
    #[inline]
    #[track_caller]
    pub fn put_bytes_min<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        let to_write = self.space_remaining().min(bytes.len());
        self.put_bytes(&bytes[..to_write]);
        &bytes[to_write..]
    }

    /// Returns true if no more bytes can be written into the buffer.
    #[inline]
    pub fn is_full(&self) -> bool { self.space_remaining() == 0 }

    /// Returns the written bytes as a hex `str`.
    #[inline]
    pub fn as_str(&self) -> &str {
        unsafe {
            let s = core::slice::from_raw_parts(self.buf.as_ptr() as *const u8, self.ptr);
            core::str::from_utf8_unchecked(s)
        }
    }

    /// Resets the buffer to become empty.
    #[inline]
    pub fn clear(&mut self) { self.ptr = 0; }

    /// How many bytes can be written to this buffer.
    ///
    /// Note that this returns the number of bytes before encoding, not number of hex digits.
    #[inline]
    pub fn space_remaining(&self) -> usize { (CAP - self.ptr) / 2 }

    pub(crate) fn put_filler(&mut self, filler: char, max_count: usize) -> usize {
        let mut buf = [0; 4];
        let filler = filler.encode_utf8(&mut buf);
        let max_capacity = (CAP - self.ptr) / filler.len();
        let to_write = max_capacity.min(max_count);

        assert!(self.ptr + filler.len() <= CAP);

        for _ in 0..to_write {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    filler.as_bytes().as_ptr(),
                    self.buf.as_ptr().add(self.ptr) as *mut u8,
                    filler.len(),
                );
            }
            self.ptr += filler.len();
        }

        to_write
    }
}

impl<const CAP: usize> Default for BufEncoder<CAP> {
    fn default() -> Self { Self::new(Case::Lower) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let encoder = BufEncoder::<2>::new(Case::Lower);
        assert_eq!(encoder.as_str(), "");
        assert!(!encoder.is_full());

        let encoder = BufEncoder::<2>::new(Case::Upper);
        assert_eq!(encoder.as_str(), "");
        assert!(!encoder.is_full());
    }

    #[test]
    fn single_byte_exact_buf() {
        let mut encoder = BufEncoder::<2>::new(Case::Lower);
        assert_eq!(encoder.space_remaining(), 1);
        encoder.put_byte(42);
        assert_eq!(encoder.as_str(), "2a");
        assert_eq!(encoder.space_remaining(), 0);
        assert!(encoder.is_full());
        encoder.clear();
        assert_eq!(encoder.space_remaining(), 1);
        assert!(!encoder.is_full());

        let mut encoder = BufEncoder::<2>::new(Case::Upper);
        assert_eq!(encoder.space_remaining(), 1);
        encoder.put_byte(42);
        assert_eq!(encoder.as_str(), "2A");
        assert_eq!(encoder.space_remaining(), 0);
        assert!(encoder.is_full());
        encoder.clear();
        assert_eq!(encoder.space_remaining(), 1);
        assert!(!encoder.is_full());
    }

    #[test]
    fn single_byte_oversized_buf() {
        let mut encoder = BufEncoder::<4>::new(Case::Lower);
        assert_eq!(encoder.space_remaining(), 2);
        encoder.put_byte(42);
        assert_eq!(encoder.space_remaining(), 1);
        assert_eq!(encoder.as_str(), "2a");
        assert!(!encoder.is_full());
        encoder.clear();
        assert_eq!(encoder.space_remaining(), 2);
        assert!(!encoder.is_full());

        let mut encoder = BufEncoder::<4>::new(Case::Upper);
        assert_eq!(encoder.space_remaining(), 2);
        encoder.put_byte(42);
        assert_eq!(encoder.space_remaining(), 1);
        assert_eq!(encoder.as_str(), "2A");
        assert!(!encoder.is_full());
        encoder.clear();
        assert_eq!(encoder.space_remaining(), 2);
        assert!(!encoder.is_full());
    }

    #[test]
    fn two_bytes() {
        let mut encoder = BufEncoder::<4>::new(Case::Lower);
        assert_eq!(encoder.space_remaining(), 2);
        encoder.put_byte(42);
        assert_eq!(encoder.space_remaining(), 1);
        encoder.put_byte(255);
        assert_eq!(encoder.space_remaining(), 0);
        assert_eq!(encoder.as_str(), "2aff");
        assert!(encoder.is_full());
        encoder.clear();
        assert_eq!(encoder.space_remaining(), 2);
        assert!(!encoder.is_full());

        let mut encoder = BufEncoder::<4>::new(Case::Upper);
        assert_eq!(encoder.space_remaining(), 2);
        encoder.put_byte(42);
        assert_eq!(encoder.space_remaining(), 1);
        encoder.put_byte(255);
        assert_eq!(encoder.space_remaining(), 0);
        assert_eq!(encoder.as_str(), "2AFF");
        assert!(encoder.is_full());
        encoder.clear();
        assert_eq!(encoder.space_remaining(), 2);
        assert!(!encoder.is_full());
    }

    #[test]
    fn put_bytes_min() {
        let mut encoder = BufEncoder::<2>::new(Case::Lower);
        let remainder = encoder.put_bytes_min(b"");
        assert_eq!(remainder, b"");
        assert_eq!(encoder.as_str(), "");
        let remainder = encoder.put_bytes_min(b"*");
        assert_eq!(remainder, b"");
        assert_eq!(encoder.as_str(), "2a");
        encoder.clear();
        let remainder = encoder.put_bytes_min(&[42, 255]);
        assert_eq!(remainder, &[255]);
        assert_eq!(encoder.as_str(), "2a");
    }

    #[test]
    fn same_as_fmt() {
        use core::fmt::{self, Write};

        struct Writer {
            buf: [u8; 2],
            pos: usize,
        }

        impl Writer {
            fn as_str(&self) -> &str { core::str::from_utf8(&self.buf[..self.pos]).unwrap() }
        }

        impl Write for Writer {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                assert!(self.pos <= 2);
                if s.len() > 2 - self.pos {
                    Err(fmt::Error)
                } else {
                    self.buf[self.pos..(self.pos + s.len())].copy_from_slice(s.as_bytes());
                    self.pos += s.len();
                    Ok(())
                }
            }
        }

        let mut writer = Writer { buf: [0u8; 2], pos: 0 };

        let mut encoder = BufEncoder::<2>::new(Case::Lower);
        for i in 0..=255 {
            write!(writer, "{:02x}", i).unwrap();
            encoder.put_byte(i);
            assert_eq!(encoder.as_str(), writer.as_str());
            writer.pos = 0;
            encoder.clear();
        }

        let mut encoder = BufEncoder::<2>::new(Case::Upper);
        for i in 0..=255 {
            write!(writer, "{:02X}", i).unwrap();
            encoder.put_byte(i);
            assert_eq!(encoder.as_str(), writer.as_str());
            writer.pos = 0;
            encoder.clear();
        }
    }
}
