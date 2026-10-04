//! `CFF `: Compact Font Format, version 1.
//!
//! Adobe's Type 2 charstring container, wrapped in a CFF header and a
//! series of length-prefixed INDEX structures. sigilbuzz parses only
//! what outline extraction needs:
//!
//! - Header: `major`, `minor`, `hdrSize`, `offSize`.
//! - Name INDEX: skipped.
//! - Top DICT INDEX: first entry only, for `CharStrings`, `charset`,
//!   `Private`, `FDArray`, and `FDSelect` offsets.
//! - String INDEX: skipped.
//! - Global Subr INDEX.
//! - CharStrings INDEX.
//! - Private DICT + Local Subr INDEX (single-font case).
//! - FDArray / FDSelect (CID-keyed fonts).
//! - Charset, only to find the base and accent glyphs of a `seac`.
//!
//! Parsing is lazy. [`Cff::parse`] reads the header, the Top DICT, and
//! the headers of the INDEX structures, which costs the same for ten
//! glyphs or sixty thousand. The one extra pass is over the FDSelect
//! range records of a CID-keyed font, to see whether they ascend and
//! can be binary-searched. Ranges that do not ascend take a second
//! pass, which records the span each range ends up with, so lookups
//! binary-search those instead. Charstrings, subroutines, and the
//! Font DICT and Private DICT of a CID-keyed glyph are located when
//! that glyph is drawn, so malformed data in one of them fails only
//! the glyphs that use it.
//!
//! Charstring execution is a Type 2 interpreter covering the outline
//! path-drawing operators, stem hints (parsed and skipped), and
//! subroutine dispatch with a spec-mandated depth cap of 10.
//!
//! The seac form of `endchar`, `[width] adx ady bchar achar endchar`,
//! draws the base character and then the accent at `(adx, ady)`, as
//! HarfBuzz and FreeType do. The two are named by Standard Encoding
//! code and found through the charset, in name-keyed fonts only.
//! `dotsection` clears the operand stack and does nothing else, as in
//! HarfBuzz and FreeType. Other deprecated Type 1 operators
//! (`callothersubr`, `pop`, and the like) are rejected.

mod charset;
mod charstring;
mod dict;
mod index;

use crate::error::{Error, Result};
use crate::tables::outline::OutlineSink;
use crate::tables::parse::Reader;

pub(crate) use charstring::{
    BlendContext, CharstringSink, Interp2, RegionCache, MAX_CHARSTRING_OPS, OUT_OF_CHARSTRING_OPS,
};
pub(crate) use dict::FdSelect;
pub(crate) use index::{read_index2, Index};

#[cfg(test)]
pub(crate) use index::encode_index;

use charstring::{Interp, Seac};
use dict::{read_local_subrs, TopDict};
use index::{read_index, slice_at};

/// A parsed `CFF ` table view.
#[derive(Debug, Clone)]
pub struct Cff<'a> {
    /// The whole table. Private DICT and Local Subrs offsets are
    /// relative to it.
    data: &'a [u8],
    /// Global subroutines.
    global_subrs: Index<'a>,
    /// CharStrings INDEX: one entry per glyph.
    char_strings: Index<'a>,
    /// Where each glyph's local subroutines come from.
    fonts: FontDicts<'a>,
    /// The Top DICT charset value: 0 to 2 for a predefined charset,
    /// otherwise its offset. Only a seac reads it.
    charset: u32,
}

/// The Private DICT layout of a `CFF ` table.
#[derive(Debug, Clone)]
enum FontDicts<'a> {
    /// A name-keyed font: one Private DICT, whose Local Subrs serve
    /// every glyph.
    Single(Index<'a>),
    /// A CID-keyed font: FDSelect picks a Font DICT in the FDArray for
    /// each glyph, and that Font DICT names the Private DICT. Without
    /// FDSelect every glyph uses Font DICT 0.
    Cid {
        fd_array: Index<'a>,
        fd_select: Option<FdSelect<'a>>,
    },
}

