/*
 * test_shape.c: minimal C-side smoke test exercised by tests/c_link.rs.
 *
 * The test driver (in Rust) compiles this against the sigilbuzz cdylib
 * and runs the resulting executable; exit 0 means PASS. The test
 * loads the bundled Open Sans font, shapes "Hello", and asserts at
 * least five glyphs come back with non-zero advances.
 */

#include "hb.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

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
    unsigned int upem = hb_face_get_upem(face);
    if (upem == 0) {
        fprintf(stderr, "upem is zero\n");
        return 5;
    }

    hb_font_t *font = hb_font_create(face);
    hb_buffer_t *buffer = hb_buffer_create();

    const char *text = "Hello";
    hb_buffer_add_utf8(buffer, text, -1, 0, -1);
    hb_buffer_set_direction(buffer, HB_DIRECTION_LTR);
    hb_buffer_set_script(buffer, HB_SCRIPT_LATIN);

    hb_bool_t ok = hb_shape_full(font, buffer, NULL, 0, NULL);
    if (!ok) {
        fprintf(stderr, "hb_shape_full failed\n");
        return 6;
    }

    unsigned int len = 0;
    hb_glyph_info_t *infos = hb_buffer_get_glyph_infos(buffer, &len);
    hb_glyph_position_t *positions = hb_buffer_get_glyph_positions(buffer, &len);

    if (len < 5) {
        fprintf(stderr, "expected at least 5 glyphs, got %u\n", len);
        return 7;
    }

    /* Every glyph in "Hello" (H, e, l, l, o) must produce a
     * positive advance from a sane Latin font. */
    for (unsigned int i = 0; i < len; i++) {
        if (positions[i].x_advance <= 0) {
            fprintf(stderr, "glyph %u has non-positive advance %d\n",
                    i, positions[i].x_advance);
            return 8;
        }
        if (infos[i].codepoint == 0) {
            fprintf(stderr, "glyph %u resolved to .notdef\n", i);
            return 9;
        }
    }

    /* Version sanity. */
    unsigned int major = 0, minor = 0, micro = 0;
    hb_version(&major, &minor, &micro);
    if (major == 0) {
        fprintf(stderr, "hb_version returned 0\n");
        return 10;
    }

    const char *vs = hb_version_string();
    if (!vs || strlen(vs) == 0) {
        fprintf(stderr, "hb_version_string is empty\n");
        return 11;
    }

    hb_buffer_destroy(buffer);
    hb_font_destroy(font);
    hb_face_destroy(face);
    hb_blob_destroy(blob);
    return 0;
}
