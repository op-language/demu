//! EBML element primitives: variable-width identifiers and sizes, typed
//! writers over any `Write` sink, and a byte-slice reader.
//!
//! The Matroska layer builds on these primitives. Everything is integer
//! arithmetic; element payloads stay byte vectors.

use std::io::Write;

/// Write the variable-width EBML size prefix for a payload of `len`
/// bytes, in the minimal biased form.
///
/// The biased form fixes the width: a 1-byte size prefixes with
/// `0x80`, a 2-byte size with `0x4000`, and so on.
///
/// # Errors
///
/// Returns the sink's error.
pub fn write_size<W: Write>(out: &mut W, len: u64) -> std::io::Result<()> {
    if len < 0x7F {
        out.write_all(&[(len as u8) | 0x80])
    } else if len < 0x3FFF {
        out.write_all(&[((len >> 8) as u8) | 0x40, len as u8])
    } else if len < 0x1F_FFFF {
        out.write_all(&[((len >> 16) as u8) | 0x20, (len >> 8) as u8, len as u8])
    } else if len < 0x0FFF_FFFF {
        out.write_all(&[
            ((len >> 24) as u8) | 0x10,
            (len >> 16) as u8,
            (len >> 8) as u8,
            len as u8,
        ])
    } else if len < 0x07FF_FFFF_FFFF {
        out.write_all(&[
            ((len >> 32) as u8) | 0x08,
            (len >> 24) as u8,
            (len >> 16) as u8,
            (len >> 8) as u8,
            len as u8,
        ])
    } else {
        out.write_all(&[
            (len >> 56) as u8,
            (len >> 48) as u8,
            (len >> 40) as u8,
            (len >> 32) as u8,
            (len >> 24) as u8,
            (len >> 16) as u8,
            (len >> 8) as u8,
            len as u8,
        ])
    }
}

/// Write one EBML identifier in its fixed-width byte form.
///
/// # Errors
///
/// Returns the sink's error.
pub fn write_id<W: Write>(out: &mut W, id: u32) -> std::io::Result<()> {
    let bytes = id.to_be_bytes();
    if id <= 0xFF {
        out.write_all(&bytes[3..4])
    } else if id <= 0xFF_FF {
        out.write_all(&bytes[2..4])
    } else if id <= 0xFF_FF_FF {
        out.write_all(&bytes[1..4])
    } else {
        out.write_all(&bytes)
    }
}

/// Append one complete element: the identifier, the size prefix, and
/// the payload.
///
/// # Errors
///
/// Returns the sink's error.
pub fn element<W: Write>(out: &mut W, id: u32, payload: &[u8]) -> std::io::Result<()> {
    write_id(out, id)?;
    write_size(out, payload.len() as u64)?;
    out.write_all(payload)
}

/// Append one element whose payload is an unsigned integer, minimally
/// wide.
///
/// # Errors
///
/// Returns the sink's error.
pub fn uint_element<W: Write>(out: &mut W, id: u32, value: u64) -> std::io::Result<()> {
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    element(out, id, &bytes[start..])
}

/// Append one element whose payload is a signed integer, minimally wide
/// with the sign preserved.
///
/// # Errors
///
/// Returns the sink's error.
pub fn sint_element<W: Write>(out: &mut W, id: u32, value: i64) -> std::io::Result<()> {
    let bytes = value.to_be_bytes();
    let mut start = 0usize;
    while start < 7 {
        let sign_bit = (bytes[start] & 0x80) != 0;
        let next = bytes[start + 1];
        let redundant = if sign_bit { next == 0xFF } else { next == 0x00 };
        if !redundant {
            break;
        }
        start += 1;
    }
    element(out, id, &bytes[start..])
}

/// Append one element whose payload is an 8-byte IEEE-754 double,
/// passed by bit pattern so no float arithmetic happens.
///
/// # Errors
///
/// Returns the sink's error.
pub fn float_element<W: Write>(out: &mut W, id: u32, bits: u64) -> std::io::Result<()> {
    element(out, id, &bits.to_be_bytes())
}

/// Append one element whose payload is an ASCII or UTF-8 string.
///
/// # Errors
///
/// Returns the sink's error.
pub fn string_element<W: Write>(out: &mut W, id: u32, value: &str) -> std::io::Result<()> {
    element(out, id, value.as_bytes())
}

/// Append one element whose payload is a raw binary blob.
///
/// # Errors
///
/// Returns the sink's error.
pub fn binary_element<W: Write>(out: &mut W, id: u32, payload: &[u8]) -> std::io::Result<()> {
    element(out, id, payload)
}

/// A byte-cursor reader over an EBML byte stream.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

/// One decoded EBML element view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element {
    /// The element identifier.
    pub id: u32,
    /// The payload byte range inside the reader's buffer.
    pub payload: (usize, usize),
    /// The payload length in bytes.
    pub len: usize,
}

impl Element {
    /// The payload bytes of the element inside `data`.
    pub fn bytes<'b>(&self, data: &'b [u8]) -> &'b [u8] {
        &data[self.payload.0..self.payload.1]
    }
}

