//! Tiny big-endian byte reader.
//!
//! Both WOFF1 and WOFF2 are entirely big-endian; pulling the helper
//! into one file keeps the parsers honest about offset bookkeeping.

use crate::error::{Result, WoffError};

/// Cursor over a byte slice with checked big-endian primitives.
///
/// A handful of helpers are only used from the `woff2` module. We
/// silence dead-code warnings rather than gating them per-feature
/// because the file is internal and the methods are tiny.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

#[allow(dead_code)]
impl<'a> Reader<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) const fn position(&self) -> usize {
        self.pos
    }

    #[allow(dead_code)]
    pub(crate) const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn skip(&mut self, n: usize, ctx: &'static str) -> Result<()> {
        let end = self.pos.checked_add(n).ok_or(WoffError::Malformed {
            offset: self.pos,
            context: "skip overflows",
        })?;
        if end > self.data.len() {
            return Err(WoffError::UnexpectedEof {
                offset: self.pos,
                context: ctx,
            });
        }
        self.pos = end;
        Ok(())
    }

    pub(crate) fn read_bytes(&mut self, n: usize, ctx: &'static str) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(WoffError::Malformed {
            offset: self.pos,
            context: "read overflows",
        })?;
        if end > self.data.len() {
            return Err(WoffError::UnexpectedEof {
                offset: self.pos,
                context: ctx,
            });
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn read_u8(&mut self, ctx: &'static str) -> Result<u8> {
        let b = self.read_bytes(1, ctx)?;
        Ok(b[0])
    }

    pub(crate) fn read_u16(&mut self, ctx: &'static str) -> Result<u16> {
        let b = self.read_bytes(2, ctx)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub(crate) fn read_i16(&mut self, ctx: &'static str) -> Result<i16> {
        let b = self.read_bytes(2, ctx)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    pub(crate) fn read_u32(&mut self, ctx: &'static str) -> Result<u32> {
        let b = self.read_bytes(4, ctx)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn read_tag(&mut self, ctx: &'static str) -> Result<[u8; 4]> {
        let b = self.read_bytes(4, ctx)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    /// Reads a WOFF2 `UIntBase128` value.
    ///
    /// Each byte's MSB is a continuation flag; the low 7 bits are
    /// data, MSB-first. The spec caps the field at 5 bytes (so the
    /// representable range is 32 bits) and forbids leading 0x80 bytes
    /// to keep the encoding canonical.
    pub(crate) fn read_uint_base128(&mut self) -> Result<u32> {
        let mut accum: u32 = 0;
        for i in 0..5 {
            let byte = self.read_u8("UIntBase128")?;
            // Reject `0x80` as the very first byte: that's a
            // canonical "leading zero" the spec calls out.
            if i == 0 && byte == 0x80 {
                return Err(WoffError::Malformed {
                    offset: self.pos - 1,
                    context: "UIntBase128 with leading zero",
                });
            }
            // Overflow check before shifting: top 7 bits of `accum`
            // would be discarded.
            if accum & 0xFE00_0000 != 0 {
                return Err(WoffError::Malformed {
                    offset: self.pos - 1,
                    context: "UIntBase128 overflow",
                });
            }
            accum = (accum << 7) | u32::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Ok(accum);
            }
        }
        Err(WoffError::Malformed {
            offset: self.pos,
            context: "UIntBase128 longer than 5 bytes",
        })
    }

    /// Reads a WOFF2 `255UInt16` value (the variable-length glyph-stream
    /// encoding).
    ///
    /// Encoding: `253 <hi> <lo>` for raw 16-bit; `254 <byte>` for
    /// `byte + 506`; `255 <byte>` for `byte + 253`; otherwise the
    /// leading byte is the value itself.
    pub(crate) fn read_packed_u16(&mut self) -> Result<u16> {
        const ONE_MORE_BYTE_CODE_1: u8 = 255;
        const ONE_MORE_BYTE_CODE_2: u8 = 254;
        const WORD_CODE: u8 = 253;
        const LOWEST_U_CODE: u16 = 253;

        let code = self.read_u8("255UInt16 code")?;
        if code == WORD_CODE {
            self.read_u16("255UInt16 word")
        } else if code == ONE_MORE_BYTE_CODE_1 {
            let b = self.read_u8("255UInt16 byte")?;
            Ok(u16::from(b) + LOWEST_U_CODE)
        } else if code == ONE_MORE_BYTE_CODE_2 {
            let b = self.read_u8("255UInt16 byte")?;
            Ok(u16::from(b) + LOWEST_U_CODE * 2)
        } else {
            Ok(u16::from(code))
        }
    }
}
