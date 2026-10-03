//! Type 2 charstring interpreter, shared by `CFF ` and `CFF2`.

use alloc::vec::Vec;

use super::charset::standard_encoding_sid;
use super::index::Index;
use super::op_code;
use crate::error::{Error, Result};
use crate::tables::outline::OutlineSink;
use crate::tables::parse::{abs_f32, Reader};

/// Subroutine recursion cap. CFF spec says 10 per Type 2.
const MAX_SUBR_DEPTH: u8 = 10;

/// Operand-stack cap for CFF1 charstrings. Type 2 spec §3.1 ceiling.
const CFF1_STACK_LIMIT: usize = 48;

/// Operand-stack cap for CFF2 charstrings. CFF2 spec §3.1 ceiling.
const CFF2_STACK_LIMIT: usize = 513;

/// Cap on the operands and operators one glyph may execute, counted
/// across every subroutine call. The depth cap alone does not bound
/// the work: a subroutine that calls the next one many times, ten
/// levels deep, runs for an exponential number of steps. Real glyphs
/// stay far below this limit.
const MAX_CHARSTRING_OPS: u32 = 100_000;

// ----------------------------------------------------------------------------
// Type 2 charstring interpreter.
// ----------------------------------------------------------------------------

pub(crate) struct Interp<'a, 'b, S: OutlineSink> {
    global: Index<'a>,
    local: Index<'a>,
    sink: &'b mut S,
    /// Operand stack. CFF spec caps this at 48 for CFF1, 513 for CFF2.
    stack: Vec<f32>,
    /// Current pen position.
    x: f32,
    y: f32,
    /// Running stem count, for width determination and hintmask
    /// padding.
    stem_count: u32,
    /// Set once we enter the first drawing operator. Before that
    /// the first optional element on the stack is the glyph width.
    consumed_width: bool,
    /// True when the interpreter should honor CFF2 extensions
    /// (`blend`, `vsindex`) and omit the width / endchar bookkeeping.
    is_cff2: bool,
    /// True once endchar fires. Outer loop halts.
    done: bool,
    /// True while a contour is open, from a move operator until the
    /// next move operator, endchar, or [`Self::finish`] closes it.
    in_contour: bool,
    /// Operands and operators executed so far, checked against
    /// [`MAX_CHARSTRING_OPS`].
    ops: u32,
    /// CFF2 blend support.
    pub(crate) blend: Option<BlendContext<'b>>,
    /// Region count and scalars for the blend context's current
    /// `vsindex`. Every `blend` in a glyph reuses them until `vsindex`
    /// changes, instead of re-reading the variation store each time.
    blend_regions: Option<BlendRegions>,
    /// The accented character an `endchar` with seac operands asked
    /// for. The caller draws it once the run ends.
    seac: Option<Seac>,
    /// True while drawing the base or accent of a seac, which may not
    /// use seac itself.
    seac_component: bool,
}

/// A Type 2 `endchar` in its seac form, `adx ady bchar achar endchar`:
/// draw the base character, then the accent with its origin at
/// `(adx, ady)`. Both are named by Standard Encoding code, kept here as
/// the SIDs the encoding gives them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Seac {
    /// Accent origin x, relative to the base character's origin.
    pub(crate) adx: f32,
    /// Accent origin y, relative to the base character's origin.
    pub(crate) ady: f32,
    /// SID of the base character.
    pub(crate) base: u16,
    /// SID of the accent character.
    pub(crate) accent: u16,
}

/// Variation store data for one `vsindex`, computed once per outline.
struct BlendRegions {
    /// The `vsindex` these values belong to.
    vsindex: u16,
    /// Regions per delta row, from the ItemVariationData subtable.
    /// `None` when the subtable is missing or truncated.
    count: Option<u16>,
    /// One scalar per region at the outline's coords.
    scalars: Vec<f32>,
}

