//! The `seac` part of the glyph closure: the base and accent glyphs an
//! accented CFF1 glyph draws.
//!
//! A Type 2 charstring keeps Type 1's `seac` as an `endchar` with four
//! operands, `adx ady bchar achar endchar` (five with a width): draw
//! the glyph for Standard Encoding code `bchar`, then the one for
//! `achar` with its origin at `(adx, ady)`. The Standard Encoding turns
//! each code into a SID, and the font's charset gives the glyph with
//! that SID. A subset that keeps such a glyph keeps both components,
//! as HarfBuzz's subsetter does, or the glyph draws wrong (or not at
//! all) in the subset.
//!
//! Only name-keyed fonts use `seac`: a CID-keyed font's charset maps
//! CIDs, not SIDs, and HarfBuzz finds no components there either. The
//! predefined Expert charsets hold no Standard Encoding glyphs, so a
//! font that uses one gets no components.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::charset::read_explicit_charset;
use super::charstring::{decode_operand, subr_bias};
use super::reader::parse_cff1;
use crate::util::{WorkBudget, WORK_LIMIT};

/// The SID of each Standard Encoding code, with 0 for the codes the
/// encoding leaves undefined. Adobe Technical Note #5176, Appendix B.
#[rustfmt::skip]
const STANDARD_ENCODING: [u8; 256] = [
    // 0-31: undefined.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 32-126: space through asciitilde, SIDs 1 to 95 in order.
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64,
    65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80,
    81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 0,
    // 128-159: undefined.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 160-255: exclamdown through germandbls, with gaps.
    0, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110,
    0, 111, 112, 113, 114, 0, 115, 116, 117, 118, 119, 120, 121, 122, 0, 123,
    0, 124, 125, 126, 127, 128, 129, 130, 131, 0, 132, 133, 0, 134, 135, 136,
    137, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 138, 0, 139, 0, 0, 0, 0, 140, 141, 142, 143, 0, 0, 0, 0,
    0, 144, 0, 0, 0, 145, 0, 0, 146, 147, 148, 149, 0, 0, 0, 0,
];

/// Highest SID in the ISOAdobe charset (`zcaron`): with the predefined
/// ISOAdobe charset, glyph `i` has SID `i` up to it.
const ISO_ADOBE_LAST_SID: u16 = 228;

/// Subroutine nesting the Type 2 spec allows.
const MAX_SUBR_DEPTH: u8 = 10;

/// Operands a Type 2 charstring may stack (CFF2's limit, which is above
/// CFF1's 48).
const MAX_STACK: usize = 513;

/// The seac part of one closure walk over a name-keyed CFF1 table.
///
/// Each kept glyph's charstring runs once, however many passes the
/// closure makes, against a work budget of its own, so a font full of
/// subroutine calls cannot use up the budget the layout passes share.
pub(crate) struct SeacClosure<'a> {
    global: Vec<&'a [u8]>,
    local: Vec<&'a [u8]>,
    char_strings: Vec<&'a [u8]>,
    /// The glyph each charset SID names.
    glyph_of_sid: BTreeMap<u16, u16>,
    /// Glyphs whose charstrings already ran.
    scanned: Vec<bool>,
    budget: WorkBudget,
}

impl<'a> SeacClosure<'a> {
    /// Reads the CFF1 table `cff`. `None` when it has no seac glyphs to
    /// find: it does not parse, is CID-keyed, or uses a predefined
    /// Expert charset (see the module docs).
    pub(crate) fn new(cff: &'a [u8]) -> Option<Self> {
        let parsed = parse_cff1(cff).ok()?;
        if parsed.is_cid {
            return None;
        }
        let glyph_of_sid = charset_glyphs(cff, parsed.charset_off, parsed.char_strings.len())?;
        Some(Self {
            scanned: alloc::vec![false; parsed.char_strings.len()],
            global: parsed.global_subrs,
            local: parsed.local_subrs,
            char_strings: parsed.char_strings,
            glyph_of_sid,
            budget: WorkBudget::new(WORK_LIMIT),
        })
    }

    /// Marks the base and accent glyphs of every kept seac glyph in
    /// `keep`. Returns true when that kept a glyph that was not kept
    /// before. A charstring that cannot be run, and a code with no
    /// glyph, add nothing: the closure is best effort.
    pub(crate) fn expand(&mut self, keep: &mut [bool]) -> bool {
        let mut added = false;
        for gid in 0..keep.len().min(self.char_strings.len()) {
            if !keep[gid] || self.scanned[gid] {
                continue;
            }
            self.scanned[gid] = true;
            let mut scan = SeacScan {
                global: &self.global,
                local: &self.local,
                stack: Vec::new(),
                stems: 0,
                budget: &self.budget,
            };
            let Step::End(Some((base, accent))) = scan.run(self.char_strings[gid], 0) else {
                continue;
            };
            for code in [base, accent] {
                let component = standard_sid(code).and_then(|sid| self.glyph_of_sid.get(&sid));
                if let Some(slot) = component.and_then(|&g| keep.get_mut(usize::from(g))) {
                    added |= !*slot;
                    *slot = true;
                }
            }
        }
        added
    }
}