impl<'a> Cff<'a> {
    /// Parses the CFF1 table.
    ///
    /// This reads only the header, the Top DICT, INDEX headers, and the
    /// FDSelect range records, never anything per glyph. Problems inside
    /// a single charstring, subroutine, or CID Font DICT surface from
    /// [`Cff::outline`] for the glyphs that use it.
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
        let _name_index = read_index(&mut r)?;

        // Top DICT INDEX: use only the first entry in a single-font
        // CFF. (CFF technically supports a FontSet, but OpenType
        // restricts it to one font per `CFF ` table.)
        let top_index = read_index(&mut r)?;
        if top_index.is_empty() {
            return Err(Error::Malformed {
                offset: 0,
                context: "CFF Top DICT INDEX empty",
            });
        }
        let top_dict_bytes = top_index.get(0)?;

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
        let fonts = if let Some((size, off)) = top.private {
            // Single Private DICT at (off, size). Parse Local Subrs.
            let priv_bytes = slice_at(data, off as usize, size as usize)?;
            FontDicts::Single(read_local_subrs(data, priv_bytes, off as usize)?)
        } else if let Some(fd_array_off) = top.fd_array {
            // CID font: FDArray is an INDEX of font dicts, each
            // carrying its own Private DICT.
            let mut fda_reader = Reader::at(data, fd_array_off as usize)?;
            let fd_array = read_index(&mut fda_reader)?;
            // FDSelect names the Font DICT for each glyph. CFF1
            // defines formats 0 and 3.
            let fd_select = match top.fd_select {
                Some(off) => Some(FdSelect::parse(
                    data,
                    off as usize,
                    char_strings.len(),
                    false,
                    "CFF FDSelect format != 0/3",
                )?),
                None => None,
            };
            FontDicts::Cid {
                fd_array,
                fd_select,
            }
        } else {
            // No Private DICT info at all: font has no subroutines.
            FontDicts::Single(Index::default())
        };

