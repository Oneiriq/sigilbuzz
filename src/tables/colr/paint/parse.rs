//! Decoding of COLR v1 `Paint` records into `ColrPaint` nodes.

use alloc::vec::Vec;

use super::{ColorLine, ColrPaint, CompositeMode, Extend, F2Dot14};
use crate::error::{Error, Result};
use crate::tables::colr::PaintOffset;

impl<'a> ColrPaint<'a> {
    /// Parses the `Paint` at `offset` inside `data` (the full COLR
    /// blob). The `offset` is absolute; sub-paints stored as 24-bit
    /// offsets inside a parent format are translated to absolute
    /// values before the variant is returned, so traversal is a
    /// straight chain of [`Colr::paint_at`](crate::tables::colr::Colr::paint_at) calls.
    //
    // The paint tree spans 32 distinct formats so the match is
    // necessarily long. Splitting per-format would hurt locality more
    // than it would help readability, so keep the big `match` intact.
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    pub fn parse(data: &'a [u8], offset: usize) -> Result<Self> {
        if offset >= data.len() {
            return Err(Error::Truncated {
                offset,
                context: "Paint offset past end",
            });
        }
        let format = data[offset];
        let base = offset;
        // Helper closure: read a u24 child-paint offset at (base + k)
        // and return the absolute offset.
        let read_off24 = |k: usize| -> Result<u32> {
            if base + k + 3 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "Paint sub-offset truncated",
                });
            }
            let rel =
                u32::from_be_bytes([0, data[base + k], data[base + k + 1], data[base + k + 2]]);
            (base as u32).checked_add(rel).ok_or(Error::Malformed {
                offset: base + k,
                context: "Paint sub-offset overflow",
            })
        };
        let read_u8 = |k: usize| -> Result<u8> {
            data.get(base + k).copied().ok_or(Error::Truncated {
                offset: base + k,
                context: "u8",
            })
        };
        let read_u16 = |k: usize| -> Result<u16> {
            if base + k + 2 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "u16",
                });
            }
            Ok(u16::from_be_bytes([data[base + k], data[base + k + 1]]))
        };
        let read_i16 = |k: usize| -> Result<i16> {
            if base + k + 2 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "i16",
                });
            }
            Ok(i16::from_be_bytes([data[base + k], data[base + k + 1]]))
        };
        let read_u32 = |k: usize| -> Result<u32> {
            if base + k + 4 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "u32",
                });
            }
            Ok(u32::from_be_bytes([
                data[base + k],
                data[base + k + 1],
                data[base + k + 2],
                data[base + k + 3],
            ]))
        };
        let read_f2dot14 = |k: usize| -> Result<F2Dot14> { Ok(read_i16(k)? as f32 / 16384.0) };
        // ColorLine / VarColorLine layout:
        //   u8 extend
        //   u16 numStops
        //   ColorStop[numStops]  (6 bytes each for ColorLine,
        //                         10 bytes for VarColorLine)
        let read_color_line = |k: usize, variable: bool| -> Result<(ColorLine<'a>, usize)> {
            // The gradient formats store the color line as a
            // u24 sub-offset (relative to the paint record); we
            // resolve it here so callers see a straight
            // `ColorLine<'a>`.
            if base + k + 3 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "ColorLine offset truncated",
                });
            }
            let rel =
                u32::from_be_bytes([0, data[base + k], data[base + k + 1], data[base + k + 2]])
                    as usize;
            let cl_start = base + rel;
            if cl_start + 3 > data.len() {
                return Err(Error::Truncated {
                    offset: cl_start,
                    context: "ColorLine body truncated",
                });
            }
            let extend = Extend::from_u8(data[cl_start]);
            let num_stops = u16::from_be_bytes([data[cl_start + 1], data[cl_start + 2]]);
            let stop_size = if variable { 10 } else { 6 };
            let stops_start = cl_start + 3;
            let stops_end = stops_start + num_stops as usize * stop_size;
            if stops_end > data.len() {
                return Err(Error::Truncated {
                    offset: stops_end,
                    context: "ColorLine stops truncated",
                });
            }
            Ok((
                ColorLine {
                    extend,
                    stops: &data[stops_start..stops_end],
                    stop_count: num_stops,
                    variable,
                },
                3, // consumed bytes at `k`
            ))
        };

        Ok(match format {
            // 1. PaintColrLayers: { u8 format; u8 numLayers; u32 firstLayerIndex }
            1 => Self::ColrLayers {
                num_layers: read_u8(1)?,
                first_layer_index: read_u32(2)?,
            },
            // 2. PaintSolid: { u8 format; u16 paletteIndex; F2Dot14 alpha }
            2 => Self::Solid {
                palette_index: read_u16(1)?,
                alpha: read_f2dot14(3)?,
            },
            // 3. PaintVarSolid: + u32 varIndexBase
            3 => Self::VarSolid {
                palette_index: read_u16(1)?,
                alpha: read_f2dot14(3)?,
                var_index_base: read_u32(5)?,
            },
            // 4. PaintLinearGradient:
            //    { u8 format; Offset24 colorLine; FWORD x0..y2 (6 words) }
            4 => {
                let (color_line, _) = read_color_line(1, false)?;
                Self::LinearGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    x1: read_i16(8)?,
                    y1: read_i16(10)?,
                    x2: read_i16(12)?,
                    y2: read_i16(14)?,
                }
            }
            // 5. PaintVarLinearGradient: + varIndexBase
            5 => {
                let (color_line, _) = read_color_line(1, true)?;
                Self::VarLinearGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    x1: read_i16(8)?,
                    y1: read_i16(10)?,
                    x2: read_i16(12)?,
                    y2: read_i16(14)?,
                    var_index_base: read_u32(16)?,
                }
            }
            // 6. PaintRadialGradient: colorLine + (x0,y0,r0,x1,y1,r1) all i16/u16
            6 => {
                let (color_line, _) = read_color_line(1, false)?;
                Self::RadialGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    r0: read_u16(8)?,
                    x1: read_i16(10)?,
                    y1: read_i16(12)?,
                    r1: read_u16(14)?,
                }
            }
            // 7. PaintVarRadialGradient
            7 => {
                let (color_line, _) = read_color_line(1, true)?;
                Self::VarRadialGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    r0: read_u16(8)?,
                    x1: read_i16(10)?,
                    y1: read_i16(12)?,
                    r1: read_u16(14)?,
                    var_index_base: read_u32(16)?,
                }
            }
            // 8. PaintSweepGradient: colorLine + centerX + centerY + startAngle + endAngle
            8 => {
                let (color_line, _) = read_color_line(1, false)?;
                Self::SweepGradient {
                    color_line,
                    center_x: read_i16(4)?,
                    center_y: read_i16(6)?,
                    start_angle: read_f2dot14(8)?,
                    end_angle: read_f2dot14(10)?,
                }
            }
            // 9. PaintVarSweepGradient
            9 => {
                let (color_line, _) = read_color_line(1, true)?;
                Self::VarSweepGradient {
                    color_line,
                    center_x: read_i16(4)?,
                    center_y: read_i16(6)?,
                    start_angle: read_f2dot14(8)?,
                    end_angle: read_f2dot14(10)?,
                    var_index_base: read_u32(12)?,
                }
            }
            // 10. PaintGlyph: { u8 format; Offset24 paint; u16 glyphID }
            10 => Self::Glyph {
                paint_offset: read_off24(1)?,
                glyph_id: read_u16(4)?,
            },
            // 11. PaintColrGlyph: { u8 format; u16 glyphID }
            11 => Self::ColrGlyph {
                glyph_id: read_u16(1)?,
            },
            // 12. PaintTransform: Offset24 paint + Offset24 affine
            12 => {
                // The 2x3 Affine2x3 table referenced by the sub-offset
                // has layout: Fixed xx, yx, xy, yy, dx, dy (6 * 4 bytes).
                let paint_offset = read_off24(1)?;
                if base + 4 + 3 > data.len() {
                    return Err(Error::Truncated {
                        offset: base + 4,
                        context: "PaintTransform affine offset truncated",
                    });
                }
                let affine_rel =
                    u32::from_be_bytes([0, data[base + 4], data[base + 5], data[base + 6]])
                        as usize;
                let a_start = base + affine_rel;
                if a_start + 24 > data.len() {
                    return Err(Error::Truncated {
                        offset: a_start,
                        context: "Affine2x3 body truncated",
                    });
                }
                let read_aff = |o: usize| -> f32 {
                    let raw = i32::from_be_bytes([
                        data[a_start + o],
                        data[a_start + o + 1],
                        data[a_start + o + 2],
                        data[a_start + o + 3],
                    ]);
                    raw as f32 / 65536.0
                };
                Self::Transform {
                    paint_offset,
                    xx: read_aff(0),
                    yx: read_aff(4),
                    xy: read_aff(8),
                    yy: read_aff(12),
                    dx: read_aff(16),
                    dy: read_aff(20),
                }
            }
            // 13. PaintVarTransform: same, VarAffine2x3 includes varIndexBase
            13 => {
                let paint_offset = read_off24(1)?;
                if base + 7 > data.len() {
                    return Err(Error::Truncated {
                        offset: base + 4,
                        context: "PaintVarTransform affine offset truncated",
                    });
                }
                let affine_rel =
                    u32::from_be_bytes([0, data[base + 4], data[base + 5], data[base + 6]])
                        as usize;
                let a_start = base + affine_rel;
                if a_start + 28 > data.len() {
                    return Err(Error::Truncated {
                        offset: a_start,
                        context: "VarAffine2x3 body truncated",
                    });
                }
                let read_aff = |o: usize| -> f32 {
                    let raw = i32::from_be_bytes([
                        data[a_start + o],
                        data[a_start + o + 1],
                        data[a_start + o + 2],
                        data[a_start + o + 3],
                    ]);
                    raw as f32 / 65536.0
                };
                let var_index_base = u32::from_be_bytes([
                    data[a_start + 24],
                    data[a_start + 25],
                    data[a_start + 26],
                    data[a_start + 27],
                ]);
                Self::VarTransform {
                    paint_offset,
                    xx: read_aff(0),
                    yx: read_aff(4),
                    xy: read_aff(8),
                    yy: read_aff(12),
                    dx: read_aff(16),
                    dy: read_aff(20),
                    var_index_base,
                }
            }
            // 14. PaintTranslate: Offset24 paint + dx + dy (i16)
            14 => Self::Translate {
                paint_offset: read_off24(1)?,
                dx: read_i16(4)?,
                dy: read_i16(6)?,
            },
            15 => Self::VarTranslate {
                paint_offset: read_off24(1)?,
                dx: read_i16(4)?,
                dy: read_i16(6)?,
                var_index_base: read_u32(8)?,
            },
            16 => Self::Scale {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
            },
            17 => Self::VarScale {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
                var_index_base: read_u32(8)?,
            },
            18 => Self::ScaleAroundCenter {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
            },
            19 => Self::VarScaleAroundCenter {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
                var_index_base: read_u32(12)?,
            },
            20 => Self::ScaleUniform {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
            },
            21 => Self::VarScaleUniform {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
                var_index_base: read_u32(6)?,
            },
            22 => Self::ScaleUniformAroundCenter {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
            },
            23 => Self::VarScaleUniformAroundCenter {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
                var_index_base: read_u32(10)?,
            },
            24 => Self::Rotate {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
            },
            25 => Self::VarRotate {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
                var_index_base: read_u32(6)?,
            },
            26 => Self::RotateAroundCenter {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
            },
            27 => Self::VarRotateAroundCenter {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
                var_index_base: read_u32(10)?,
            },
            28 => Self::Skew {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
            },
            29 => Self::VarSkew {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
                var_index_base: read_u32(8)?,
            },
            30 => Self::SkewAroundCenter {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
            },
            31 => Self::VarSkewAroundCenter {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
                var_index_base: read_u32(12)?,
            },
            32 => Self::Composite {
                source_paint_offset: read_off24(1)?,
                composite_mode: CompositeMode::from_u8(read_u8(4)?),
                backdrop_paint_offset: read_off24(5)?,
            },
            _ => {
                return Err(Error::Unsupported {
                    context: "unknown COLR v1 paint format",
                });
            }
        })
    }

    /// Returns every absolute child-paint offset this node carries.
    /// Callers drive a depth-first traversal by feeding these back
    /// into [`Colr::paint_at`](crate::tables::colr::Colr::paint_at).
    #[must_use]
    pub fn child_paint_offsets(&self) -> Vec<PaintOffset> {
        match *self {
            Self::Glyph { paint_offset, .. }
            | Self::Transform { paint_offset, .. }
            | Self::VarTransform { paint_offset, .. }
            | Self::Translate { paint_offset, .. }
            | Self::VarTranslate { paint_offset, .. }
            | Self::Scale { paint_offset, .. }
            | Self::VarScale { paint_offset, .. }
            | Self::ScaleAroundCenter { paint_offset, .. }
            | Self::VarScaleAroundCenter { paint_offset, .. }
            | Self::ScaleUniform { paint_offset, .. }
            | Self::VarScaleUniform { paint_offset, .. }
            | Self::ScaleUniformAroundCenter { paint_offset, .. }
            | Self::VarScaleUniformAroundCenter { paint_offset, .. }
            | Self::Rotate { paint_offset, .. }
            | Self::VarRotate { paint_offset, .. }
            | Self::RotateAroundCenter { paint_offset, .. }
            | Self::VarRotateAroundCenter { paint_offset, .. }
            | Self::Skew { paint_offset, .. }
            | Self::VarSkew { paint_offset, .. }
            | Self::SkewAroundCenter { paint_offset, .. }
            | Self::VarSkewAroundCenter { paint_offset, .. } => alloc::vec![paint_offset],
            Self::Composite {
                source_paint_offset,
                backdrop_paint_offset,
                ..
            } => alloc::vec![source_paint_offset, backdrop_paint_offset],
            _ => Vec::new(),
        }
    }
}
