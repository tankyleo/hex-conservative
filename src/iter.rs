// SPDX-License-Identifier: CC0-1.0

//! Iterator that converts hex to bytes.

use core::borrow::Borrow;
use core::convert::TryInto;
use core::iter::FusedIterator;
use core::str;
#[cfg(feature = "std")]
use std::io;

#[cfg(all(feature = "alloc", not(feature = "std")))]
use crate::alloc::vec::Vec;
use crate::error::{InvalidChar, InvalidCharError, OddLengthStringError};
use crate::{Case, Table};

/// Convenience alias for `HexToBytesIter<HexDigitsIter<'a>>`.
pub type HexSliceToBytesIter<'a> = HexToBytesIter<HexDigitsIter<'a>>;

/// Iterator yielding bytes decoded from an iterator of pairs of hex digits.
pub struct HexToBytesIter<T: Iterator<Item = [u8; 2]>> {
    iter: T,
    original_len: usize,
}

impl<'a> HexToBytesIter<HexDigitsIter<'a>> {
    /// Constructs a new `HexToBytesIter` from a string slice.
    ///
    /// # Errors
    ///
    /// If the input string is of odd length.
    #[inline]
    pub fn new(s: &'a str) -> Result<Self, OddLengthStringError> {
        if s.len() % 2 != 0 {
            Err(OddLengthStringError { len: s.len() })
        } else {
            Ok(Self::new_unchecked(s))
        }
    }

    pub(crate) fn new_unchecked(s: &'a str) -> Self {
        Self::from_pairs(HexDigitsIter::new_unchecked(s.as_bytes()))
    }

    /// Writes all the bytes yielded by this `HexToBytesIter` to the provided slice.
    ///
    /// Stops writing if this `HexToBytesIter` yields an `InvalidCharError`.
    ///
    /// # Panics
    ///
    /// Panics if the length of this `HexToBytesIter` is not equal to the length of the provided
    /// slice.
    pub(crate) fn drain_to_slice(self, buf: &mut [u8]) -> Result<(), InvalidCharError> {
        assert_eq!(self.len(), buf.len());
        let mut ptr = buf.as_mut_ptr();
        for byte in self {
            // SAFETY: for loop iterates `len` times, and `buf` has length `len`
            unsafe {
                core::ptr::write(ptr, byte?);
                ptr = ptr.add(1);
            }
        }
        Ok(())
    }

    /// Writes all the bytes yielded by this `HexToBytesIter` to a `Vec<u8>`.
    ///
    /// This is equivalent to the combinator chain `iter().map().collect()` but was found by
    /// benchmarking to be faster.
    #[cfg(any(test, feature = "std", feature = "alloc"))]
    pub(crate) fn drain_to_vec(self) -> Result<Vec<u8>, InvalidCharError> {
        let len = self.len();
        let mut ret = Vec::with_capacity(len);
        let mut ptr = ret.as_mut_ptr();
        for byte in self {
            // SAFETY: for loop iterates `len` times, and `ret` has a capacity of at least `len`
            unsafe {
                // docs: "`core::ptr::write` is appropriate for initializing uninitialized memory"
                core::ptr::write(ptr, byte?);
                ptr = ptr.add(1);
            }
        }
        // SAFETY: `len` elements have been initialized, and `ret` has a capacity of at least `len`
        unsafe {
            ret.set_len(len);
        }
        Ok(ret)
    }
}

impl<T: Iterator<Item = [u8; 2]> + ExactSizeIterator> HexToBytesIter<T> {
    /// Constructs a custom hex decoding iterator from another iterator.
    #[inline]
    pub fn from_pairs(iter: T) -> Self { Self { original_len: iter.len(), iter } }
}

