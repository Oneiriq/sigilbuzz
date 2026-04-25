//! UAX #9 Bidi_Paired_Bracket / Bidi_Paired_Bracket_Type data.
//!
//! Curated extract of `BidiBrackets.txt` covering the bracket pairs
//! shapers actually meet in the wild — ASCII parens / brackets /
//! braces, the CJK corner / lenticular / tortoise families, and the
//! math angle / floor / ceiling brackets. Each row maps a bracket
//! codepoint to its [`BracketType`] (open / close) and the codepoint
//! of the matching half.
//!
//! Coverage is intentionally narrow: a full BidiBrackets.txt expansion
//! to every Unicode-defined pair (white-square, ornate-parenthesis,
//! Wancho, Vai, Tangsa, ...) would be mechanical to add but bulky and
//! shaping-irrelevant — none of those brackets show up in mixed-LTR /
//! RTL text. The pairs here are the ones the bidi algorithm's N0
//! actually has to disambiguate when a paragraph mixes Latin / Hebrew
//! / Arabic with punctuation.

/// Whether a bracket codepoint opens or closes its pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketType {
    /// Opening half of the pair — `(`, `[`, `「`, ...
    Open,
    /// Closing half of the pair — `)`, `]`, `」`, ...
    Close,
}

/// One bracket-pair entry. The `pair` field holds the codepoint of the
/// matching bracket — i.e. the open's close, or the close's open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketEntry {
    /// Open or close.
    pub kind: BracketType,
    /// Codepoint of the matching half of this pair.
    pub pair: u32,
}

