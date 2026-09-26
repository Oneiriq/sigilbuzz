/*
 * test_subset.c: drives the hb_subset_* bridge from C.
 *
 * Loads Open Sans, asks the subsetter for a face containing only
 * U+0041 / U+0042 / U+0043, and asserts the result has exactly four
 * glyphs (.notdef + A + B + C). Exit 0 means PASS.
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
    if (!face || hb_face_get_glyph_count(face) == 0) {
        fprintf(stderr, "could not parse face\n");
        return 4;
    }

    /* Build a subset input that asks for ABC. */
    hb_subset_input_t *input = hb_subset_input_create();
    if (!input) {
        fprintf(stderr, "hb_subset_input_create failed\n");
        return 5;
    }
    hb_set_t *unicode_set = hb_subset_input_unicode_set(input);
    if (!unicode_set) {
        fprintf(stderr, "hb_subset_input_unicode_set returned NULL\n");
        return 6;
    }
    hb_set_add(unicode_set, 0x41); /* A */
    hb_set_add(unicode_set, 0x42); /* B */
    hb_set_add(unicode_set, 0x43); /* C */
    if (hb_set_get_population(unicode_set) != 3) {
        fprintf(stderr, "expected 3 codepoints in unicode_set, got %u\n",
                hb_set_get_population(unicode_set));
        return 7;
    }
    /* The set belongs to the input (HarfBuzz "transfer none"): the same
     * pointer comes back every time and the caller never destroys it. */
    if (hb_subset_input_unicode_set(input) != unicode_set) {
        fprintf(stderr, "hb_subset_input_unicode_set returned a different pointer\n");
        return 11;
    }

    hb_face_t *subset_face = hb_subset_or_fail(face, input);
    if (!subset_face) {
        fprintf(stderr, "hb_subset_or_fail returned NULL\n");
        return 8;
    }
    unsigned int count = hb_face_get_glyph_count(subset_face);
    if (count != 4) {
        fprintf(stderr, "expected 4 glyphs in subset (.notdef + A + B + C), got %u\n", count);
        return 9;
    }

    /* Subset face must have a non-zero upem (it's a real SFNT). */
    if (hb_face_get_upem(subset_face) == 0) {
        fprintf(stderr, "subset face upem is zero\n");
        return 10;
    }

    hb_face_destroy(subset_face);
    hb_subset_input_destroy(input);
    hb_face_destroy(face);
    hb_blob_destroy(blob);
    return 0;
}