pub(crate) struct BlendContext<'b> {
    /// Normalized coords; one per axis.
    pub coords: &'b [f32],
    /// Item variation store (offset + bytes).
    pub ivs: &'b crate::tables::variation_store::ItemVariationStore<'b>,
    /// Current vsindex.
    pub vsindex: u16,
}

impl<'a, 'b, S: OutlineSink> Interp<'a, 'b, S> {
    pub(crate) fn new(global: Index<'a>, local: Index<'a>, sink: &'b mut S, is_cff2: bool) -> Self {
        Self {
            global,
            local,
            sink,
            stack: Vec::with_capacity(48),
            x: 0.0,
            y: 0.0,
            stem_count: 0,
            consumed_width: is_cff2, // CFF2 never carries a width.
            is_cff2,
            done: false,
            in_contour: false,
            ops: 0,
            blend: None,
            blend_regions: None,
            seac: None,
            seac_component: false,
        }
    }

    /// Makes this interpreter draw a seac base or accent: the pen starts
    /// at the component's origin `(x, y)` instead of `(0, 0)`, and a
    /// seac inside the component is an error.
    pub(crate) fn start_seac_component(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
        self.seac_component = true;
    }

    /// The seac that the charstring's `endchar` asked for, if any.
    pub(crate) fn take_seac(&mut self) -> Option<Seac> {
        self.seac.take()
    }

    pub(crate) fn run(&mut self, code: &'a [u8], depth: u8) -> Result<()> {
        if depth > MAX_SUBR_DEPTH {
            return Err(Error::Malformed {
                offset: 0,
                context: "CFF subroutine depth exceeded",
            });
        }
        let mut r = Reader::new(code);
        while !r.is_empty() {
            if self.done {
                return Ok(());
            }
            self.ops += 1;
            if self.ops > MAX_CHARSTRING_OPS {
                return Err(Error::Malformed {
                    offset: r.position(),
                    context: "CFF charstring exceeds operation limit",
                });
            }
            let b0 = r.read_u8()?;
            if (32..=246).contains(&b0) {
                self.push((i32::from(b0) - 139) as f32)?;
            } else if (247..=250).contains(&b0) {
                let b1 = r.read_u8()?;
                let v = ((i32::from(b0) - 247) * 256 + i32::from(b1) + 108) as f32;
                self.push(v)?;
            } else if (251..=254).contains(&b0) {
                let b1 = r.read_u8()?;
                let v = (-(i32::from(b0) - 251) * 256 - i32::from(b1) - 108) as f32;
                self.push(v)?;
            } else if b0 == 255 {
                // 16.16 fixed.
                let raw = r.read_i32()?;
                self.push(raw as f32 / 65536.0)?;
            } else if b0 == op_code::SHORTINT {
                let v = r.read_i16()?;
                self.push(f32::from(v))?;
            } else {
                // Operator.
                self.exec_op(b0, &mut r, depth)?;
            }
        }
        Ok(())
    }

