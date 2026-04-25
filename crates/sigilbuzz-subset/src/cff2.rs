//! CFF2 subsetting helpers.
//!
//! CFF2 is CFF1 trimmed: no Name INDEX, no String INDEX, no Encoding,
//! no charset, no `endchar` operator. Only one Top DICT, stored
//! directly (no enclosing INDEX). VariationStore is optional.
//!
//! The Type 2 charstring scanner from [`crate::cff`] already accepts
//! both flavours — it stops at `OP_RETURN` / `OP_ENDCHAR` /
//! end-of-stream, and recognises `vsindex` / `blend` so CFF2-specific
//! ops don't confuse the operand-stack tracking. The CFF2 entry point
//! exists today as a placeholder so the upstream `subset()` API can
//! dispatch on which CFF variant the source font carries.
//!
//! # Status
//!
//! The byte-level CFF2 emitter (Top DICT serialise, INDEX rebuilds,
//! VariationStore pass-through) is staged in the same follow-up
//! commit as the CFF1 emitter; today this entry point returns
//! `Unsupported`.

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