impl<'a> Reader<'a> {
    /// A reader over the whole byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// The read position.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Whether the cursor reached the end.
    pub fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Read one variable-width EBML identifier.
    fn read_id(&mut self) -> Option<u32> {
        let first = *self.data.get(self.pos)?;
        let width = usize::try_from(first.leading_zeros() + 1).ok()?;
        if width == 0 || width > 4 || self.pos + width > self.data.len() {
            return None;
        }
        let mut id = u32::from(first);
        for byte in &self.data[self.pos + 1..self.pos + width] {
            id = (id << 8) | u32::from(*byte);
        }
        self.pos += width;
        Some(id)
    }

    /// Read one variable-width size.
    fn read_size(&mut self) -> Option<u64> {
        let first = *self.data.get(self.pos)?;
        let width = usize::try_from(first.leading_zeros() + 1).ok()?;
        if width == 0 || width > 8 || self.pos + width > self.data.len() {
            return None;
        }
        let mut len = u64::from(first) & (0xFFu64 >> width);
        for byte in &self.data[self.pos + 1..self.pos + width] {
            len = (len << 8) | u64::from(*byte);
        }
        self.pos += width;
        // The all-ones form means "unknown size"; the caller falls back
        // to the rest of the buffer.
        if len == (1u64 << (7 * width)) - 1 {
            return None;
        }
        Some(len)
    }

    /// Read the next element at the cursor, or `None` at the end or at
    /// a malformed sequence.
    pub fn next_element(&mut self) -> Option<Element> {
        let id = self.read_id()?;
        let len = match self.read_size() {
            Some(len) => len,
            None => u64::try_from(self.data.len() - self.pos).ok()?,
        };
        let len = usize::try_from(len).ok()?;
        if self.pos.checked_add(len)? > self.data.len() {
            return None;
        }
        let element = Element {
            id,
            payload: (self.pos, self.pos + len),
            len,
        };
        self.pos += len;
        Some(element)
    }

    /// Interpret a payload byte slice as one unsigned integer.
    pub fn payload_uint(bytes: &[u8]) -> Option<u64> {
        if bytes.is_empty() || bytes.len() > 8 {
            return None;
        }
        let mut value = 0u64;
        for byte in bytes {
            value = (value << 8) | u64::from(*byte);
        }
        Some(value)
    }

    /// Interpret a payload byte slice as one signed integer.
    ///
    /// The subtraction runs in `i128` so an 8-byte payload with the
    /// sign bit set cannot overflow.
    pub fn payload_sint(bytes: &[u8]) -> Option<i64> {
        if bytes.is_empty() || bytes.len() > 8 {
            return None;
        }
        let negative = bytes[0] & 0x80 != 0;
        let mut value: i128 = 0;
        for byte in bytes {
            value = (value << 8) | i128::from(*byte);
        }
        if negative {
            let subtrahend = 1i128 << (8 * bytes.len() as u32);
            let corrected = value - subtrahend;
            i64::try_from(corrected).ok()
        } else {
            i64::try_from(value).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_prefixes_are_minimal_and_round_trip() {
        for len in [
            0u64,
            1,
            0x7E,
            0x7F,
            0x80,
            0x3FFE,
            0x4000,
            0x1F_FFFF,
            0x2000_0000,
        ] {
            let mut out = Vec::new();
            write_size(&mut out, len).expect("size writes");
            // The VINT size marks its width in leading zeros.
            let width = (out[0].leading_zeros()) as usize + 1;
            assert_eq!(out.len(), width, "width matches the prefix");
            let mut value = u64::from(out[0] & (0xFF >> width));
            for byte in &out[1..] {
                value = (value << 8) | u64::from(*byte);
            }
            assert_eq!(value, len, "len {len} round trips");
        }
    }

    #[test]
    fn element_wrapper_round_trips() {
        let mut out = Vec::new();
        element(&mut out, 0x86, b"matroska").expect("element writes");
        let mut cursor = Reader::new(&out);
        let element = cursor.next_element().expect("one element");
        assert_eq!(element.id, 0x86);
        assert_eq!(element.bytes(&out), b"matroska");
    }

    #[test]
    fn uint_elements_are_minimally_wide() {
        let mut out = Vec::new();
        uint_element(&mut out, 0xD7, 0).expect("zero writes");
        uint_element(&mut out, 0xD7, 1).expect("one writes");
        uint_element(&mut out, 0xD7, 300).expect("wide writes");
        let mut cursor = Reader::new(&out);
        let first = cursor.next_element().expect("first");
        assert_eq!(first.bytes(&out), [0u8]);
        let second = cursor.next_element().expect("second");
        assert_eq!(second.bytes(&out), [1u8]);
        let third = cursor.next_element().expect("third");
        assert_eq!(third.bytes(&out), [1, 44]);
    }

    #[test]
    fn payload_sint_round_trips() {
        for value in [
            -9_223_372_036_854_775_807i64 - 1,
            -32768i64,
            -1,
            0,
            255,
            32767,
            9_223_372_036_854_775_807,
        ] {
            let mut out = Vec::new();
            sint_element(&mut out, 0xE7, value).expect("sint writes");
            let mut cursor = Reader::new(&out);
            let element = cursor.next_element().expect("element");
            let decoded = Reader::payload_sint(element.bytes(&out));
            assert_eq!(decoded, Some(value), "value {value} round trips");
        }
        // The 8-byte sign-bit edge: i64::MIN with the high bit set.
        let bytes = i64::MIN.to_be_bytes();
        assert_eq!(Reader::payload_sint(&bytes), Some(i64::MIN));
    }
}