    fn exec_op(&mut self, b0: u8, r: &mut Reader<'a>, depth: u8) -> Result<()> {
        match b0 {
            op_code::HSTEM | op_code::VSTEM | op_code::HSTEMHM | op_code::VSTEMHM => {
                self.maybe_consume_width();
                // Each stem pair consumes two args; stem_count += stack/2.
                let n = (self.stack.len() as u32) / 2;
                self.stem_count += n;
                self.stack.clear();
            }
            op_code::HINTMASK | op_code::CNTRMASK => {
                self.maybe_consume_width();
                // An implicit vstem may precede the first mask if
                // there are operands left over.
                let extra = (self.stack.len() as u32) / 2;
                self.stem_count += extra;
                self.stack.clear();
                let n_bytes = (self.stem_count as usize).div_ceil(8);
                r.skip(n_bytes)?;
            }
            op_code::RMOVETO => {
                self.maybe_consume_width();
                self.close_contour();
                let dy = self.pop()?;
                let dx = self.pop()?;
                self.x += dx;
                self.y += dy;
                self.sink.move_to(self.x, self.y);
                self.in_contour = true;
                self.stack.clear();
            }
            op_code::HMOVETO => {
                self.maybe_consume_width();
                self.close_contour();
                let dx = self.pop()?;
                self.x += dx;
                self.sink.move_to(self.x, self.y);
                self.in_contour = true;
                self.stack.clear();
            }
            op_code::VMOVETO => {
                self.maybe_consume_width();
                self.close_contour();
                let dy = self.pop()?;
                self.y += dy;
                self.sink.move_to(self.x, self.y);
                self.in_contour = true;
                self.stack.clear();
            }
            op_code::RLINETO => {
                let args = core::mem::take(&mut self.stack);
                let mut i = 0;
                while i + 1 < args.len() {
                    self.x += args[i];
                    self.y += args[i + 1];
                    self.sink.line_to(self.x, self.y);
                    i += 2;
                }
            }
            op_code::HLINETO => {
                // Alternating horizontal/vertical, starting horizontal.
                let args = core::mem::take(&mut self.stack);
                let mut horiz = true;
                for &a in &args {
                    if horiz {
                        self.x += a;
                    } else {
                        self.y += a;
                    }
                    self.sink.line_to(self.x, self.y);
                    horiz = !horiz;
                }
            }
            op_code::VLINETO => {
                let args = core::mem::take(&mut self.stack);
                let mut horiz = false;
                for &a in &args {
                    if horiz {
                        self.x += a;
                    } else {
                        self.y += a;
                    }
                    self.sink.line_to(self.x, self.y);
                    horiz = !horiz;
                }
            }
            op_code::RRCURVETO => {
                let args = core::mem::take(&mut self.stack);
                let mut i = 0;
                while i + 5 < args.len() {
                    let c1x = self.x + args[i];
                    let c1y = self.y + args[i + 1];
                    let c2x = c1x + args[i + 2];
                    let c2y = c1y + args[i + 3];
                    let x = c2x + args[i + 4];
                    let y = c2y + args[i + 5];
                    self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
                    self.x = x;
                    self.y = y;
                    i += 6;
                }
            }
            op_code::HHCURVETO => {
                let args = core::mem::take(&mut self.stack);
                let mut i = 0;
                let extra_y = args.len() % 4 == 1;
                let dy_start = if extra_y { args[0] } else { 0.0 };
                if extra_y {
                    i = 1;
                }
                let mut y_start = self.y + dy_start;
                while i + 3 < args.len() {
                    let c1x = self.x + args[i];
                    let c1y = y_start;
                    let c2x = c1x + args[i + 1];
                    let c2y = c1y + args[i + 2];
                    let x = c2x + args[i + 3];
                    let y = c2y;
                    self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
                    self.x = x;
                    self.y = y;
                    y_start = self.y;
                    i += 4;
                }
            }
            op_code::VVCURVETO => {
                let args = core::mem::take(&mut self.stack);
                let mut i = 0;
                let extra_x = args.len() % 4 == 1;
                let dx_start = if extra_x { args[0] } else { 0.0 };
                if extra_x {
                    i = 1;
                }
                let mut x_start = self.x + dx_start;
                while i + 3 < args.len() {
                    let c1x = x_start;
                    let c1y = self.y + args[i];
                    let c2x = c1x + args[i + 1];
                    let c2y = c1y + args[i + 2];
                    let x = c2x;
                    let y = c2y + args[i + 3];
                    self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
                    self.x = x;
                    self.y = y;
                    x_start = self.x;
                    i += 4;
                }
            }
            op_code::HVCURVETO => {
                let args = core::mem::take(&mut self.stack);
                self.alternating_curveto(&args, true)?;
            }
            op_code::VHCURVETO => {
                let args = core::mem::take(&mut self.stack);
                self.alternating_curveto(&args, false)?;
            }
            op_code::RCURVELINE => {
                let args = core::mem::take(&mut self.stack);
                // All but last two are curve triples (6 args each);
                // final two are an rlineto.
                let mut i = 0;
                while i + 7 < args.len() {
                    let c1x = self.x + args[i];
                    let c1y = self.y + args[i + 1];
                    let c2x = c1x + args[i + 2];
                    let c2y = c1y + args[i + 3];
                    let x = c2x + args[i + 4];
                    let y = c2y + args[i + 5];
                    self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
                    self.x = x;
                    self.y = y;
                    i += 6;
                }
                if i + 1 < args.len() {
                    self.x += args[i];
                    self.y += args[i + 1];
                    self.sink.line_to(self.x, self.y);
                }
            }
            op_code::RLINECURVE => {
                let args = core::mem::take(&mut self.stack);
                // All but last six are line pairs; final six are an rrcurveto.
                let mut i = 0;
                while i + 7 < args.len() {
                    self.x += args[i];
                    self.y += args[i + 1];
                    self.sink.line_to(self.x, self.y);
                    i += 2;
                }
                if i + 5 < args.len() {
                    let c1x = self.x + args[i];
                    let c1y = self.y + args[i + 1];
                    let c2x = c1x + args[i + 2];
                    let c2y = c1y + args[i + 3];
                    let x = c2x + args[i + 4];
                    let y = c2y + args[i + 5];
                    self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
                    self.x = x;
                    self.y = y;
                }
            }
            op_code::CALLSUBR => {
                let idx = self.pop()?;
                let i = biased_subr(&self.local, idx).ok_or(Error::Malformed {
                    offset: 0,
                    context: "CFF callsubr out of range",
                })?;
                let subr = self.local.get(i)?;
                self.run(subr, depth + 1)?;
            }
            op_code::CALLGSUBR => {
                let idx = self.pop()?;
                let i = biased_subr(&self.global, idx).ok_or(Error::Malformed {
                    offset: 0,
                    context: "CFF callgsubr out of range",
                })?;
                let subr = self.global.get(i)?;
                self.run(subr, depth + 1)?;
            }
            op_code::RETURN => {
                return Ok(());
            }
            op_code::ENDCHAR => {
                if !self.is_cff2 {
                    self.maybe_consume_width();
                    // Type 2 keeps Type 1's seac as an endchar with four
                    // operands, `adx ady bchar achar`. When endchar is
                    // the first stack-clearing operator a width may sit
                    // below them, which makes five. Like HarfBuzz and
                    // FreeType, take the top four whenever there are at
                    // least four.
                    if self.stack.len() >= 4 {
                        self.seac = Some(self.read_seac(r.position())?);
                    }
                }
                self.close_contour();
                self.done = true;
                self.stack.clear();
            }
            op_code::VSINDEX => {
                if self.is_cff2 {
                    let idx = self.pop()?;
                    if let Some(ref mut b) = self.blend {
                        b.vsindex = idx as u16;
                    }
                }
            }
            op_code::BLEND => {
                if self.is_cff2 {
                    self.apply_blend()?;
                }
            }
            op_code::ESCAPE => {
                let b1 = r.read_u8()?;
                match b1 {
                    op_code::ESC_HFLEX
                    | op_code::ESC_FLEX
                    | op_code::ESC_HFLEX1
                    | op_code::ESC_FLEX1 => {
                        // Approximate flex as two curves. For parity
                        // with ttf-parser the exact flex expansion
                        // matters: sigilbuzz emits two rrcurvetos
                        // from the 7/11/9/11 args respectively.
                        self.flex(b1)?;
                    }
                    // `dotsection`, a Type 1 hint that Type 2 keeps
                    // only as a deprecated no-op. Fonts converted from
                    // Type 1 still carry it. HarfBuzz and FreeType
                    // ignore it: they clear the operand stack and take
                    // no width from it.
                    op_code::ESC_DOTSECTION => {
                        self.stack.clear();
                    }
                    // Other Type 1 deprecated ops: reject.
                    3 | 4 | 5 | 7 | 8 | 13 | 14 | 15 | 16 | 17 | 21 | 32 | 33 => {
                        return Err(Error::Unsupported {
                            context: "CFF deprecated Type 1 operator",
                        });
                    }
                    // Arithmetic / logic ops: not needed for
                    // outline extraction but tolerated by clearing
                    // the stack; sigilbuzz isn't a CharString VM.
                    _ => {
                        self.stack.clear();
                    }
                }
            }
            _ => {
                return Err(Error::Malformed {
                    offset: 0,
                    context: "CFF unknown operator",
                });
            }
        }
        Ok(())
    }