impl<T: Iterator<Item = [u8; 2]> + ExactSizeIterator> Iterator for HexToBytesIter<T> {
    type Item = Result<u8, InvalidCharError>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let [hi, lo] = self.iter.next()?;
        Some(hex_chars_to_byte(hi, lo).map_err(|(c, is_high)| {
            let pos = if is_high {
                (self.original_len - self.iter.len() - 1) * 2
            } else {
                (self.original_len - self.iter.len() - 1) * 2 + 1
            };

            let utf8_byte_len = match c {
                0x00..=0x7f =>
                    return InvalidCharError { invalid: InvalidChar::Utf8(char::from(c)), pos },
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return InvalidCharError { invalid: InvalidChar::Other(c), pos },
            };

            let mut bytes = arrayvec::ArrayVec::<u8, 5>::new();

            if is_high {
                bytes.push(hi);
                bytes.push(lo);
            } else {
                bytes.push(lo);
            };

            assert!(!bytes[0].is_ascii());
            while bytes.len() < utf8_byte_len {
                let b = match self.iter.next() {
                    Some(b) => b,
                    None => return InvalidCharError { invalid: InvalidChar::Other(c), pos },
                };
                bytes.try_extend_from_slice(&b).expect("unexpected capacity error");
            }

            let s = match core::str::from_utf8(&bytes) {
                Ok(s) => s,
                Err(e) => match e.valid_up_to() {
                    0 => return InvalidCharError { invalid: InvalidChar::Other(c), pos },
                    v @ 1..=4 => core::str::from_utf8(&bytes[..v]).unwrap(),
                    _ => unreachable!(),
                },
            };
            let invalid = s.chars().next().expect("expected at least 1 character");
            InvalidCharError { invalid: InvalidChar::Utf8(invalid), pos }
        }))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) { self.iter.size_hint() }

    #[inline]
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        let [hi, lo] = self.iter.nth(n)?;
        Some(hex_chars_to_byte(hi, lo).map_err(|(c, is_high)| InvalidCharError {
            invalid: InvalidChar::Utf8(char::from(c)),
            pos: if is_high {
                (self.original_len - self.iter.len() - 1) * 2
            } else {
                (self.original_len - self.iter.len() - 1) * 2 + 1
            },
        }))
    }
}

impl<T: Iterator<Item = [u8; 2]> + DoubleEndedIterator + ExactSizeIterator> DoubleEndedIterator
    for HexToBytesIter<T>
{
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        fn is_utf8_continuation(b: u8) -> bool { (0x80..=0xbf).contains(&b) }

        let [hi, lo] = self.iter.next_back()?;
        Some(hex_chars_to_byte(hi, lo).map_err(|(mut c, is_high)| {
            let mut pos = if is_high { self.iter.len() * 2 } else { self.iter.len() * 2 + 1 };
            // put c and pos at the right most position that is wrong
            if (lo as char).to_digit(16).is_none() {
                if is_high {
                    pos += 1;
                }
                c = lo;
            }

            // if the right most position that is wrong is ascii, or other, return immediately
            if c.is_ascii() {
                return InvalidCharError { invalid: InvalidChar::Utf8(char::from(c)), pos };
            } else if !is_utf8_continuation(c) {
                return InvalidCharError { invalid: InvalidChar::Other(c), pos };
            }

            let mut bytes = arrayvec::ArrayVec::<u8, 4>::new();

            // if left is wrong, right is wrong too, otherwise we should have returned above
            assert!(is_high);
            if is_utf8_continuation(lo) {
                assert_eq!(c, lo);
                bytes.push(lo);
                bytes.push(hi);
            } else {
                // otherwise, we should have returned above
                assert!(is_utf8_continuation(hi));
                bytes.push(hi);
            }

            while is_utf8_continuation(bytes[bytes.len() - 1]) {
                let [hi, lo] = match self.iter.next_back() {
                    Some(b) => b,
                    None => return InvalidCharError { invalid: InvalidChar::Other(c), pos },
                };
                if let Err(_e) = bytes.try_push(lo) {
                    return InvalidCharError { invalid: InvalidChar::Other(c), pos };
                }
                if is_utf8_continuation(lo) {
                    if let Err(_e) = bytes.try_push(hi) {
                        return InvalidCharError { invalid: InvalidChar::Other(c), pos };
                    }
                }
            }

            bytes.reverse();

            let s = match core::str::from_utf8(&bytes) {
                Ok(s) => s,
                Err(_e) => {
                    return InvalidCharError { invalid: InvalidChar::Other(c), pos };
                }
            };
            assert!(!bytes[0].is_ascii());
            let invalid = s.chars().next().expect("should yield at least 1 character");
            InvalidCharError { invalid: InvalidChar::Utf8(invalid), pos }
        }))
    }

    #[inline]
    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        let [hi, lo] = self.iter.nth_back(n)?;
        Some(hex_chars_to_byte(hi, lo).map_err(|(c, is_high)| InvalidCharError {
            invalid: InvalidChar::Utf8(char::from(c)),
            pos: if is_high { self.iter.len() * 2 } else { self.iter.len() * 2 + 1 },
        }))
    }
}

