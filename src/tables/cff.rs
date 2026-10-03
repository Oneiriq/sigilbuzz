//! `CFF `: Compact Font Format, version 1.
//!
//! Adobe's Type 2 charstring container, wrapped in a CFF header and a
//! series of length-prefixed INDEX structures. sigilbuzz parses only
//! what outline extraction needs:
//!
//! - Header: `major`, `minor`, `hdrSize`, `offSize`.
//! - Name INDEX: skipped.
//! - Top DICT INDEX: first entry only, for `CharStrings`, `Private`,
//!   `FDArray`, and `FDSelect` offsets.
//! - String INDEX: skipped.
//! - Global Subr INDEX.
//! - CharStrings INDEX.
//! - Private DICT + Local Subr INDEX (single-font case).
//! - FDArray / FDSelect (CID-keyed fonts).
//!
//! Charstring execution is a Type 2 interpreter covering the outline
//! path-drawing operators, stem hints (parsed and skipped), and
//! subroutine dispatch with a spec-mandated depth cap of 10.
//!
//! Deprecated Type 1 operators (`closepath`, `seac`, `callothersubr`,
//! `pop`) are rejected.

mod charstring;
mod dict;
mod index;

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::outline::OutlineSink;
use crate::tables::parse::Reader;

pub(crate) use charstring::{BlendContext, Interp2};
pub(crate) use dict::fill_fd_ranges;
pub(crate) use index::{read_index, read_index2};

use charstring::Interp;
use dict::{parse_fd_select, read_local_subrs, TopDict};
use index::slice_at;

/// A parsed `CFF ` table view.
#[derive(Debug, Clone)]
pub struct Cff<'a> {
    /// Global subroutines, indexed 0..len.
    global_subrs: Vec<&'a [u8]>,
    /// Per-font-dict local subroutines. CID fonts pick one per glyph
    /// via FDSelect; non-CID fonts store a single entry.
    local_subrs: Vec<Vec<&'a [u8]>>,
    /// CharStrings INDEX: one entry per glyph.
    char_strings: Vec<&'a [u8]>,
    /// FDSelect mapping: Some(indices) for CID, None for simple.
    fd_select: Option<Vec<u8>>,
}