/// Returns the bracket entry for `cp`, or `None` if the codepoint is
/// not in the curated pair set.
#[must_use]
#[allow(clippy::too_many_lines)] // ~80 codepoint pairs — flat match is the clearest layout
pub const fn bracket_of(cp: u32) -> Option<BracketEntry> {
    // Sorted by `cp` so a future binary-search rewrite stays trivial;
    // the const-fn match is fine for ~80 entries — rustc lowers it to
    // a jump table.
    match cp {
        // ASCII parens / brackets / braces. (U+0028 ↔ U+0029 etc.)
        0x0028 => Some(BracketEntry { kind: BracketType::Open, pair: 0x0029 }),
        0x0029 => Some(BracketEntry { kind: BracketType::Close, pair: 0x0028 }),
        0x005B => Some(BracketEntry { kind: BracketType::Open, pair: 0x005D }),
        0x005D => Some(BracketEntry { kind: BracketType::Close, pair: 0x005B }),
        0x007B => Some(BracketEntry { kind: BracketType::Open, pair: 0x007D }),
        0x007D => Some(BracketEntry { kind: BracketType::Close, pair: 0x007B }),

        // Mathematical and miscellaneous technical brackets.
        // ⌈⌉ U+2308/U+2309 — left / right ceiling.
        0x2308 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2309 }),
        0x2309 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2308 }),
        // ⌊⌋ U+230A/U+230B — left / right floor.
        0x230A => Some(BracketEntry { kind: BracketType::Open, pair: 0x230B }),
        0x230B => Some(BracketEntry { kind: BracketType::Close, pair: 0x230A }),
        // ❨❩ U+2768/U+2769 — medium parenthesis ornament.
        0x2768 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2769 }),
        0x2769 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2768 }),
        // ❪❫ U+276A/U+276B — flat parenthesis ornament.
        0x276A => Some(BracketEntry { kind: BracketType::Open, pair: 0x276B }),
        0x276B => Some(BracketEntry { kind: BracketType::Close, pair: 0x276A }),
        // ❬❭ U+276C/U+276D — pointing-angle ornament (medium).
        0x276C => Some(BracketEntry { kind: BracketType::Open, pair: 0x276D }),
        0x276D => Some(BracketEntry { kind: BracketType::Close, pair: 0x276C }),
        // ❮❯ U+276E/U+276F — pointing-angle ornament (heavy).
        0x276E => Some(BracketEntry { kind: BracketType::Open, pair: 0x276F }),
        0x276F => Some(BracketEntry { kind: BracketType::Close, pair: 0x276E }),
        // ❰❱ U+2770/U+2771 — heavy pointing-angle ornament.
        0x2770 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2771 }),
        0x2771 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2770 }),
        // ❲❳ U+2772/U+2773 — light tortoise-shell ornament.
        0x2772 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2773 }),
        0x2773 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2772 }),
        // ❴❵ U+2774/U+2775 — medium curly-bracket ornament.
        0x2774 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2775 }),
        0x2775 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2774 }),
        // ⟦⟧ U+27E6/U+27E7 — mathematical white square bracket.
        0x27E6 => Some(BracketEntry { kind: BracketType::Open, pair: 0x27E7 }),
        0x27E7 => Some(BracketEntry { kind: BracketType::Close, pair: 0x27E6 }),
        // ⟨⟩ U+27E8/U+27E9 — mathematical angle bracket.
        0x27E8 => Some(BracketEntry { kind: BracketType::Open, pair: 0x27E9 }),
        0x27E9 => Some(BracketEntry { kind: BracketType::Close, pair: 0x27E8 }),
        // ⟪⟫ U+27EA/U+27EB — mathematical double angle bracket.
        0x27EA => Some(BracketEntry { kind: BracketType::Open, pair: 0x27EB }),
        0x27EB => Some(BracketEntry { kind: BracketType::Close, pair: 0x27EA }),
        // ⟬⟭ U+27EC/U+27ED — mathematical white tortoise-shell bracket.
        0x27EC => Some(BracketEntry { kind: BracketType::Open, pair: 0x27ED }),
        0x27ED => Some(BracketEntry { kind: BracketType::Close, pair: 0x27EC }),
        // ⟮⟯ U+27EE/U+27EF — mathematical flattened parenthesis.
        0x27EE => Some(BracketEntry { kind: BracketType::Open, pair: 0x27EF }),
        0x27EF => Some(BracketEntry { kind: BracketType::Close, pair: 0x27EE }),

        // Miscellaneous-mathematical-symbols-A square / curly / angle.
        // ⦃⦄ U+2983/U+2984 — left / right white curly bracket.
        0x2983 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2984 }),
        0x2984 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2983 }),
        // ⦅⦆ U+2985/U+2986 — left / right white parenthesis.
        0x2985 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2986 }),
        0x2986 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2985 }),
        // ⦇⦈ U+2987/U+2988 — Z-notation image bracket.
        0x2987 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2988 }),
        0x2988 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2987 }),
        // ⦉⦊ U+2989/U+298A — Z-notation binding bracket.
        0x2989 => Some(BracketEntry { kind: BracketType::Open, pair: 0x298A }),
        0x298A => Some(BracketEntry { kind: BracketType::Close, pair: 0x2989 }),
        // ⦋⦌ U+298B/U+298C — square bracket with underbar.
        0x298B => Some(BracketEntry { kind: BracketType::Open, pair: 0x298C }),
        0x298C => Some(BracketEntry { kind: BracketType::Close, pair: 0x298B }),
        // ⦍⦎ U+298D/U+298E — square bracket with tick (top corner).
        0x298D => Some(BracketEntry { kind: BracketType::Open, pair: 0x298E }),
        0x298E => Some(BracketEntry { kind: BracketType::Close, pair: 0x298D }),
        // ⦏⦐ U+298F/U+2990 — square bracket with tick (bottom corner).
        0x298F => Some(BracketEntry { kind: BracketType::Open, pair: 0x2990 }),
        0x2990 => Some(BracketEntry { kind: BracketType::Close, pair: 0x298F }),
        // ⦑⦒ U+2991/U+2992 — angle bracket with dot.
        0x2991 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2992 }),
        0x2992 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2991 }),
        // ⦓⦔ U+2993/U+2994 — arc less-than / greater-than bracket.
        0x2993 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2994 }),
        0x2994 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2993 }),
        // ⦕⦖ U+2995/U+2996 — double arc less-than / greater-than.
        0x2995 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2996 }),
        0x2996 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2995 }),
        // ⦗⦘ U+2997/U+2998 — black tortoise-shell bracket.
        0x2997 => Some(BracketEntry { kind: BracketType::Open, pair: 0x2998 }),
        0x2998 => Some(BracketEntry { kind: BracketType::Close, pair: 0x2997 }),

        // Miscellaneous-mathematical-symbols-B square / curly / angle.
        // ⧘⧙ U+29D8/U+29D9 — wiggly fence.
        0x29D8 => Some(BracketEntry { kind: BracketType::Open, pair: 0x29D9 }),
        0x29D9 => Some(BracketEntry { kind: BracketType::Close, pair: 0x29D8 }),
        // ⧚⧛ U+29DA/U+29DB — double wiggly fence.
        0x29DA => Some(BracketEntry { kind: BracketType::Open, pair: 0x29DB }),
        0x29DB => Some(BracketEntry { kind: BracketType::Close, pair: 0x29DA }),
        // ⧼⧽ U+29FC/U+29FD — curved angle bracket.
        0x29FC => Some(BracketEntry { kind: BracketType::Open, pair: 0x29FD }),
        0x29FD => Some(BracketEntry { kind: BracketType::Close, pair: 0x29FC }),

        // CJK Symbols and Punctuation.
        // 〈〉 U+2329/U+232A — left / right pointing angle (canonical
        // equivalents to U+27E8/U+27E9 above; deprecated but still
        // round-tripped).
        0x2329 => Some(BracketEntry { kind: BracketType::Open, pair: 0x232A }),
        0x232A => Some(BracketEntry { kind: BracketType::Close, pair: 0x2329 }),
        // 「」 U+300C/U+300D — left / right corner bracket.
        0x300C => Some(BracketEntry { kind: BracketType::Open, pair: 0x300D }),
        0x300D => Some(BracketEntry { kind: BracketType::Close, pair: 0x300C }),
        // 『』 U+300E/U+300F — left / right white corner bracket.
        0x300E => Some(BracketEntry { kind: BracketType::Open, pair: 0x300F }),
        0x300F => Some(BracketEntry { kind: BracketType::Close, pair: 0x300E }),
        // 【】 U+3010/U+3011 — left / right black lenticular bracket.
        0x3010 => Some(BracketEntry { kind: BracketType::Open, pair: 0x3011 }),
        0x3011 => Some(BracketEntry { kind: BracketType::Close, pair: 0x3010 }),
        // 〔〕 U+3014/U+3015 — left / right tortoise-shell bracket.
        0x3014 => Some(BracketEntry { kind: BracketType::Open, pair: 0x3015 }),
        0x3015 => Some(BracketEntry { kind: BracketType::Close, pair: 0x3014 }),
        // 〖〗 U+3016/U+3017 — left / right white lenticular.
        0x3016 => Some(BracketEntry { kind: BracketType::Open, pair: 0x3017 }),
        0x3017 => Some(BracketEntry { kind: BracketType::Close, pair: 0x3016 }),
        // 〘〙 U+3018/U+3019 — left / right white tortoise-shell.
        0x3018 => Some(BracketEntry { kind: BracketType::Open, pair: 0x3019 }),
        0x3019 => Some(BracketEntry { kind: BracketType::Close, pair: 0x3018 }),
        // 〚〛 U+301A/U+301B — left / right white square bracket.
        0x301A => Some(BracketEntry { kind: BracketType::Open, pair: 0x301B }),
        0x301B => Some(BracketEntry { kind: BracketType::Close, pair: 0x301A }),
        // 《》 U+300A/U+300B — left / right double angle bracket.
        0x300A => Some(BracketEntry { kind: BracketType::Open, pair: 0x300B }),
        0x300B => Some(BracketEntry { kind: BracketType::Close, pair: 0x300A }),
        // 〈〉 U+3008/U+3009 — left / right angle bracket (CJK).
        0x3008 => Some(BracketEntry { kind: BracketType::Open, pair: 0x3009 }),
        0x3009 => Some(BracketEntry { kind: BracketType::Close, pair: 0x3008 }),

        // Small-form variants. Used in vertical CJK and East Asian
        // forms; same pairing semantics as their wide cousins.
        0xFE59 => Some(BracketEntry { kind: BracketType::Open, pair: 0xFE5A }),
        0xFE5A => Some(BracketEntry { kind: BracketType::Close, pair: 0xFE59 }),
        0xFE5B => Some(BracketEntry { kind: BracketType::Open, pair: 0xFE5C }),
        0xFE5C => Some(BracketEntry { kind: BracketType::Close, pair: 0xFE5B }),
        0xFE5D => Some(BracketEntry { kind: BracketType::Open, pair: 0xFE5E }),
        0xFE5E => Some(BracketEntry { kind: BracketType::Close, pair: 0xFE5D }),

        // Halfwidth and Fullwidth Forms.
        // （） U+FF08/U+FF09.
        0xFF08 => Some(BracketEntry { kind: BracketType::Open, pair: 0xFF09 }),
        0xFF09 => Some(BracketEntry { kind: BracketType::Close, pair: 0xFF08 }),
        // ［］ U+FF3B/U+FF3D.
        0xFF3B => Some(BracketEntry { kind: BracketType::Open, pair: 0xFF3D }),
        0xFF3D => Some(BracketEntry { kind: BracketType::Close, pair: 0xFF3B }),
        // ｛｝ U+FF5B/U+FF5D.
        0xFF5B => Some(BracketEntry { kind: BracketType::Open, pair: 0xFF5D }),
        0xFF5D => Some(BracketEntry { kind: BracketType::Close, pair: 0xFF5B }),
        // ｟｠ U+FF5F/U+FF60.
        0xFF5F => Some(BracketEntry { kind: BracketType::Open, pair: 0xFF60 }),
        0xFF60 => Some(BracketEntry { kind: BracketType::Close, pair: 0xFF5F }),
        // ｢｣ U+FF62/U+FF63 — halfwidth corner brackets.
        0xFF62 => Some(BracketEntry { kind: BracketType::Open, pair: 0xFF63 }),
        0xFF63 => Some(BracketEntry { kind: BracketType::Close, pair: 0xFF62 }),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_paren_pairs_resolve() {
        assert_eq!(
            bracket_of('(' as u32),
            Some(BracketEntry { kind: BracketType::Open, pair: ')' as u32 }),
        );
        assert_eq!(
            bracket_of(')' as u32),
            Some(BracketEntry { kind: BracketType::Close, pair: '(' as u32 }),
        );
    }

    #[test]
    fn ascii_square_and_curly_pairs_resolve() {
        assert_eq!(
            bracket_of('[' as u32),
            Some(BracketEntry { kind: BracketType::Open, pair: ']' as u32 }),
        );
        assert_eq!(
            bracket_of('{' as u32),
            Some(BracketEntry { kind: BracketType::Open, pair: '}' as u32 }),
        );
    }

    #[test]
    fn cjk_corner_brackets_resolve() {
        // U+300C 「 ↔ U+300D 」.
        assert_eq!(
            bracket_of(0x300C),
            Some(BracketEntry { kind: BracketType::Open, pair: 0x300D }),
        );
        assert_eq!(
            bracket_of(0x300D),
            Some(BracketEntry { kind: BracketType::Close, pair: 0x300C }),
        );
    }

    #[test]
    fn math_angle_brackets_resolve() {
        // U+27E8 ⟨ ↔ U+27E9 ⟩.
        assert_eq!(
            bracket_of(0x27E8),
            Some(BracketEntry { kind: BracketType::Open, pair: 0x27E9 }),
        );
        assert_eq!(
            bracket_of(0x27E9),
            Some(BracketEntry { kind: BracketType::Close, pair: 0x27E8 }),
        );
    }

    #[test]
    fn fullwidth_paren_resolves() {
        assert_eq!(
            bracket_of(0xFF08),
            Some(BracketEntry { kind: BracketType::Open, pair: 0xFF09 }),
        );
    }

    #[test]
    fn non_brackets_return_none() {
        assert_eq!(bracket_of('A' as u32), None);
        assert_eq!(bracket_of(' ' as u32), None);
        assert_eq!(bracket_of(0x05D0), None); // Hebrew alef
    }

    #[test]
    fn open_close_relation_is_symmetric() {
        // Pick a few pairs and verify the closing half points back to
        // the opening half and vice versa.
        for &cp in &[
            '(' as u32, '[' as u32, '{' as u32, 0x300C, 0x27E8, 0xFF08, 0xFE59,
        ] {
            let open = bracket_of(cp).expect("known opener");
            assert_eq!(open.kind, BracketType::Open);
            let close = bracket_of(open.pair).expect("paired closer exists");
            assert_eq!(close.kind, BracketType::Close);
            assert_eq!(close.pair, cp, "round-trip pair lookup");
        }
    }
}