impl<T: Iterator<Item = [u8; 2]> + ExactSizeIterator> ExactSizeIterator for HexToBytesIter<T> {}

impl<T: Iterator<Item = [u8; 2]> + ExactSizeIterator + FusedIterator> FusedIterator
    for HexToBytesIter<T>
{
}

#[cfg(feature = "std")]
impl<T: Iterator<Item = [u8; 2]> + ExactSizeIterator + FusedIterator> io::Read
    for HexToBytesIter<T>
{
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read = 0usize;
        for dst in buf {
            match self.next() {
                Some(Ok(src)) => {
                    *dst = src;
                    bytes_read += 1;
                }
                _ => break,
            }
        }
        Ok(bytes_read)
    }
}

/// An internal iterator returning hex digits from a string.
///
/// Generally you shouldn't need to refer to this or bother with it and just use
/// [`HexToBytesIter::new`] consuming the returned value and use `HexSliceToBytesIter` if you need
/// to refer to the iterator in your types.
pub struct HexDigitsIter<'a> {
    // Invariant: the length of the chunks is 2.
    // Technically, this is `iter::Map` but we can't use it because fn is anonymous.
    // We can swap this for actual `ArrayChunks` once it's stable.
    iter: core::slice::ChunksExact<'a, u8>,
}

impl<'a> HexDigitsIter<'a> {
    #[inline]
    fn new_unchecked(digits: &'a [u8]) -> Self { Self { iter: digits.chunks_exact(2) } }
}

impl<'a> Iterator for HexDigitsIter<'a> {
    type Item = [u8; 2];

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|digits| digits.try_into().expect("HexDigitsIter invariant"))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) { self.iter.size_hint() }

    #[inline]
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.iter.nth(n).map(|digits| digits.try_into().expect("HexDigitsIter invariant"))
    }
}

impl<'a> DoubleEndedIterator for HexDigitsIter<'a> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back().map(|digits| digits.try_into().expect("HexDigitsIter invariant"))
    }

    #[inline]
    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        self.iter.nth_back(n).map(|digits| digits.try_into().expect("HexDigitsIter invariant"))
    }
}

impl<'a> ExactSizeIterator for HexDigitsIter<'a> {}

impl<'a> core::iter::FusedIterator for HexDigitsIter<'a> {}

/// `hi` and `lo` are bytes representing hex characters.
///
/// Returns the valid byte or the invalid input byte and a bool indicating error for `hi` or `lo`.
fn hex_chars_to_byte(hi: u8, lo: u8) -> Result<u8, (u8, bool)> {
    let hih = (hi as char).to_digit(16).ok_or((hi, true))?;
    let loh = (lo as char).to_digit(16).ok_or((lo, false))?;

    let ret = (hih << 4) + loh;
    Ok(ret as u8)
}

/// Iterator over bytes which encodes the bytes and yields hex characters.
pub struct BytesToHexIter<I>
where
    I: Iterator,
    I::Item: Borrow<u8>,
{
    /// The iterator whose next byte will be encoded to yield hex characters.
    iter: I,
    /// The low character of the pair (high, low) of hex characters encoded per byte.
    low: Option<char>,
    /// The byte-to-hex conversion table.
    table: &'static Table,
}

impl<I> BytesToHexIter<I>
where
    I: Iterator,
    I::Item: Borrow<u8>,
{
    /// Constructs a `BytesToHexIter` that will yield hex characters in the given case from a byte
    /// iterator.
    pub fn new(iter: I, case: Case) -> BytesToHexIter<I> {
        Self { iter, low: None, table: case.table() }
    }
}