impl<'a> Cff<'a> {
    /// Parses the CFF1 table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u8()?;
        let _minor = r.read_u8()?;
        let hdr_size = r.read_u8()? as usize;
        let _off_size = r.read_u8()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "CFF major version != 1",
            });
        }
        if hdr_size < 4 {
            return Err(Error::Malformed {
                offset: 2,
                context: "CFF hdrSize < 4",
            });
        }
        // Skip any padding between the fixed header and the Name INDEX.
        r.seek(hdr_size)?;

        // Name INDEX: skip.
        let name_index = read_index(&mut r)?;

        // Top DICT INDEX: use only the first entry in a single-font
        // CFF. (CFF technically supports a FontSet, but OpenType
        // restricts it to one font per `CFF ` table.)
        let top_index = read_index(&mut r)?;
        let top_dict_bytes = top_index.first().copied().ok_or(Error::Malformed {
            offset: 0,
            context: "CFF Top DICT INDEX empty",
        })?;
        let _ = name_index;

        // String INDEX: skip.
        let _string_index = read_index(&mut r)?;

        // Global Subr INDEX.
        let global_subrs = read_index(&mut r)?;

        // Parse Top DICT for the offsets we need.
        let top = TopDict::parse(top_dict_bytes)?;

        // CharStrings INDEX (absolute offset into CFF data).
        let char_strings_off = top.char_strings.ok_or(Error::Malformed {
            offset: 0,
            context: "CFF Top DICT missing CharStrings",
        })? as usize;
        let mut cs_reader = Reader::at(data, char_strings_off)?;
        let char_strings = read_index(&mut cs_reader)?;

        // Private DICT(s) + local subrs.
        let (local_subrs, fd_select) = if let Some((size, off)) = top.private {
            // Single Private DICT at (off, size). Parse Local Subrs.
            let priv_bytes = slice_at(data, off as usize, size as usize)?;
            let local = read_local_subrs(data, priv_bytes, off as usize)?;
            (alloc::vec![local], None)
        } else if let Some(fd_array_off) = top.fd_array {
            // CID font: FDArray is an INDEX of font dicts, each
            // carrying its own Private DICT.
            let mut fda_reader = Reader::at(data, fd_array_off as usize)?;
            let fd_array = read_index(&mut fda_reader)?;
            let mut locals = Vec::with_capacity(fd_array.len());
            for font_dict_bytes in &fd_array {
                let fd = TopDict::parse(font_dict_bytes)?;
                if let Some((size, off)) = fd.private {
                    let priv_bytes = slice_at(data, off as usize, size as usize)?;
                    let local = read_local_subrs(data, priv_bytes, off as usize)?;
                    locals.push(local);
                } else {
                    locals.push(Vec::new());
                }
            }
            // FDSelect: one u8 or u16 per glyph naming which Private
            // DICT to use. Parse format 0 and format 3.
            let fd_select = if let Some(fd_sel_off) = top.fd_select {
                let n_glyphs = char_strings.len();
                Some(parse_fd_select(data, fd_sel_off as usize, n_glyphs)?)
            } else {
                None
            };
            (locals, fd_select)
        } else {
            // No Private DICT info at all: font has no subroutines.
            (alloc::vec![Vec::new()], None)
        };

        Ok(Self {
            global_subrs,
            local_subrs,
            char_strings,
            fd_select,
        })
    }

    /// Number of glyphs covered.
    #[must_use]
    pub fn num_glyphs(&self) -> u16 {
        self.char_strings.len() as u16
    }

    /// Drives `sink` with the path ops for `glyph_id`. Returns
    /// `Ok(false)` when the id has no charstring (out of range),
    /// `Ok(true)` otherwise. An empty charstring counts as drawn:
    /// callers filter on `Outline::is_empty`.
    pub fn outline<S: OutlineSink>(&self, glyph_id: u16, sink: &mut S) -> Result<bool> {
        let Some(cs) = self.char_strings.get(glyph_id as usize) else {
            return Ok(false);
        };
        let local_idx = if let Some(ref sel) = self.fd_select {
            sel.get(glyph_id as usize).copied().unwrap_or(0) as usize
        } else {
            0
        };
        let local_subrs = self
            .local_subrs
            .get(local_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut interp = Interp::new(&self.global_subrs, local_subrs, sink, false);
        interp.run(cs, 0)?;
        // A well-formed CFF1 charstring has already closed its last
        // contour at endchar; this only covers charstrings that end
        // without one.
        interp.finish();
        Ok(true)
    }
}

/// Charstring operators we care about. Numeric values from the Type 2
/// spec (Adobe Technical Note #5177).
pub(crate) mod op_code {
    pub const HSTEM: u8 = 1;
    pub const VSTEM: u8 = 3;
    pub const VMOVETO: u8 = 4;
    pub const RLINETO: u8 = 5;
    pub const HLINETO: u8 = 6;
    pub const VLINETO: u8 = 7;
    pub const RRCURVETO: u8 = 8;
    pub const CALLSUBR: u8 = 10;
    pub const RETURN: u8 = 11;
    pub const ESCAPE: u8 = 12;
    pub const ENDCHAR: u8 = 14;
    pub const VSINDEX: u8 = 15; // CFF2
    pub const BLEND: u8 = 16; // CFF2
    pub const HSTEMHM: u8 = 18;
    pub const HINTMASK: u8 = 19;
    pub const CNTRMASK: u8 = 20;
    pub const RMOVETO: u8 = 21;
    pub const HMOVETO: u8 = 22;
    pub const VSTEMHM: u8 = 23;
    pub const RCURVELINE: u8 = 24;
    pub const RLINECURVE: u8 = 25;
    pub const VVCURVETO: u8 = 26;
    pub const HHCURVETO: u8 = 27;
    pub const SHORTINT: u8 = 28;
    pub const CALLGSUBR: u8 = 29;
    pub const VHCURVETO: u8 = 30;
    pub const HVCURVETO: u8 = 31;

    // Escaped (prefix 12).
    pub const ESC_HFLEX: u8 = 34;
    pub const ESC_FLEX: u8 = 35;
    pub const ESC_HFLEX1: u8 = 36;
    pub const ESC_FLEX1: u8 = 37;
}

#[cfg(test)]
#[allow(clippy::vec_init_then_push, clippy::same_item_push)]
mod tests;
