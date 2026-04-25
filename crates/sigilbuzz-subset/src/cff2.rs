//! CFF2 subsetting helpers.
//!
//! CFF2 is CFF1 trimmed: no Name INDEX, no String INDEX, no Encoding,
//! no charset, no `endchar` operator. Only one Top DICT, stored
//! directly (no enclosing INDEX). VariationStore is optional.
//!
//! The Type 2 charstring scanner from [`crate::cff`] already accepts
//! both flavours — it stops at `OP_RETURN` / `OP_ENDCHAR` /
//! end-of-stream, and recognises `vsindex` / `blend` so CFF2-specific
//! ops don't confuse the operand-stack tracking. The byte-level emitter
//! primitives ([`crate::cff::encode_index`],
//! [`crate::cff::encode_dict_int`], [`crate::cff::renumber_charstring`])
//! are shared with CFF1 — CFF2's smaller surface (no Name / String /
//! Encoding / charset INDEXes) means the eventual emitter is a strict
//! subset of the CFF1 layout pass.
//!
//! # Status
//!
//! CFF2 sources are routed through the identity-passthrough path in
//! [`crate::subset`] when the closure walker has not dropped any glyph
//! — the CFF2 table including its VariationStore, FDArray, FDSelect,
//! and inline Top DICT is preserved verbatim. Non-identity CFF2
//! subsetting still surfaces [`SubsetError::Unsupported`] — the
//! orchestration mirrors CFF1 (smaller surface: no String INDEX, no
//! Encoding, no charset, single inline Top DICT) but its FDArray
//! INDEX rebuild + FDSelect rewrite is shared with CID-keyed CFF1 and
//! is staged for the same follow-up.

// The CFF1 charstring scanner already accepts CFF2 inputs (no endchar
// terminator, vsindex / blend recognised). The CFF2 emitter, when it
// lands, calls those helpers directly via the `crate::cff::` path —
// no extra surface lives here today.
#[cfg(test)]
use crate::cff::{encode_int_operand, scan_subr_calls, subr_bias};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cff2_charstring_without_endchar_is_walked_to_eof() {
        // CFF2 charstrings have no terminating endchar — the
        // scanner must complete on end-of-stream. Charstring: push 0,
        // callsubr (resolves to local subr 0 with default bias 107).
        let cs = [139u8, 10 /* OP_CALLSUBR */];
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].index_after_bias, 107);
    }

    #[test]
    fn cff2_blend_and_vsindex_dont_break_scanner() {
        // 0 0 rmoveto vsindex blend: the scanner clears the stack
        // and steps past these ops. Then 0 callsubr.
        let cs = [
            139, 139, 21, // rmoveto
            139, 15, // vsindex (consumes one)
            139, 16, // blend
            139, 10, // callsubr
        ];
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn analysis_helpers_reach_cff2_module() {
        // CFF2 inherits the bias / scanner / encoder helpers from
        // crate::cff. Smoke-check the re-exports compile and round
        // trip an integer operand encode through the shared encoder.
        assert_eq!(subr_bias(0), 107);
        let _enc = encode_int_operand(50);
    }
}