/// The SID the Standard Encoding gives charstring operand `code`:
/// `None` outside 0 to 255 (a fraction truncates, as in HarfBuzz) and
/// for codes the encoding leaves undefined.
fn standard_sid(code: f32) -> Option<u16> {
    if !(0.0..256.0).contains(&code) {
        return None;
    }
    let sid = *STANDARD_ENCODING.get(code as usize)?;
    (sid != 0).then_some(u16::from(sid))
}

/// The glyph each SID of the charset at `charset_off` names, the first
/// one when several share a SID. `None` for the predefined Expert
/// charsets, which name no Standard Encoding glyphs, and for a charset
/// that cannot be read.
fn charset_glyphs(cff: &[u8], charset_off: u32, n_glyphs: usize) -> Option<BTreeMap<u16, u16>> {
    let n_glyphs = u16::try_from(n_glyphs).unwrap_or(u16::MAX);
    let mut map = BTreeMap::new();
    match charset_off {
        0 => {
            for gid in 1..n_glyphs.min(ISO_ADOBE_LAST_SID + 1) {
                map.insert(gid, gid);
            }
        }
        1 | 2 => return None,
        off => {
            let sids =
                read_explicit_charset(cff, off as usize, usize::from(n_glyphs).saturating_sub(1))
                    .ok()?;
            for (i, &sid) in sids.iter().enumerate() {
                // Glyph 0 is `.notdef`, which the charset leaves out.
                let gid = u16::try_from(i + 1).ok()?;
                map.entry(sid).or_insert(gid);
            }
        }
    }
    Some(map)
}

/// How a charstring (or subroutine) run ended.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    /// A subroutine returned, or a charstring ran out of bytes.
    Continue,
    /// `endchar`, with the seac's base and accent codes when it took
    /// the seac form.
    End(Option<(f32, f32)>),
    /// The charstring could not be run.
    Fail,
}

/// A Type 2 charstring walker that tracks only what finding `seac`
/// needs: the operand stack, subroutine calls, and the stem count that
/// sizes hint masks.
struct SeacScan<'a> {
    global: &'a [&'a [u8]],
    local: &'a [&'a [u8]],
    stack: Vec<f32>,
    /// Stem hints declared so far, across subroutines.
    stems: usize,
    budget: &'a WorkBudget,
}

impl SeacScan<'_> {
    /// Runs `cs` at subroutine nesting `depth`.
    fn run(&mut self, cs: &[u8], depth: u8) -> Step {
        if !self.budget.spend(cs.len().max(1)) {
            return Step::Fail;
        }
        let mut pos = 0;
        while let Some(&b0) = cs.get(pos) {
            match b0 {
                // Operands.
                28 => {
                    let Some(b) = cs.get(pos + 1..pos + 3) else {
                        return Step::Fail;
                    };
                    self.stack.push(f32::from(i16::from_be_bytes([b[0], b[1]])));
                    pos += 3;
                }
                255 => {
                    let Some(b) = cs.get(pos + 1..pos + 5) else {
                        return Step::Fail;
                    };
                    let fixed = i32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                    self.stack.push(fixed as f32 / 65536.0);
                    pos += 5;
                }
                32..=254 => {
                    let Some((v, len)) = decode_operand(cs, pos) else {
                        return Step::Fail;
                    };
                    self.stack.push(v as f32);
                    pos += len;
                }
                // callsubr, callgsubr.
                10 | 29 => {
                    let subrs = if b0 == 10 { self.local } else { self.global };
                    let Some(index) = self.stack.pop() else {
                        return Step::Fail;
                    };
                    let target = index as i64 + i64::from(subr_bias(subrs.len()));
                    let subr = usize::try_from(target).ok().and_then(|i| subrs.get(i));
                    let Some(subr) = subr else {
                        return Step::Fail;
                    };
                    if depth >= MAX_SUBR_DEPTH {
                        return Step::Fail;
                    }
                    match self.run(subr, depth + 1) {
                        Step::Continue => {}
                        other => return other,
                    }
                    pos += 1;
                }
                // return.
                11 => return Step::Continue,
                // endchar: the seac form takes the top four operands,
                // a width below them or not.
                14 => {
                    let n = self.stack.len();
                    return Step::End((n >= 4).then(|| (self.stack[n - 2], self.stack[n - 1])));
                }
                // hstem, vstem, hstemhm, vstemhm.
                1 | 3 | 18 | 23 => {
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                    pos += 1;
                }
                // hintmask, cntrmask: an implicit vstem may come first.
                19 | 20 => {
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                    pos += 1 + self.stems.div_ceil(8);
                }
                // Escaped two-byte operators: flex and arithmetic.
                12 => {
                    self.stack.clear();
                    pos += 2;
                }
                // Path construction.
                4 | 5 | 6 | 7 | 8 | 21 | 22 | 24 | 25 | 26 | 27 | 30 | 31 => {
                    self.stack.clear();
                    pos += 1;
                }
                // Reserved in CFF1 (including CFF2's vsindex and blend).
                _ => return Step::Fail,
            }
            if self.stack.len() > MAX_STACK {
                return Step::Fail;
            }
        }
        Step::Continue
    }
}

#[cfg(test)]
mod tests;
