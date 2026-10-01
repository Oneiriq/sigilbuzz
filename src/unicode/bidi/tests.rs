//! Tests for the bidi algorithm: paragraph direction, end-to-end
//! levels, per-rule coverage (W, N0-N2, X, L2), and an extract of the
//! UAX #9 reference tests.

use super::*;
use alloc::vec;

// ---- Backwards-compat tests from 0.1.0 (preserved) ----

#[test]
fn ascii_letters_are_strong_ltr() {
    assert_eq!(bidi_class('a'), BidiClass::L);
    assert_eq!(bidi_class('Z'), BidiClass::L);
}

#[test]
fn arabic_is_strong_al() {
    assert_eq!(bidi_class('ا'), BidiClass::Al);
    assert_eq!(bidi_class('م'), BidiClass::Al);
}

#[test]
fn hebrew_is_strong_rtl() {
    assert_eq!(bidi_class('א'), BidiClass::R);
    assert_eq!(bidi_class('ש'), BidiClass::R);
}

#[test]
fn first_strong_determines_paragraph_direction() {
    assert_eq!(paragraph_direction("Hello"), Direction::Ltr);
    assert_eq!(paragraph_direction("שלום"), Direction::Rtl);
    assert_eq!(paragraph_direction("السلام عليكم"), Direction::Rtl);
}

#[test]
fn leading_neutrals_are_skipped() {
    assert_eq!(paragraph_direction("   Hello"), Direction::Ltr);
    assert_eq!(paragraph_direction("12 שלום"), Direction::Rtl);
}

#[test]
fn strong_ltr_wins_over_rtl_that_follows() {
    assert_eq!(paragraph_direction("Hello שלום"), Direction::Ltr);
}

#[test]
fn empty_or_neutral_input_defaults_to_ltr() {
    assert_eq!(paragraph_direction(""), Direction::Ltr);
    assert_eq!(paragraph_direction("     "), Direction::Ltr);
    assert_eq!(paragraph_direction("12345"), Direction::Ltr);
}

// ---- New tests: BidiInfo end-to-end ----

#[test]
fn pure_latin_resolves_to_zero_levels() {
    let info = BidiInfo::new("Hello", None);
    assert_eq!(info.paragraph_direction(), Direction::Ltr);
    assert_eq!(info.levels(), &[0, 0, 0, 0, 0]);
    assert_eq!(info.reorder(), vec![0, 1, 2, 3, 4]);
}