impl<I> Iterator for BytesToHexIter<I>
where
    I: Iterator,
    I::Item: Borrow<u8>,
{
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<char> {
        match self.low {
            Some(c) => {
                self.low = None;
                Some(c)
            }
            None => self.iter.next().map(|b| {
                let [high, low] = self.table.byte_to_chars(*b.borrow());
                self.low = Some(low);
                high
            }),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (min, max) = self.iter.size_hint();
        match self.low {
            Some(_) => (min * 2 + 1, max.map(|max| max * 2 + 1)),
            None => (min * 2, max.map(|max| max * 2)),
        }
    }
}

impl<I> DoubleEndedIterator for BytesToHexIter<I>
where
    I: DoubleEndedIterator,
    I::Item: Borrow<u8>,
{
    #[inline]
    fn next_back(&mut self) -> Option<char> {
        match self.low {
            Some(c) => {
                self.low = None;
                Some(c)
            }
            None => self.iter.next_back().map(|b| {
                let [high, low] = self.table.byte_to_chars(*b.borrow());
                self.low = Some(low);
                high
            }),
        }
    }
}

impl<I> ExactSizeIterator for BytesToHexIter<I>
where
    I: ExactSizeIterator,
    I::Item: Borrow<u8>,
{
    #[inline]
    fn len(&self) -> usize { self.iter.len() * 2 }
}

impl<I> FusedIterator for BytesToHexIter<I>
where
    I: FusedIterator,
    I::Item: Borrow<u8>,
{
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_byte() {
        assert_eq!(Table::LOWER.byte_to_chars(0x00), ['0', '0']);
        assert_eq!(Table::LOWER.byte_to_chars(0x0a), ['0', 'a']);
        assert_eq!(Table::LOWER.byte_to_chars(0xad), ['a', 'd']);
        assert_eq!(Table::LOWER.byte_to_chars(0xff), ['f', 'f']);

        assert_eq!(Table::UPPER.byte_to_chars(0x00), ['0', '0']);
        assert_eq!(Table::UPPER.byte_to_chars(0x0a), ['0', 'A']);
        assert_eq!(Table::UPPER.byte_to_chars(0xad), ['A', 'D']);
        assert_eq!(Table::UPPER.byte_to_chars(0xff), ['F', 'F']);

        let mut buf = [0u8; 2];
        assert_eq!(Table::LOWER.byte_to_str(&mut buf, 0x00), "00");
        assert_eq!(Table::LOWER.byte_to_str(&mut buf, 0x0a), "0a");
        assert_eq!(Table::LOWER.byte_to_str(&mut buf, 0xad), "ad");
        assert_eq!(Table::LOWER.byte_to_str(&mut buf, 0xff), "ff");

        assert_eq!(Table::UPPER.byte_to_str(&mut buf, 0x00), "00");
        assert_eq!(Table::UPPER.byte_to_str(&mut buf, 0x0a), "0A");
        assert_eq!(Table::UPPER.byte_to_str(&mut buf, 0xad), "AD");
        assert_eq!(Table::UPPER.byte_to_str(&mut buf, 0xff), "FF");
    }

    #[test]
    fn decode_iter_forward() {
        let hex = "deadbeef";
        let bytes = [0xde, 0xad, 0xbe, 0xef];

        for (i, b) in HexToBytesIter::new(hex).unwrap().enumerate() {
            assert_eq!(b.unwrap(), bytes[i]);
        }

        let mut iter = HexToBytesIter::new(hex).unwrap();
        for i in (0..=bytes.len()).rev() {
            assert_eq!(iter.len(), i);
            let _ = iter.next();
        }
    }

    #[test]
    fn decode_iter_backward() {
        let hex = "deadbeef";
        let bytes = [0xef, 0xbe, 0xad, 0xde];

        for (i, b) in HexToBytesIter::new(hex).unwrap().rev().enumerate() {
            assert_eq!(b.unwrap(), bytes[i]);
        }

        let mut iter = HexToBytesIter::new(hex).unwrap().rev();
        for i in (0..=bytes.len()).rev() {
            assert_eq!(iter.len(), i);
            let _ = iter.next();
        }
    }

    #[test]
    fn hex_to_digits_size_hint() {
        let hex = "deadbeef";
        let iter = HexDigitsIter::new_unchecked(hex.as_bytes());
        // HexDigitsIter yields two digits at a time `[u8; 2]`.
        assert_eq!(iter.size_hint(), (4, Some(4)));
    }

    #[test]
    fn hex_to_bytes_size_hint() {
        let hex = "deadbeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        assert_eq!(iter.size_hint(), (4, Some(4)));
    }

    #[test]
    fn hex_to_bytes_slice_drain() {
        let hex = "deadbeef";
        let want = [0xde, 0xad, 0xbe, 0xef];
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [0u8; 4];
        iter.drain_to_slice(&mut got).unwrap();
        assert_eq!(got, want);

        let hex = "";
        let want = [];
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [];
        iter.drain_to_slice(&mut got).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    #[should_panic]
    fn hex_to_bytes_slice_drain_panic_empty() {
        let hex = "deadbeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [];
        iter.drain_to_slice(&mut got).unwrap();
    }

    #[test]
    #[should_panic]
    fn hex_to_bytes_slice_drain_panic_too_small() {
        let hex = "deadbeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [0u8; 3];
        iter.drain_to_slice(&mut got).unwrap();
    }

    #[test]
    #[should_panic]
    fn hex_to_bytes_slice_drain_panic_too_big() {
        let hex = "deadbeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [0u8; 5];
        iter.drain_to_slice(&mut got).unwrap();
    }

    #[test]
    fn hex_to_bytes_slice_drain_first_char_error() {
        let hex = "geadbeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [0u8; 4];
        assert_eq!(
            iter.drain_to_slice(&mut got),
            Err(InvalidCharError { invalid: InvalidChar::Utf8('g'), pos: 0 })
        );
    }

    #[test]
    fn hex_to_bytes_slice_drain_middle_char_error() {
        let hex = "deadgeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [0u8; 4];
        assert_eq!(
            iter.drain_to_slice(&mut got),
            Err(InvalidCharError { invalid: InvalidChar::Utf8('g'), pos: 4 })
        );
    }

    #[test]
    fn hex_to_bytes_slice_drain_end_char_error() {
        let hex = "deadbeeg";
        let iter = HexToBytesIter::new_unchecked(hex);
        let mut got = [0u8; 4];
        assert_eq!(
            iter.drain_to_slice(&mut got),
            Err(InvalidCharError { invalid: InvalidChar::Utf8('g'), pos: 7 })
        );
    }

    #[test]
    fn hex_to_bytes_vec_drain() {
        let hex = "deadbeef";
        let want = [0xde, 0xad, 0xbe, 0xef];
        let iter = HexToBytesIter::new_unchecked(hex);
        let got = iter.drain_to_vec().unwrap();
        assert_eq!(got, want);

        let hex = "";
        let iter = HexToBytesIter::new_unchecked(hex);
        let got = iter.drain_to_vec().unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn hex_to_bytes_vec_drain_first_char_error() {
        let hex = "geadbeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        assert_eq!(
            iter.drain_to_vec(),
            Err(InvalidCharError { invalid: InvalidChar::Utf8('g'), pos: 0 })
        );
    }

    #[test]
    fn hex_to_bytes_vec_drain_middle_char_error() {
        let hex = "deadgeef";
        let iter = HexToBytesIter::new_unchecked(hex);
        assert_eq!(
            iter.drain_to_vec(),
            Err(InvalidCharError { invalid: InvalidChar::Utf8('g'), pos: 4 })
        );
    }

    #[test]
    fn hex_to_bytes_vec_drain_end_char_error() {
        let hex = "deadbeeg";
        let iter = HexToBytesIter::new_unchecked(hex);
        assert_eq!(
            iter.drain_to_vec(),
            Err(InvalidCharError { invalid: InvalidChar::Utf8('g'), pos: 7 })
        );
    }

    #[test]
    fn encode_iter() {
        let bytes = [0xde, 0xad, 0xbe, 0xef];
        let lower_want = "deadbeef";
        let upper_want = "DEADBEEF";

        for (i, c) in BytesToHexIter::new(bytes.iter(), Case::Lower).enumerate() {
            assert_eq!(c, lower_want.chars().nth(i).unwrap());
        }
        for (i, c) in BytesToHexIter::new(bytes.iter(), Case::Upper).enumerate() {
            assert_eq!(c, upper_want.chars().nth(i).unwrap());
        }
    }

    #[test]
    fn encode_iter_backwards() {
        let bytes = [0xde, 0xad, 0xbe, 0xef];
        let lower_want = "efbeadde";
        let upper_want = "EFBEADDE";

        for (i, c) in BytesToHexIter::new(bytes.iter(), Case::Lower).rev().enumerate() {
            assert_eq!(c, lower_want.chars().nth(i).unwrap());
        }
        for (i, c) in BytesToHexIter::new(bytes.iter(), Case::Upper).rev().enumerate() {
            assert_eq!(c, upper_want.chars().nth(i).unwrap());
        }
    }

    #[test]
    fn roundtrip_forward() {
        let lower_want = "deadbeefcafebabe";
        let upper_want = "DEADBEEFCAFEBABE";
        let lower_bytes_iter = HexToBytesIter::new(lower_want).unwrap().map(|res| res.unwrap());
        let lower_got = BytesToHexIter::new(lower_bytes_iter, Case::Lower).collect::<String>();
        assert_eq!(lower_got, lower_want);
        let upper_bytes_iter = HexToBytesIter::new(upper_want).unwrap().map(|res| res.unwrap());
        let upper_got = BytesToHexIter::new(upper_bytes_iter, Case::Upper).collect::<String>();
        assert_eq!(upper_got, upper_want);
    }

    #[test]
    fn roundtrip_backward() {
        let lower_want = "deadbeefcafebabe";
        let upper_want = "DEADBEEFCAFEBABE";
        let lower_bytes_iter =
            HexToBytesIter::new(lower_want).unwrap().rev().map(|res| res.unwrap());
        let lower_got =
            BytesToHexIter::new(lower_bytes_iter, Case::Lower).rev().collect::<String>();
        assert_eq!(lower_got, lower_want);
        let upper_bytes_iter =
            HexToBytesIter::new(upper_want).unwrap().rev().map(|res| res.unwrap());
        let upper_got =
            BytesToHexIter::new(upper_bytes_iter, Case::Upper).rev().collect::<String>();
        assert_eq!(upper_got, upper_want);
    }

    #[test]
    fn decode_iter_multi_byte_utf8_error_even_position() {
        use crate::error::InvalidCharError;

        // 2 byte utf8
        let badchar1 = "«23456789abcdef";
        let iter = HexToBytesIter::new(badchar1).unwrap();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 0, invalid: InvalidChar::Utf8('«') }),
            }
        }

        // 3 byte utf8
        let badchar2 = "12☺456789abcde";
        let iter = HexToBytesIter::new(badchar2).unwrap();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 2, invalid: InvalidChar::Utf8('☺') }),
            }
        }

        // 4 byte utf8
        let badchar3 = "123456789abcde🚀";
        let iter = HexToBytesIter::new(badchar3).unwrap();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 14, invalid: InvalidChar::Utf8('🚀') }),
            }
        }
    }

    #[test]
    fn decode_iter_multi_byte_utf8_error_odd_position() {
        use crate::error::InvalidCharError;

        // 2 byte utf8
        let badchar1 = "1«3456789abcdef";
        let iter = HexToBytesIter::new(badchar1).unwrap();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 1, invalid: InvalidChar::Utf8('«') }),
            }
        }

        // 3 byte utf8
        let badchar2 = "123☺56789abcde";
        let iter = HexToBytesIter::new(badchar2).unwrap();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 3, invalid: InvalidChar::Utf8('☺') }),
            }
        }

        // 4 byte utf8
        let badchar3 = "123456789abcd🚀f";
        let iter = HexToBytesIter::new(badchar3).unwrap();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 13, invalid: InvalidChar::Utf8('🚀') }),
            }
        }
    }

    #[test]
    fn decode_iter_rev_multi_byte_utf8_error_even_position() {
        use crate::error::InvalidCharError;

        // 2 byte utf8
        let badchar1 = "«23456789abcdef";
        let iter = HexToBytesIter::new(badchar1).unwrap().rev();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 1, invalid: InvalidChar::Utf8('«') }),
            }
        }

        // 3 byte utf8
        let badchar2 = "12☺456789abcde";
        let iter = HexToBytesIter::new(badchar2).unwrap().rev();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 4, invalid: InvalidChar::Utf8('☺') }),
            }
        }

        // 4 byte utf8
        let badchar3 = "123456789abcde🚀";
        let iter = HexToBytesIter::new(badchar3).unwrap().rev();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 17, invalid: InvalidChar::Utf8('🚀') }),
            }
        }
    }
    #[test]
    fn decode_iter_rev_multi_byte_utf8_error_odd_position() {
        use crate::error::InvalidCharError;

        // 2 byte utf8
        let badchar1 = "1«3456789abcdef";
        let iter = HexToBytesIter::new(badchar1).unwrap().rev();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 2, invalid: InvalidChar::Utf8('«') }),
            }
        }

        // 3 byte utf8
        let badchar2 = "123☺56789abcde";
        let iter = HexToBytesIter::new(badchar2).unwrap().rev();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 5, invalid: InvalidChar::Utf8('☺') }),
            }
        }

        // 4 byte utf8
        let badchar3 = "123456789abcd🚀f";
        let iter = HexToBytesIter::new(badchar3).unwrap().rev();
        for i in iter {
            match i {
                Ok(_) => (),
                Err(e) =>
                    assert_eq!(e, InvalidCharError { pos: 16, invalid: InvalidChar::Utf8('🚀') }),
            }
        }
    }

    #[test]
    fn test_utf8() {
        let iter = HexDigitsIter::new_unchecked(b"abcd");
        let mut iter = HexToBytesIter::from_pairs(iter);
        assert_eq!(iter.next(), Some(Ok(0xab)));
        assert_eq!(iter.next(), Some(Ok(0xcd)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_utf8_errors() {
        // multi byte valid utf8 that starts at odd position and is even, then 0xff
        let iter = HexDigitsIter::new_unchecked(&[0x32, 0xc2, 0xab, 0xff]);
        let mut iter = HexToBytesIter::from_pairs(iter);
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Utf8('«'), pos: 1 }))
        );
        assert_eq!(iter.next(), None);

        // No more continuation
        let iter = HexDigitsIter::new_unchecked(&[0x32, 0xc2, 0x32, 0x32]);
        let mut iter = HexToBytesIter::from_pairs(iter);
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Other(0xc2), pos: 1 }))
        );
        assert_eq!(iter.next(), None);

        // high is not hex, low is ascii
        let iter = HexDigitsIter::new_unchecked(&[0xff, 0x7a]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Utf8('z'), pos: 1 }))
        );
        assert_eq!(iter.next(), None);

        // high is not hex, low is continuation
        let iter = HexDigitsIter::new_unchecked(&[0xc2, 0xab]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Utf8('«'), pos: 1 }))
        );
        assert_eq!(iter.next(), None);

        // high is not hex, low is not ascii, not continuation
        let iter = HexDigitsIter::new_unchecked(&[0xff, 0xe0]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Other(0xe0), pos: 1 }))
        );
        assert_eq!(iter.next(), None);

        // high is ascii, low is hex
        let iter = HexDigitsIter::new_unchecked(&[0x7a, 0x32]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Utf8('z'), pos: 0 }))
        );
        assert_eq!(iter.next(), None);

        // high is continuation, low is hex
        let iter = HexDigitsIter::new_unchecked(&[0x32, 0xc2, 0xab, 0x32]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Utf8('«'), pos: 2 }))
        );
        assert_eq!(iter.next(), None);

        // high is else, low is hex
        let iter = HexDigitsIter::new_unchecked(&[0xff, 0x32]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Other(0xff), pos: 0 }))
        );
        assert_eq!(iter.next(), None);

        // high is hex, low is ascii
        let iter = HexDigitsIter::new_unchecked(&[0x32, 0x7a]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Utf8('z'), pos: 1 }))
        );
        assert_eq!(iter.next(), None);

        // high is hex, low is else
        let iter = HexDigitsIter::new_unchecked(&[0x32, 0xff]);
        let mut iter = HexToBytesIter::from_pairs(iter).rev();
        assert_eq!(
            iter.next(),
            Some(Err(InvalidCharError { invalid: InvalidChar::Other(0xff), pos: 1 }))
        );
        assert_eq!(iter.next(), None);
    }
}
