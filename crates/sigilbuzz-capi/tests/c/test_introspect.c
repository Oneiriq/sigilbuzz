/*
 * test_introspect.c: drives hb_face_collect_unicodes,
 * hb_ot_layout_collect_features, the variation sequence collectors, and
 * the hb_font glyph lookups against Open Sans.
 *
 * The collectors populate an `hb_set_t` the caller hands in. The test
 * asserts the sets hold at least one known entry (U+0041 for
 * collect_unicodes, any feature tag for collect_features) and that Open
 * Sans, which has no cmap format 14 subtable, lists no variation
 * sequences. Exit 0 means PASS.
 */

#include "hb.h"
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <font.ttf>\n", argv[0]);
        return 2;
    }

    hb_blob_t *blob = hb_blob_create_from_file(argv[1]);
    if (!blob || hb_blob_get_length(blob) == 0) {
        fprintf(stderr, "could not load font %s\n", argv[1]);
        return 3;
    }
    hb_face_t *face = hb_face_create(blob, 0);

    /* collect_unicodes must include the basic-Latin uppercase set. */
    hb_set_t *unicodes = hb_set_create();
    hb_face_collect_unicodes(face, unicodes);
    for (uint32_t cp = 0x41; cp <= 0x5A; cp++) {
        if (hb_set_has(unicodes, cp) != 1) {
            fprintf(stderr, "expected U+%04X in collect_unicodes output\n", cp);
            return 4;
        }
    }
    if (hb_set_get_population(unicodes) < 100) {
        fprintf(stderr, "collect_unicodes returned only %u codepoints\n",
                hb_set_get_population(unicodes));
        return 5;
    }
    hb_set_destroy(unicodes);

    /* collect_features (GSUB) must populate at least one feature. */
    hb_set_t *features = hb_set_create();
    hb_ot_layout_collect_features(face, HB_OT_TAG_GSUB, NULL, NULL, features);
    if (hb_set_get_population(features) == 0) {
        fprintf(stderr, "expected non-empty GSUB feature set\n");
        return 6;
    }
    hb_set_destroy(features);

    /* An unrecognised table tag must be a clean no-op. */
    hb_set_t *empty = hb_set_create();
    hb_ot_layout_collect_features(face, HB_TAG('X','X','X','X'), NULL, NULL, empty);
    if (hb_set_get_population(empty) != 0) {
        fprintf(stderr, "expected empty set for unknown table tag\n");
        return 7;
    }
    hb_set_destroy(empty);

    /* Open Sans has no cmap format 14 subtable: no selectors, and no
     * variation glyph even for a mapped base. */
    hb_set_t *selectors = hb_set_create();
    hb_face_collect_variation_selectors(face, selectors);
    hb_face_collect_variation_unicodes(face, 0xFE00, selectors);
    if (hb_set_get_population(selectors) != 0) {
        fprintf(stderr, "expected no variation selectors in Open Sans\n");
        return 8;
    }
    hb_set_destroy(selectors);

    hb_font_t *font = hb_font_create(face);
    hb_codepoint_t glyph = 0;
    if (!hb_font_get_nominal_glyph(font, 0x41, &glyph) || glyph == 0) {
        fprintf(stderr, "expected a nominal glyph for U+0041\n");
        return 9;
    }
    if (hb_font_get_variation_glyph(font, 0x41, 0xFE00, &glyph) || glyph != 0) {
        fprintf(stderr, "expected no variation glyph for U+0041 U+FE00\n");
        return 10;
    }
    if (!hb_font_get_glyph(font, 0x41, 0, &glyph) || glyph == 0) {
        fprintf(stderr, "expected hb_font_get_glyph to find U+0041\n");
        return 11;
    }
    hb_font_destroy(font);

    hb_face_destroy(face);
    hb_blob_destroy(blob);
    return 0;
}
