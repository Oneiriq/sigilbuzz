/*
 * test_paint.c: drives the hb_paint_* bridge from C.
 *
 * The drop-in fixture font (Open Sans) is monochrome: it has no
 * COLR table. That makes this a useful linkage smoke test: the C
 * compiler / linker must resolve every hb_paint_* symbol against
 * sigilbuzz-capi, and the dispatcher must turn a `hb_font_paint_glyph`
 * call against a non-COLR face into a clean no-op (no callbacks
 * fired, no crash). The byte-level callback-order assertions live in
 * the Rust-side `paint_bridge` integration tests against a synthetic
 * COLR fixture.
 *
 * Exit 0 means PASS.
 */

#include "hb.h"
#include <stdio.h>
#include <stdlib.h>

static int g_color_calls = 0;
static int g_push_layer_calls = 0;

static void on_color(hb_paint_funcs_t *funcs, void *paint_data,
                     hb_bool_t is_foreground, hb_color_t color) {
    (void)funcs; (void)paint_data; (void)is_foreground; (void)color;
    g_color_calls += 1;
}

static void on_push_layer(hb_paint_funcs_t *funcs, void *paint_data,
                          uint32_t composite_mode) {
    (void)funcs; (void)paint_data; (void)composite_mode;
    g_push_layer_calls += 1;
}

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
    hb_font_t *font = hb_font_create(face);

    hb_paint_funcs_t *funcs = hb_paint_funcs_create();
    if (!funcs) {
        fprintf(stderr, "hb_paint_funcs_create returned NULL\n");
        return 4;
    }
    hb_paint_funcs_set_color_func(funcs, on_color);
    hb_paint_funcs_set_push_layer_func(funcs, on_push_layer);

    /* Open Sans has no COLR: paint_glyph must be a clean no-op. */
    hb_font_paint_glyph(font, 36 /* arbitrary gid */, funcs, NULL,
                        0, HB_COLOR(0, 0, 0, 0xFF));

    if (g_color_calls != 0 || g_push_layer_calls != 0) {
        fprintf(stderr,
                "expected zero callbacks against a non-COLR font, got color=%d push_layer=%d\n",
                g_color_calls, g_push_layer_calls);
        return 5;
    }

    /* Symbol smoke: every setter must be reachable. */
    hb_paint_funcs_set_push_transform_func(funcs, NULL);
    hb_paint_funcs_set_pop_transform_func(funcs, NULL);
    hb_paint_funcs_set_push_clip_glyph_func(funcs, NULL);
    hb_paint_funcs_set_pop_clip_func(funcs, NULL);
    hb_paint_funcs_set_pop_layer_func(funcs, NULL);
    hb_paint_funcs_set_linear_gradient_func(funcs, NULL);
    hb_paint_funcs_set_radial_gradient_func(funcs, NULL);
    hb_paint_funcs_set_sweep_gradient_func(funcs, NULL);

    /* Set / has / population on a fresh hb_set_t: proves the set
     * API symbols all link. */
    hb_set_t *set = hb_set_create();
    hb_set_add(set, 7);
    hb_set_add(set, 11);
    if (hb_set_has(set, 7) != 1 || hb_set_has(set, 9) != 0) {
        fprintf(stderr, "hb_set_has misbehaved\n");
        return 6;
    }
    if (hb_set_get_population(set) != 2) {
        fprintf(stderr, "hb_set_get_population != 2\n");
        return 7;
    }
    hb_codepoint_t cp = HB_SET_VALUE_INVALID;
    if (hb_set_next(set, &cp) != 1 || cp != 7) {
        fprintf(stderr, "hb_set_next first iteration unexpected\n");
        return 8;
    }
    if (hb_set_next(set, &cp) != 1 || cp != 11) {
        fprintf(stderr, "hb_set_next second iteration unexpected\n");
        return 9;
    }
    if (hb_set_next(set, &cp) != 0) {
        fprintf(stderr, "hb_set_next did not terminate\n");
        return 10;
    }
    hb_set_destroy(set);

    hb_paint_funcs_destroy(funcs);
    hb_font_destroy(font);
    hb_face_destroy(face);
    hb_blob_destroy(blob);
    return 0;
}