#[test]
fn pure_hebrew_resolves_to_level_one() {
    // 4 Hebrew chars.
    let info = BidiInfo::new("שלום", None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    assert_eq!(info.levels(), &[1, 1, 1, 1]);
    // L2: reverse all chars at level >= 1.
    assert_eq!(info.reorder(), vec![3, 2, 1, 0]);
}

#[test]
fn ltr_then_hebrew_keeps_latin_at_zero_hebrew_at_one() {
    // "A " + "ב" (Hebrew bet): paragraph LTR.
    let info = BidiInfo::new("A ב", None);
    assert_eq!(info.paragraph_direction(), Direction::Ltr);
    // 'A' L -> 0; ' ' WS reset to 0 (L1 trailing); 'ב' R -> 1.
    // After L1 trailing-whitespace reset on the space (it's not
    // trailing: Hebrew letter follows), space stays at 0.
    // Actually the space comes between two strong runs; N1
    // sees L on one side and R on the other, so no rule fires;
    // N2 sets it to embedding (0). So levels: [0, 0, 1].
    assert_eq!(info.levels(), &[0, 0, 1]);
    // L2 reverses chars at level >= 1 (just the bet).
    assert_eq!(info.reorder(), vec![0, 1, 2]);
}

#[test]
fn hebrew_then_latin_with_rtl_paragraph_reverses_hebrew() {
    // Paragraph RTL ("שלום A"). 4 Hebrew + space + 'A'.
    let info = BidiInfo::new("שלום A", None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    // 4 hebrews at 1, space gets paragraph (1) by N2, A at 2 (LTR
    // inside RTL para).
    let levels = info.levels();
    assert_eq!(levels.len(), 6);
    // A at level 2.
    assert_eq!(levels[5], 2);
    // Hebrew letters at level 1.
    for &l in &levels[0..4] {
        assert_eq!(l, 1);
    }
}

#[test]
fn explicit_paragraph_direction_overrides_first_strong() {
    let info = BidiInfo::new("Hello", Some(Direction::Rtl));
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    // Latin in RTL paragraph -> level 2.
    assert_eq!(info.levels(), &[2, 2, 2, 2, 2]);
}

#[test]
fn empty_input_yields_empty_info() {
    let info = BidiInfo::new("", None);
    assert_eq!(info.paragraph_direction(), Direction::Ltr);
    assert!(info.levels().is_empty());
    assert!(info.reorder().is_empty());
}

// ---- W-rule unit coverage ----

#[test]
fn w2_en_after_al_becomes_an() {
    // Arabic alef + ASCII '1'. W2 fires: EN after AL -> AN.
    // W3: AL->R. Embedding level 1. I2 at odd: AN->+1 -> level 2.
    let info = BidiInfo::new("\u{0627}1", None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    let levels = info.levels();
    assert_eq!(levels.len(), 2);
    // Arabic alef: AL -> R -> odd, no bump -> level 1.
    assert_eq!(levels[0], 1);
    // '1' became AN; embedding 1; AN bumped +1 -> level 2.
    assert_eq!(levels[1], 2);
}

#[test]
fn w4_es_between_ens_promotes_in_rtl() {
    // "1+2" in default paragraph: sos=L, W7 turns the EN run
    // back to L -> level 0. To exercise W4 cleanly we force
    // RTL paragraph: sos=R, no W7 promotion -> digits keep EN.
    let info = BidiInfo::new("1+2", Some(Direction::Rtl));
    let levels = info.levels();
    assert_eq!(levels.len(), 3);
    // Embedding 1 for ENs; I2 bumps EN by 1 -> level 2.
    // The ES becomes EN via W4 (between two ENs).
    assert_eq!(levels, &[2, 2, 2]);
}

#[test]
fn w5_et_adjacent_to_en_becomes_en_in_rtl() {
    // "$1": ET EN. W5 turns ET into EN. Default-LTR paragraph
    // would then run W7 and downgrade EN->L. Force RTL to see
    // pure W5 effect.
    let info = BidiInfo::new("$1", Some(Direction::Rtl));
    let levels = info.levels();
    assert_eq!(levels.len(), 2);
    // Both EN at level 2 (embedding 1, I2 bumps EN +1).
    assert_eq!(levels, &[2, 2]);
}

#[test]
fn w7_en_after_l_becomes_l() {
    // "A1": L EN. W7 fires: EN preceded by L -> L. So both at
    // level 0 in an LTR paragraph.
    let info = BidiInfo::new("A1", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 2);
    assert_eq!(levels, &[0, 0]);
}

// ---- N-rule unit coverage ----

#[test]
fn n2_neutral_takes_embedding() {
    // Pure-neutral paragraph: just '!'. Default paragraph LTR,
    // N2 sets ON to L -> level 0.
    let info = BidiInfo::new("!", None);
    assert_eq!(info.levels(), &[0]);
}

// ---- N0 paired-bracket coverage ----

#[test]
fn n0_ascii_parens_in_rtl_paragraph_take_r() {
    // ב ( ב ב ב ): Hebrew letter, opener, three Hebrew, closer.
    // Embedding direction R; brackets surround a pure-R run so N0
    // fires and both parens resolve as R -> all level 1.
    let info = BidiInfo::new("\u{05D1}(\u{05D1}\u{05D1}\u{05D1})", None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    let levels = info.levels();
    assert_eq!(levels.len(), 6);
    for &l in levels {
        assert_eq!(l, 1, "every char including brackets at level 1");
    }
}

#[test]
fn n0_ascii_parens_in_ltr_paragraph_take_l() {
    // A ( ב ב ב ) C: LTR paragraph, brackets around an inner
    // Hebrew run. Embedding is L (paragraph LTR); the run inside
    // is R, but with no L between them the pair takes the
    // surrounding context. The text before the opener is L (A),
    // so per N0 the brackets resolve to embed direction L.
    let info = BidiInfo::new("A(\u{05D1}\u{05D1}\u{05D1})C", None);
    assert_eq!(info.paragraph_direction(), Direction::Ltr);
    let levels = info.levels();
    assert_eq!(levels.len(), 7);
    // Brackets at L (level 0); Hebrew at level 1.
    assert_eq!(levels[0], 0, "A");
    assert_eq!(levels[1], 0, "open paren resolves L");
    assert_eq!(levels[2], 1, "Hebrew");
    assert_eq!(levels[5], 0, "close paren resolves L");
    assert_eq!(levels[6], 0, "C after closer at L");
}

#[test]
fn n0_brackets_with_embedding_strong_inside_take_embedding() {
    // A ( ב A ב ): brackets enclose mixed content with the
    // embedding direction's strong (L: 'A') present. N0's first
    // rule: if embed-strong appears between open and close, the
    // pair takes embed-strong.
    let info = BidiInfo::new("A(\u{05D1}A\u{05D1})", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 6);
    // Brackets resolve to L (embedding): level 0.
    assert_eq!(levels[1], 0);
    assert_eq!(levels[5], 0);
}

#[test]
fn n0_cjk_corner_brackets_resolve() {
    // 「 ב ב 」 with paragraph LTR + a leading 'A' to anchor sos.
    // U+300C/U+300D are CJK corner brackets; N0 must treat them
    // identically to ASCII parens.
    let info = BidiInfo::new("A\u{300C}\u{05D1}\u{05D1}\u{300D}", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 5);
    assert_eq!(levels[0], 0, "A at L");
    // The corner brackets enclose pure-R content with a leading L
    // before. N0 picks embedding (L) -> brackets at level 0.
    assert_eq!(levels[1], 0, "open corner bracket at embedding L");
    assert_eq!(levels[4], 0, "close corner bracket at embedding L");
}

#[test]
fn n0_nested_brackets_resolve_independently() {
    // A [ ( ב ) ב ] B: outer square brackets and inner parens.
    // Inner: ב only -> no L strong -> opposite (R) seen -> preceding
    //   text before '(' is '['. Looking at preceding strong: '['
    //   has not yet been resolved by N0; it's still ON. So look
    //   further back: 'A' (L) precedes. Embedding L; opposite R.
    //   No embed-strong inside, so check preceding strong: L
    //   (from A). Preceding != opposite, so take embed = L.
    //   Inner brackets -> L.
    // Outer: span includes '(', ')', and ב; both brackets are
    //   now L by inner's N0 pass plus the inner ב is R. Embed
    //   strong (L) seen -> outer brackets -> L.
    let info = BidiInfo::new("A[(\u{05D1})\u{05D1}]B", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 8);
    assert_eq!(levels[0], 0, "A");
    assert_eq!(levels[1], 0, "outer open");
    assert_eq!(levels[2], 0, "inner open");
    assert_eq!(levels[3], 1, "Hebrew between inner brackets");
    assert_eq!(levels[4], 0, "inner close");
    assert_eq!(levels[5], 1, "Hebrew between outer brackets");
    assert_eq!(levels[6], 0, "outer close at L");
    assert_eq!(levels[7], 0, "B");
}

#[test]
fn n0_unpaired_bracket_falls_through_to_n1() {
    // Just an opener with no closer: '(' alone. N0 sees no pair
    // (the stack-pop never matches), so it leaves the paren as
    // ON and N1 / N2 handle it. In an LTR paragraph the lone
    // paren resolves to L via N2.
    let info = BidiInfo::new("A(B", None);
    let levels = info.levels();
    assert_eq!(levels, &[0, 0, 0]);
}

#[test]
fn n0_mismatched_bracket_skips() {
    // A ( ב ] ב ): opener `(` paired with `)`; the misplaced
    // `]` is a stray closer with no matching `[` opener. BD16
    // says skip the unmatched closer. The paren pair still fires
    // and resolves to L (embedding) for the LTR paragraph.
    let info = BidiInfo::new("A(\u{05D1}]\u{05D1})", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 6);
    assert_eq!(levels[0], 0, "A");
    assert_eq!(levels[1], 0, "open paren resolved L");
    assert_eq!(levels[5], 0, "close paren resolved L");
}

#[test]
fn n1_neutral_between_two_rs_takes_r() {
    // ב!ב: three chars. N1 should set the '!' to R, then I1
    // bumps R by 1 -> all level 1.
    let info = BidiInfo::new("\u{05D1}!\u{05D1}", None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    let levels = info.levels();
    assert_eq!(levels.len(), 3);
    for &l in levels {
        assert_eq!(l, 1);
    }
}

// ---- X-rule explicit-format coverage ----

#[test]
fn rle_pdf_pair_embeds_then_pops() {
    // RLE 'A' PDF 'B': 'A' is inside an RTL embedding (level 1),
    // 'B' is at the paragraph level (0).
    let info = BidiInfo::new("\u{202B}A\u{202C}B", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 4);
    // RLE itself: nothing precedes it, so the paragraph level (0).
    assert_eq!(levels[0], 0);
    // 'A' inside RLE -> embedded at level 1, then I2 bumps L
    // by 1 -> level 2.
    assert_eq!(levels[1], 2);
    // PDF: X9 removes it, and a removed character takes the level of
    // the character before it, so it closes the embedded run.
    assert_eq!(levels[2], 2);
    // 'B' back at paragraph level -> 0.
    assert_eq!(levels[3], 0);
}

// ---- Removed characters and L1 ----

#[test]
fn zwnj_inside_an_rtl_word_keeps_the_word_level() {
    // Persian "mi" ZWNJ "khaham" in an LTR paragraph. ZWNJ is BN,
    // which X9 removes; it takes the level of the letter before it
    // instead of the paragraph level, so the word is one level-1 run.
    let text = "a \u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}";
    let info = BidiInfo::new(text, None);
    assert_eq!(info.levels(), &[0, 0, 1, 1, 1, 1, 1]);
    // ZWJ at the very start takes the paragraph level.
    let info = BidiInfo::new("\u{200D}\u{0628}", Some(Direction::Ltr));
    assert_eq!(info.levels(), &[0, 1]);
}

#[test]
fn l1_resets_segment_separators_and_the_whitespace_before_them() {
    // Hebrew, two spaces, TAB, Hebrew in an LTR paragraph: N1 would
    // give the spaces and the tab level 1 (R on both sides); L1 puts
    // the tab and the spaces before it back at the paragraph level.
    let info = BidiInfo::new("\u{05D0}  \t\u{05D1}", Some(Direction::Ltr));
    assert_eq!(info.levels(), &[1, 0, 0, 0, 1]);
}

#[test]
fn l1_resets_trailing_whitespace_and_controls() {
    // RLE "ab " PDF at the end of an LTR paragraph: the space and the
    // PDF trail the text, so they return to level 0.
    let info = BidiInfo::new("x\u{202B}ab \u{202C}", None);
    assert_eq!(info.levels(), &[0, 0, 2, 2, 0, 0]);
    // A space inside the text is not trailing.
    let info = BidiInfo::new("\u{05D0} \u{05D1}", Some(Direction::Ltr));
    assert_eq!(info.levels(), &[1, 1, 1]);
}

#[test]
fn fsi_with_latin_inside_resolves_as_lri() {
    // 'A' FSI 'X' PDI 'B': first strong inside FSI is L, so FSI
    // must behave as LRI (embed at next *even* level, here 2).
    // 'X' is L -> I1 even-level no bump -> level 2.
    let info = BidiInfo::new("A\u{2068}X\u{2069}B", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 5);
    assert_eq!(levels[0], 0);
    assert_eq!(levels[2], 2);
    assert_eq!(levels[4], 0);
}

#[test]
fn fsi_with_hebrew_inside_resolves_as_rli() {
    // 'A' FSI 'אבג' PDI 'B': first strong inside FSI is R, so
    // FSI must behave as RLI (embed at next *odd* level, here 1).
    // Hebrew letters at level 1; B at paragraph 0.
    let info = BidiInfo::new("A\u{2068}\u{05D0}\u{05D1}\u{05D2}\u{2069}B", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 7);
    assert_eq!(levels[0], 0);
    for &l in &levels[2..5] {
        assert_eq!(l, 1, "hebrew inside FSI should be at level 1");
    }
    assert_eq!(levels[6], 0);
}

#[test]
fn fsi_with_no_strong_inside_defaults_to_lri() {
    // 'A' FSI '!!!' PDI 'B': no strong type inside FSI, so default
    // is LRI: embed at level 2 in LTR paragraph; '!' chars resolve
    // to L via N2 at level 2.
    let info = BidiInfo::new("A\u{2068}!!!\u{2069}B", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 7);
    assert_eq!(levels[0], 0);
    for &l in &levels[2..5] {
        assert_eq!(l, 2, "neutrals inside FSI default to LRI level 2");
    }
    assert_eq!(levels[6], 0);
}

#[test]
fn fsi_skips_nested_isolates_when_resolving() {
    // 'A' FSI LRI 'B' PDI 'ש' PDI 'C': first strong AT FSI's
    // own depth must be 'ש' (R), not the nested LRI's 'B'. So FSI
    // should resolve as RLI: embed level 1, hebrew at 1, 'B'
    // (inside the nested LRI at FSI+1=2) at the inner LRI's
    // even-bumped level 2.
    let text = "A\u{2068}\u{2066}B\u{2069}\u{05E9}\u{2069}C";
    let info = BidiInfo::new(text, None);
    let levels = info.levels();
    // Hebrew 'ש' is the 6th char (index 5) of the input.
    // Verify FSI itself sat at the surrounding paragraph level (0)
    // and the 'ש' resolves at FSI's odd-embedded level 1.
    assert_eq!(levels[0], 0); // 'A'
    assert_eq!(levels[5], 1, "hebrew inside FSI must be at level 1");
    assert_eq!(*levels.last().unwrap(), 0); // 'C'
}

#[test]
fn lri_pdi_isolate_pair_works() {
    // 'A' LRI 'B' PDI 'C': straightforward LTR isolate.
    let info = BidiInfo::new("A\u{2066}B\u{2069}C", None);
    let levels = info.levels();
    assert_eq!(levels.len(), 5);
    // All chars in LTR paragraph at level 0 except for the LRI
    // itself which sits at the surrounding level. The 'B' is
    // inside an LTR isolate at level 2.
    assert_eq!(levels[0], 0);
    assert_eq!(levels[2], 2);
    assert_eq!(levels[4], 0);
}

// ---- L2 reordering coverage ----

#[test]
fn reorder_preserves_logical_when_no_levels() {
    let info = BidiInfo::new("abc", None);
    assert_eq!(info.reorder(), vec![0, 1, 2]);
}

#[test]
fn reorder_reverses_pure_rtl() {
    let info = BidiInfo::new("\u{05D0}\u{05D1}\u{05D2}", None);
    assert_eq!(info.reorder(), vec![2, 1, 0]);
}

#[test]
fn reorder_mixed_latin_hebrew_in_ltr_para() {
    // "A" + Hebrew bet gimel + "B"  ->  level 0 1 1 0
    // L2 reverses the level-1 span: visual = A gimel bet B.
    let info = BidiInfo::new("A\u{05D1}\u{05D2}B", None);
    let levels = info.levels();
    assert_eq!(levels, &[0, 1, 1, 0]);
    assert_eq!(info.reorder(), vec![0, 2, 1, 3]);
}

// ---- UAX 9 reference test extract ----
//
// 10 curated entries from BidiCharacterTest.txt. Each row is
// (text, paragraph_dir_override, expected per-char levels).
//
// Entries are paraphrased: the reference test uses raw
// codepoints; here we hand-pick a representative slice so the
// intent is readable.

#[test]
fn uax9_reference_extract_covers_strong_neutral_weak_explicit() {
    struct Case<'a> {
        text: &'a str,
        para: Option<Direction>,
        levels: &'a [u8],
    }
    let cases = [
        // 1. Pure Latin.
        Case {
            text: "abc",
            para: None,
            levels: &[0, 0, 0],
        },
        // 2. Pure Hebrew.
        Case {
            text: "\u{05D0}\u{05D1}\u{05D2}",
            para: None,
            levels: &[1, 1, 1],
        },
        // 3. Latin + Hebrew + Latin in LTR.
        Case {
            text: "a\u{05D0}b",
            para: None,
            levels: &[0, 1, 0],
        },
        // 4. Hebrew + Latin + Hebrew in RTL.
        Case {
            text: "\u{05D0}a\u{05D1}",
            para: None,
            levels: &[1, 2, 1],
        },
        // 5. Digit-only paragraph: sos=L, W7 turns EN->L -> level 0.
        Case {
            text: "123",
            para: None,
            levels: &[0, 0, 0],
        },
        // 6. Arabic letter + ASCII digit. W2: EN->AN; W3: AL->R;
        //    odd embedding 1, I2 bumps AN by 1 -> level 2.
        Case {
            text: "\u{0627}1",
            para: None,
            levels: &[1, 2],
        },
        // 7. ET adjacent to EN: "$1" with paragraph LTR. W5
        //    turns ET into EN, then W7 turns the EN run back
        //    into L (sos=L) -> level 0.
        Case {
            text: "$1",
            para: None,
            levels: &[0, 0],
        },
        // 8. ES between two ENs in LTR para: W4 turns ES into
        //    EN, W7 then turns the run into L -> level 0.
        Case {
            text: "1+2",
            para: None,
            levels: &[0, 0, 0],
        },
        // 9. RLE override.
        Case {
            text: "\u{202B}AB\u{202C}",
            para: None,
            levels: &[0, 2, 2, 0],
        },
        // 10. LRI / PDI isolate. The LRI / PDI sit at the
        //     surrounding level; only B inside the isolate is
        //     at level 2.
        Case {
            text: "A\u{2066}B\u{2069}C",
            para: None,
            levels: &[0, 0, 2, 0, 0],
        },
    ];
    for (i, c) in cases.iter().enumerate() {
        let info = BidiInfo::new(c.text, c.para);
        assert_eq!(
            info.levels(),
            c.levels,
            "case {i}: text {:?} expected {:?}, got {:?}",
            c.text,
            c.levels,
            info.levels()
        );
    }
}

#[test]
fn paragraph_ranges_split_after_each_separator() {
    // Each paragraph as (start, end).
    let split = |text: &str| -> Vec<(usize, usize)> {
        paragraph_ranges(text)
            .into_iter()
            .map(|r| (r.start, r.end))
            .collect()
    };
    assert!(split("").is_empty());
    assert_eq!(split("abc"), [(0, 3)]);
    assert_eq!(split("a\nb"), [(0, 2), (2, 3)]);
    assert_eq!(split("a\r\nb"), [(0, 3), (3, 4)]);
    assert_eq!(split("a\n\rb"), [(0, 2), (2, 3), (3, 4)]);
    assert_eq!(split("a\r"), [(0, 2)]);
    assert_eq!(split("\u{2029}\u{85}"), [(0, 3), (3, 5)]);
    assert_eq!(split("a\u{1C}b\u{1D}c\u{1E}"), [(0, 2), (2, 4), (4, 6)]);
    // Line and segment separators do not end a paragraph.
    assert_eq!(split("a\u{2028}b\tc\u{0B}d"), [(0, 9)]);
}