    /// Reads the seac operands, the top four on the stack. `at` is the
    /// charstring position of the `endchar`, for errors.
    fn read_seac(&self, at: usize) -> Result<Seac> {
        if self.seac_component {
            return Err(Error::Malformed {
                offset: at,
                context: "CFF seac base or accent uses seac",
            });
        }
        let n = self.stack.len();
        let sid = |code: f32| {
            standard_encoding_sid(code).ok_or(Error::Malformed {
                offset: at,
                context: "CFF seac code not in the Standard Encoding",
            })
        };
        Ok(Seac {
            adx: self.stack[n - 4],
            ady: self.stack[n - 3],
            base: sid(self.stack[n - 2])?,
            accent: sid(self.stack[n - 1])?,
        })
    }

    fn alternating_curveto(&mut self, args: &[f32], start_horiz: bool) -> Result<()> {
        // HVCURVETO (start_horiz=true) and VHCURVETO alternate the
        // starting tangent direction per 4-arg group. Each group
        // lays out (d1, d2, d3, d4), with an optional trailing d5
        // added to the off-axis coord on the last group.
        let mut i = 0;
        let mut horiz = start_horiz;
        while i + 3 < args.len() {
            let remaining = args.len() - i;
            let final_group = remaining < 8;
            let has_extra = final_group && remaining == 5;
            let (c1, c2, c3, c4) = (args[i], args[i + 1], args[i + 2], args[i + 3]);
            let extra = if has_extra { args[i + 4] } else { 0.0 };

            let (c1x, c1y) = if horiz {
                (self.x + c1, self.y)
            } else {
                (self.x, self.y + c1)
            };
            let c2x = c1x + c2;
            let c2y = c1y + c3;
            let (x, y) = if horiz {
                (c2x + extra, c2y + c4)
            } else {
                (c2x + c4, c2y + extra)
            };
            self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
            self.x = x;
            self.y = y;
            horiz = !horiz;
            i += if has_extra { 5 } else { 4 };
        }
        Ok(())
    }