        Ok(Self {
            data,
            global_subrs,
            char_strings,
            fonts,
            charset: top.charset.unwrap_or(0),
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
    ///
    /// A glyph whose `endchar` takes the seac form draws whatever its
    /// own charstring drew, then the base glyph, then the accent glyph
    /// moved to the accent origin.
    pub fn outline<S: OutlineSink>(&self, glyph_id: u16, sink: &mut S) -> Result<bool> {
        self.draw(glyph_id, sink)
    }

    /// [`Cff::outline`] into any [`CharstringSink`], such as one that
    /// keeps the points in the `f64` the charstring is evaluated in.
    pub(crate) fn draw<S: CharstringSink>(&self, glyph_id: u16, sink: &mut S) -> Result<bool> {
        self.draw_with(glyph_id, sink, None)
    }

    /// [`Cff::draw`] for a caller that bounds many glyphs together: the
    /// glyph's charstrings (a seac's base and accent included) may run
    /// at most `ops_left` operations between them, which they take from
    /// `ops_left`, failing with the operation-limit error past them.
    pub(crate) fn draw_limited<S: CharstringSink>(
        &self,
        glyph_id: u16,
        sink: &mut S,
        ops_left: &mut u32,
    ) -> Result<bool> {
        self.draw_with(glyph_id, sink, Some(ops_left))
    }

    /// [`Cff::draw`], each charstring limited to its own operations, or
    /// with `ops_left` shared as [`Cff::draw_limited`] shares it.
    fn draw_with<S: CharstringSink>(
        &self,
        glyph_id: u16,
        sink: &mut S,
        mut ops_left: Option<&mut u32>,
    ) -> Result<bool> {
        let gid = usize::from(glyph_id);
        if gid >= self.char_strings.len() {
            return Ok(false);
        }
        let local_subrs = self.local_subrs(gid)?;
        let seac = self.run_charstring(gid, local_subrs, sink, None, ops_left.as_deref_mut())?;
        if let Some(seac) = seac {
            self.draw_seac(seac, local_subrs, sink, ops_left)?;
        }
        Ok(true)
    }

    /// Runs glyph `gid`'s charstring into `sink` and returns the seac
    /// its `endchar` asked for. `component` is the origin of a seac base
    /// or accent; a seac inside one fails.
    fn run_charstring<S: CharstringSink>(
        &self,
        gid: usize,
        local_subrs: Index<'a>,
        sink: &mut S,
        component: Option<(f64, f64)>,
        ops_left: Option<&mut u32>,
    ) -> Result<Option<Seac>> {
        let cs = self.char_strings.get(gid)?;
        let mut interp = Interp::new(self.global_subrs, local_subrs, sink, false);
        if let Some((x, y)) = component {
            interp.start_seac_component(x, y);
        }
        let ran = match ops_left {
            Some(ops_left) => {
                interp.limit_ops(*ops_left);
                let ran = interp.run(cs, 0);
                *ops_left = ops_left.saturating_sub(interp.ops());
                ran
            }
            None => interp.run(cs, 0),
        };
        ran?;
        // A well-formed CFF1 charstring has already closed its last
        // contour at endchar; this only covers charstrings that end
        // without one.
        interp.finish();
        Ok(interp.take_seac())
    }

    /// Draws a seac's base glyph at the origin and its accent glyph at
    /// `(adx, ady)`, finding both through the charset. Both use the
    /// Local Subrs of the glyph that asked for them. CID-keyed fonts
    /// name glyphs by CID, not SID, so seac is unsupported there.
    fn draw_seac<S: CharstringSink>(
        &self,
        seac: Seac,
        local_subrs: Index<'a>,
        sink: &mut S,
        mut ops_left: Option<&mut u32>,
    ) -> Result<()> {
        if let FontDicts::Cid { .. } = self.fonts {
            return Err(Error::Unsupported {
                context: "CFF seac in a CID-keyed font",
            });
        }
        let n_glyphs = self.char_strings.len();
        let base = charset::glyph_for_sid(self.data, self.charset, seac.base, n_glyphs)?;
        let accent = charset::glyph_for_sid(self.data, self.charset, seac.accent, n_glyphs)?;
        let origin = Some((0.0, 0.0));
        self.run_charstring(base, local_subrs, sink, origin, ops_left.as_deref_mut())?;
        let accent_origin = Some((seac.adx, seac.ady));
        self.run_charstring(accent, local_subrs, sink, accent_origin, ops_left)?;
        Ok(())
    }

    /// The Local Subrs INDEX for glyph `gid`. A CID-keyed glyph whose
    /// FD has no Font DICT, or whose Font DICT has no Private DICT,
    /// gets an empty one.
    fn local_subrs(&self, gid: usize) -> Result<Index<'a>> {
        match &self.fonts {
            FontDicts::Single(local) => Ok(*local),
            FontDicts::Cid {
                fd_array,
                fd_select,
            } => {
                let fd = usize::from(fd_select.as_ref().map_or(0, |s| s.fd_for_glyph(gid)));
                if fd >= fd_array.len() {
                    return Ok(Index::default());
                }
                let font_dict = TopDict::parse(fd_array.get(fd)?)?;
                let Some((size, off)) = font_dict.private else {
                    return Ok(Index::default());
                };
                let priv_bytes = slice_at(self.data, off as usize, size as usize)?;
                read_local_subrs(self.data, priv_bytes, off as usize)
            }
        }
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
    pub const ESC_DOTSECTION: u8 = 0;
    pub const ESC_HFLEX: u8 = 34;
    pub const ESC_FLEX: u8 = 35;
    pub const ESC_HFLEX1: u8 = 36;
    pub const ESC_FLEX1: u8 = 37;
}

#[cfg(test)]
#[allow(clippy::vec_init_then_push, clippy::same_item_push)]
mod tests;