    // Each flex variant keeps one arm with its arity check inside,
    // so the four variants stay parallel.
    #[allow(clippy::collapsible_match)]
    fn flex(&mut self, esc: u8) -> Result<()> {
        // Flex expands to two rrcurvetos. For outline extraction we
        // emit the two cubics directly; flex-specific depth / height
        // hints are rendering concerns we don't model.
        match esc {
            op_code::ESC_FLEX => {
                // 12 35: 13 args total (6 + 6 + flex depth).
                if self.stack.len() >= 13 {
                    let a = core::mem::take(&mut self.stack);
                    self.rr_curve(&a[..6]);
                    self.rr_curve(&a[6..12]);
                }
            }
            op_code::ESC_HFLEX => {
                // 12 34: 7 args. First curve has dy=0, second has
                // ending dy=0 and reflects dy pattern.
                if self.stack.len() >= 7 {
                    let a = core::mem::take(&mut self.stack);
                    let c1 = [a[0], 0.0, a[1], a[2], a[3], 0.0];
                    let c2 = [a[4], 0.0, a[5], -a[2], a[6], 0.0];
                    self.rr_curve(&c1);
                    self.rr_curve(&c2);
                }
            }
            op_code::ESC_HFLEX1 => {
                // 12 36: 9 args `dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6`.
                // The flex starts AND ends at the same y value, so the
                // implicit dy6 must cancel the accumulated y delta:
                // dy1 + dy2 + dy3(=0) + dy4(=0) + dy5 + dy6 = 0, hence
                // dy6 = -(dy1 + dy2 + dy5) = -(a[1] + a[3] + a[7]).
                if self.stack.len() >= 9 {
                    let a = core::mem::take(&mut self.stack);
                    let dy_total = a[1] + a[3] + a[7];
                    let c1 = [a[0], a[1], a[2], a[3], a[4], 0.0];
                    let c2 = [a[5], 0.0, a[6], a[7], a[8], -dy_total];
                    self.rr_curve(&c1);
                    self.rr_curve(&c2);
                }
            }
            op_code::ESC_FLEX1 => {
                // 12 37: 11 args. Last endpoint on the dominant axis.
                if self.stack.len() >= 11 {
                    let a = core::mem::take(&mut self.stack);
                    let dx_total = a[0] + a[2] + a[4] + a[6] + a[8];
                    let dy_total = a[1] + a[3] + a[5] + a[7] + a[9];
                    let (dx_final, dy_final) = if abs_f32(dx_total) > abs_f32(dy_total) {
                        (a[10], -dy_total)
                    } else {
                        (-dx_total, a[10])
                    };
                    let c1 = [a[0], a[1], a[2], a[3], a[4], a[5]];
                    let c2 = [a[6], a[7], a[8], a[9], dx_final, dy_final];
                    self.rr_curve(&c1);
                    self.rr_curve(&c2);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn rr_curve(&mut self, a: &[f32]) {
        let c1x = self.x + a[0];
        let c1y = self.y + a[1];
        let c2x = c1x + a[2];
        let c2y = c1y + a[3];
        let x = c2x + a[4];
        let y = c2y + a[5];
        self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
        self.x = x;
        self.y = y;
    }

    // The blend loops index the stack rows and the scalars in lockstep,
    // which reads more clearly with explicit indices.
    #[allow(clippy::needless_range_loop)]
    fn apply_blend(&mut self) -> Result<()> {
        // Stack layout: n default values, followed by n*nRegions
        // delta values, followed by the count `n`. `nRegions` is
        // fixed by the IVS subtable at the current vsindex. Without
        // a BlendContext we infer `nRegions` from the surplus stack
        // depth. That is only correct when the font's charstring
        // and our best-effort default agree, which is enough to
        // keep the interpreter balanced so parsing continues past
        // BLEND.
        let n_raw = self.pop()?;
        let n = n_raw as usize;
        if n == 0 {
            return Ok(());
        }
        let underflow = Error::Malformed {
            offset: 0,
            context: "CFF2 blend: stack underflow",
        };
        // `n` comes from a float operand and can be huge. The stack
        // must hold at least `n` values, which keeps the products
        // below from overflowing.
        if n > self.stack.len() {
            return Err(underflow);
        }
        self.refresh_blend_regions();
        // Set exactly when a BlendContext exists.
        let regions = self.blend_regions.as_ref();
        let n_regions = regions.and_then(|r| r.count).map_or_else(
            || {
                let extra = self.stack.len().saturating_sub(n);
                extra / n
            },
            usize::from,
        );
        let total_deltas = n * n_regions;
        if self.stack.len() < n + total_deltas {
            return Err(underflow);
        }
        let start = self.stack.len() - n - total_deltas;
        // Default value `i` sits below every delta row, so each row is
        // summed and added in place. The sums and the order of the
        // additions are the same as summing every row first.
        for i in 0..n {
            let mut accum = 0.0_f32;
            if let Some(r) = regions {
                for j in 0..n_regions {
                    let d = self.stack[start + n + i * n_regions + j];
                    if let Some(&s) = r.scalars.get(j) {
                        accum += s * d;
                    }
                }
            }
            self.stack[start + i] += accum;
        }
        self.stack.truncate(start + n);
        Ok(())
    }

    /// Fills [`Self::blend_regions`] for the blend context's current
    /// `vsindex`, unless it already holds that `vsindex`. Does nothing
    /// without a blend context.
    fn refresh_blend_regions(&mut self) {
        let Some(b) = self.blend.as_ref() else {
            return;
        };
        if self
            .blend_regions
            .as_ref()
            .is_some_and(|r| r.vsindex == b.vsindex)
        {
            return;
        }
        self.blend_regions = Some(BlendRegions {
            vsindex: b.vsindex,
            count: b.ivs.variation_region_count(b.vsindex),
            scalars: b
                .ivs
                .region_scalars(b.vsindex, b.coords)
                .unwrap_or_default(),
        });
    }

    fn push(&mut self, v: f32) -> Result<()> {
        let limit = if self.is_cff2 {
            CFF2_STACK_LIMIT
        } else {
            CFF1_STACK_LIMIT
        };
        if self.stack.len() >= limit {
            return Err(Error::Malformed {
                offset: 0,
                context: "CFF charstring: operand stack overflow",
            });
        }
        self.stack.push(v);
        Ok(())
    }

    fn pop(&mut self) -> Result<f32> {
        self.stack.pop().ok_or(Error::Malformed {
            offset: 0,
            context: "CFF charstring: stack underflow",
        })
    }

    fn maybe_consume_width(&mut self) {
        if !self.consumed_width {
            self.consumed_width = true;
            // Width is the first operand on the stack for the initial
            // hint or move operator when the stack size is odd for
            // hints or > expected for moves. Simplest: if the stack
            // carries one more operand than the operator needs, the
            // leading one is the width. We ignore it.
            // The conservative approach (HarfBuzz's): drop the lowest
            // operand when the top operator is a move and the stack
            // has an odd count > needed. Rather than re-parse, we
            // just flag consumed_width; concrete handlers consume
            // their expected operands via `pop`, leaving any leading
            // width harmlessly on the stack where `stack.clear()`
            // discards it at the end of the op.
        }
    }

    fn close_contour(&mut self) {
        if self.in_contour {
            self.sink.close();
            self.in_contour = false;
        }
    }

    /// Closes the contour still open when the charstring runs out.
    ///
    /// CFF1 closes its last contour at `endchar`, but CFF2 has no
    /// `endchar`: a CFF2 charstring simply ends, and without this call
    /// its final contour would stay open. Call it once after
    /// [`Self::run`] returns. It is idempotent, so a CFF1 charstring
    /// that already ended with `endchar` is unaffected.
    pub(crate) fn finish(&mut self) {
        self.close_contour();
    }
}

/// Thin CFF2 wrapper around [`Interp`] with blend context wired in
/// and `is_cff2 = true`. Sharing the same interpreter keeps the
/// charstring op table in one place.
pub(crate) struct Interp2<'a, 'b, S: OutlineSink> {
    inner: Interp<'a, 'b, S>,
}

impl<'a, 'b, S: OutlineSink> Interp2<'a, 'b, S> {
    pub(crate) fn new(
        global: Index<'a>,
        local: Index<'a>,
        sink: &'b mut S,
        blend: Option<BlendContext<'b>>,
    ) -> Self {
        let mut inner = Interp::new(global, local, sink, true);
        inner.blend = blend;
        Self { inner }
    }

    pub(crate) fn run(&mut self, code: &'a [u8], depth: u8) -> Result<()> {
        self.inner.run(code, depth)
    }

    /// Closes the final contour. See [`Interp::finish`].
    pub(crate) fn finish(&mut self) {
        self.inner.finish();
    }
}

pub(super) fn subr_bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33_900 {
        1131
    } else {
        32_768
    }
}

/// Resolves a biased subroutine number popped off the operand stack
/// to an index into `subrs`. Blended CFF2 operands can hold any
/// float, so the sum is checked and negative or huge indices resolve
/// to `None`.
fn biased_subr(subrs: &Index<'_>, idx: f32) -> Option<usize> {
    let i = (idx as i32).checked_add(subr_bias(subrs.len()))?;
    let i = usize::try_from(i).ok()?;
    (i < subrs.len()).then_some(i)
}
