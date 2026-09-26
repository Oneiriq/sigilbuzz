/*
 * test_paint.c: drives the hb_paint_* surface from C, against the same
 * declarations HarfBuzz's hb-paint.h makes.
 *
 * - Setters take (funcs, func, user_data, destroy). Every callback gets
 *   its own user_data back. destroy runs when a callback is replaced,
 *   right away for a NULL func or an immutable funcs object, and once
 *   per slot when the funcs object is freed.
 * - A glyph without color data (Open Sans, argv[1]) paints
 *   push_clip_glyph(glyph, font), color(1, foreground), pop_clip.
 * - A COLRv1 glyph built in memory paints HarfBuzz's sequence: root
 *   transform, push_group/pop_group(mode) around composites, inverse
 *   root / clip / root around PaintGlyph, biased sweep angles, color
 *   lines readable through both the accessor and the struct fields,
 *   palette fallback to the foreground, and custom_palette_color.
 * - HB_COLOR packs blue high and alpha low, as in HarfBuzz.
 *
 * Exit 0 means PASS.
 */

#include "hb.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

#define CHECK(cond, ...)                                          \
    do {                                                          \
        if (!(cond)) {                                            \
            fprintf(stderr, "FAIL line %d: ", __LINE__);          \
            fprintf(stderr, __VA_ARGS__);                         \
            fprintf(stderr, "\n");                                \
            failures += 1;                                        \
        }                                                         \
    } while (0)

/* ---------- Recording callbacks ---------- */

typedef struct {
    char log[4096];
    hb_font_t *expected_font;
    int wrong_user_data;
    int wrong_font;
} recorder_t;

/* One distinct user_data per callback. */
static int tag_push_transform, tag_pop_transform, tag_push_clip_glyph,
    tag_pop_clip, tag_color, tag_linear, tag_radial, tag_sweep,
    tag_push_group, tag_pop_group, tag_custom;

static void append(recorder_t *r, const char *s) {
    size_t used = strlen(r->log);
    if (used + strlen(s) + 2 < sizeof r->log) {
        if (used) strcat(r->log, " ");
        strcat(r->log, s);
    }
}

static void check_tag(recorder_t *r, void *user_data, int *tag) {
    if (user_data != (void *)tag) r->wrong_user_data += 1;
}

static void on_push_transform(hb_paint_funcs_t *funcs, void *paint_data,
                              float xx, float yx, float xy, float yy,
                              float dx, float dy, void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    char buf[128];
    (void)funcs;
    check_tag(r, user_data, &tag_push_transform);
    snprintf(buf, sizeof buf, "T[%g %g %g %g %g %g]", xx, yx, xy, yy, dx, dy);
    append(r, buf);
}

static void on_pop_transform(hb_paint_funcs_t *funcs, void *paint_data,
                             void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    (void)funcs;
    check_tag(r, user_data, &tag_pop_transform);
    append(r, "P");
}

static void on_push_clip_glyph(hb_paint_funcs_t *funcs, void *paint_data,
                               hb_codepoint_t glyph, hb_font_t *font,
                               void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    char buf[32];
    (void)funcs;
    check_tag(r, user_data, &tag_push_clip_glyph);
    if (font != r->expected_font) r->wrong_font += 1;
    snprintf(buf, sizeof buf, "C%u", (unsigned)glyph);
    append(r, buf);
}

static void on_pop_clip(hb_paint_funcs_t *funcs, void *paint_data,
                        void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    (void)funcs;
    check_tag(r, user_data, &tag_pop_clip);
    append(r, "PC");
}

static void on_color(hb_paint_funcs_t *funcs, void *paint_data,
                     hb_bool_t is_foreground, hb_color_t color,
                     void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    char buf[32];
    (void)funcs;
    check_tag(r, user_data, &tag_color);
    snprintf(buf, sizeof buf, "K%d/%08x", is_foreground ? 1 : 0, (unsigned)color);
    append(r, buf);
}

/* Reads the stops twice: through the accessor and through the struct's
 * own function pointer, which C code built against HarfBuzz may do. */
static void log_line(recorder_t *r, hb_color_line_t *line) {
    hb_color_stop_t stops[8];
    hb_color_stop_t direct[8];
    unsigned int count = 8, direct_count = 8, i;
    unsigned int total = hb_color_line_get_color_stops(line, 0, &count, stops);
    unsigned int total_direct = line->get_color_stops(
        line, line->data, 0, &direct_count, direct,
        line->get_color_stops_user_data);
    char buf[64];
    if (total != total_direct || count != direct_count ||
        memcmp(stops, direct, count * sizeof stops[0]) != 0)
        append(r, "LINE-MISMATCH");
    snprintf(buf, sizeof buf, "e%d n%u", (int)hb_color_line_get_extend(line), total);
    append(r, buf);
    for (i = 0; i < count; i++) {
        snprintf(buf, sizeof buf, "s%g:%d/%08x", stops[i].offset,
                 stops[i].is_foreground ? 1 : 0, (unsigned)stops[i].color);
        append(r, buf);
    }
}

static void on_linear(hb_paint_funcs_t *funcs, void *paint_data,
                      hb_color_line_t *color_line, float x0, float y0,
                      float x1, float y1, float x2, float y2,
                      void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    char buf[128];
    (void)funcs;
    check_tag(r, user_data, &tag_linear);
    snprintf(buf, sizeof buf, "L[%g %g %g %g %g %g]", x0, y0, x1, y1, x2, y2);
    append(r, buf);
    log_line(r, color_line);
}

static void on_radial(hb_paint_funcs_t *funcs, void *paint_data,
                      hb_color_line_t *color_line, float x0, float y0,
                      float r0, float x1, float y1, float r1,
                      void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    char buf[128];
    (void)funcs;
    check_tag(r, user_data, &tag_radial);
    snprintf(buf, sizeof buf, "R[%g %g %g %g %g %g]", x0, y0, r0, x1, y1, r1);
    append(r, buf);
    log_line(r, color_line);
}

static void on_sweep(hb_paint_funcs_t *funcs, void *paint_data,
                     hb_color_line_t *color_line, float x0, float y0,
                     float start_angle, float end_angle, void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    char buf[128];
    (void)funcs;
    check_tag(r, user_data, &tag_sweep);
    /* HarfBuzz reports (stored + 1) * pi. */
    snprintf(buf, sizeof buf, "W[%g %g %.4f %.4f]", x0, y0, start_angle, end_angle);
    append(r, buf);
    log_line(r, color_line);
}

static void on_push_group(hb_paint_funcs_t *funcs, void *paint_data,
                          void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    (void)funcs;
    check_tag(r, user_data, &tag_push_group);
    append(r, "G");
}

static void on_pop_group(hb_paint_funcs_t *funcs, void *paint_data,
                         hb_paint_composite_mode_t mode, void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    char buf[32];
    (void)funcs;
    check_tag(r, user_data, &tag_pop_group);
    snprintf(buf, sizeof buf, "PG%d", (int)mode);
    append(r, buf);
}

/* Overrides palette entry 1 with an opaque 0xAABBCC (b, g, r). */
static hb_bool_t on_custom_palette(hb_paint_funcs_t *funcs, void *paint_data,
                                   unsigned int color_index, hb_color_t *color,
                                   void *user_data) {
    recorder_t *r = (recorder_t *)paint_data;
    (void)funcs;
    check_tag(r, user_data, &tag_custom);
    if (color_index != 1) return 0;
    *color = HB_COLOR(0xAA, 0xBB, 0xCC, 0xFF);
    return 1;
}

static hb_paint_funcs_t *recording_funcs(void) {
    hb_paint_funcs_t *f = hb_paint_funcs_create();
    hb_paint_funcs_set_push_transform_func(f, on_push_transform, &tag_push_transform, NULL);
    hb_paint_funcs_set_pop_transform_func(f, on_pop_transform, &tag_pop_transform, NULL);
    hb_paint_funcs_set_push_clip_glyph_func(f, on_push_clip_glyph, &tag_push_clip_glyph, NULL);
    hb_paint_funcs_set_pop_clip_func(f, on_pop_clip, &tag_pop_clip, NULL);
    hb_paint_funcs_set_color_func(f, on_color, &tag_color, NULL);
    hb_paint_funcs_set_linear_gradient_func(f, on_linear, &tag_linear, NULL);
    hb_paint_funcs_set_radial_gradient_func(f, on_radial, &tag_radial, NULL);
    hb_paint_funcs_set_sweep_gradient_func(f, on_sweep, &tag_sweep, NULL);
    hb_paint_funcs_set_push_group_func(f, on_push_group, &tag_push_group, NULL);
    hb_paint_funcs_set_pop_group_func(f, on_pop_group, &tag_pop_group, NULL);
    return f;
}

static void paint(recorder_t *r, hb_font_t *font, hb_codepoint_t glyph,
                  hb_paint_funcs_t *funcs, unsigned int palette,
                  hb_color_t foreground) {
    memset(r, 0, sizeof *r);
    r->expected_font = font;
    hb_font_paint_glyph(font, glyph, funcs, r, palette, foreground);
}

/* ---------- In-memory COLRv1 font ---------- */

typedef struct {
    unsigned char b[512];
    size_t len;
} bytes_t;

static void put8(bytes_t *o, unsigned v) { o->b[o->len++] = (unsigned char)v; }
static void put16(bytes_t *o, unsigned v) { put8(o, v >> 8); put8(o, v); }
static void put24(bytes_t *o, unsigned long v) { put8(o, (unsigned)(v >> 16)); put16(o, (unsigned)v & 0xFFFF); }
static void put32(bytes_t *o, unsigned long v) { put16(o, (unsigned)(v >> 16)); put16(o, (unsigned)v & 0xFFFF); }
static void put_bytes(bytes_t *o, const bytes_t *src) {
    memcpy(o->b + o->len, src->b, src->len);
    o->len += src->len;
}
#define F2DOT14(v) ((unsigned)((int)((v) * 16384.0) & 0xFFFF))

/*
 * COLR v1 (spec layout, 34-byte header) with one base glyph, 7:
 *   PaintComposite(mode MULTIPLY = 23)
 *     source:   PaintGlyph(2) -> PaintSolid(entry 1, alpha 0.5)
 *     backdrop: PaintGlyph(1) -> PaintSweepGradient(center (50, 60),
 *               stored angles -1 and 0.5, extend REPEAT,
 *               stops (0, entry 0, alpha 1), (1, entry 0xFFFF, alpha 0.5))
 * CPAL: one palette, entry 0 red, entry 1 green.
 */
static void build_font(bytes_t *font) {
    bytes_t colr = {{0}, 0}, cpal = {{0}, 0};

    /* COLR header */
    put16(&colr, 1);  /* version */
    put16(&colr, 0);  /* numBaseGlyphRecords */
    put32(&colr, 34); /* baseGlyphRecordsOffset */
    put32(&colr, 34); /* layerRecordsOffset */
    put16(&colr, 0);  /* numLayerRecords */
    put32(&colr, 34); /* baseGlyphListOffset */
    put32(&colr, 0);  /* layerListOffset */
    put32(&colr, 0);  /* clipListOffset */
    put32(&colr, 0);  /* varIndexMapOffset */
    put32(&colr, 0);  /* itemVariationStoreOffset */
    /* BaseGlyphList: one record, paint right after it (offset 10). */
    put32(&colr, 1);
    put16(&colr, 7);
    put32(&colr, 10);
    /* +0 PaintComposite: source at +8, mode, backdrop at +19. */
    put8(&colr, 32); put24(&colr, 8); put8(&colr, 23); put24(&colr, 19);
    /* +8 PaintGlyph(2), child at +6 (= +14). */
    put8(&colr, 10); put24(&colr, 6); put16(&colr, 2);
    /* +14 PaintSolid(entry 1, alpha 0.5). */
    put8(&colr, 2); put16(&colr, 1); put16(&colr, F2DOT14(0.5));
    /* +19 PaintGlyph(1), child at +6 (= +25). */
    put8(&colr, 10); put24(&colr, 6); put16(&colr, 1);
    /* +25 PaintSweepGradient, color line at +12 (= +37). */
    put8(&colr, 8); put24(&colr, 12); put16(&colr, 50); put16(&colr, 60);
    put16(&colr, F2DOT14(-1.0)); put16(&colr, F2DOT14(0.5));
    /* +37 ColorLine: REPEAT, two stops. */
    put8(&colr, 1); put16(&colr, 2);
    put16(&colr, F2DOT14(0.0)); put16(&colr, 0); put16(&colr, F2DOT14(1.0));
    put16(&colr, F2DOT14(1.0)); put16(&colr, 0xFFFF); put16(&colr, F2DOT14(0.5));

    /* CPAL v0: one palette of two entries, records as b, g, r, a. */
    put16(&cpal, 0); put16(&cpal, 2); put16(&cpal, 1); put16(&cpal, 2);
    put32(&cpal, 14); put16(&cpal, 0);
    put8(&cpal, 0); put8(&cpal, 0); put8(&cpal, 255); put8(&cpal, 255);
    put8(&cpal, 0); put8(&cpal, 255); put8(&cpal, 0); put8(&cpal, 255);

    /* SFNT directory: COLR then CPAL. */
    font->len = 0;
    put32(font, 0x00010000UL); put16(font, 2); put16(font, 0); put16(font, 0); put16(font, 0);
    put8(font, 'C'); put8(font, 'O'); put8(font, 'L'); put8(font, 'R');
    put32(font, 0); put32(font, 44); put32(font, (unsigned long)colr.len);
    put8(font, 'C'); put8(font, 'P'); put8(font, 'A'); put8(font, 'L');
    put32(font, 0); put32(font, 44 + (unsigned long)colr.len); put32(font, (unsigned long)cpal.len);
    put_bytes(font, &colr);
    put_bytes(font, &cpal);
}

/* ---------- Destroy callbacks ---------- */

typedef struct {
    int destroyed;
} slot_data_t;

static void count_destroy(void *user_data) {
    ((slot_data_t *)user_data)->destroyed += 1;
}

static void noop_bare(hb_paint_funcs_t *funcs, void *paint_data, void *user_data) {
    (void)funcs; (void)paint_data; (void)user_data;
}

static void test_destroy_callbacks(void) {
    slot_data_t a = {0}, b = {0}, c = {0}, d = {0}, e = {0}, g = {0};
    hb_paint_funcs_t *f = hb_paint_funcs_create();
    hb_paint_funcs_t *extra;

    hb_paint_funcs_set_pop_clip_func(f, noop_bare, &a, count_destroy);
    CHECK(a.destroyed == 0, "installing must not destroy");

    /* Replacing destroys the old user_data. */
    hb_paint_funcs_set_pop_clip_func(f, noop_bare, &b, count_destroy);
    CHECK(a.destroyed == 1 && b.destroyed == 0, "replace: a=%d b=%d", a.destroyed, b.destroyed);

    /* A NULL func destroys the new user_data at once and releases the old. */
    hb_paint_funcs_set_push_group_func(f, noop_bare, &c, count_destroy);
    hb_paint_funcs_set_push_group_func(f, NULL, &d, count_destroy);
    CHECK(c.destroyed == 1 && d.destroyed == 1, "NULL func: c=%d d=%d", c.destroyed, d.destroyed);

    hb_paint_funcs_set_pop_transform_func(f, noop_bare, &e, count_destroy);

    /* An extra reference does not free anything. */
    extra = hb_paint_funcs_reference(f);
    CHECK(extra == f, "reference must return the same pointer");
    hb_paint_funcs_destroy(extra);
    CHECK(b.destroyed == 0 && e.destroyed == 0, "early destroy through a reference");

    /* Immutable objects refuse setters and destroy the new user_data. */
    CHECK(!hb_paint_funcs_is_immutable(f), "fresh funcs is mutable");
    hb_paint_funcs_make_immutable(f);
    CHECK(hb_paint_funcs_is_immutable(f), "make_immutable did not stick");
    hb_paint_funcs_set_pop_clip_func(f, noop_bare, &g, count_destroy);
    CHECK(g.destroyed == 1 && b.destroyed == 0, "immutable: g=%d b=%d", g.destroyed, b.destroyed);

    /* Freeing destroys every installed user_data exactly once. */
    hb_paint_funcs_destroy(f);
    CHECK(a.destroyed == 1 && b.destroyed == 1 && c.destroyed == 1 &&
              d.destroyed == 1 && e.destroyed == 1 && g.destroyed == 1,
          "after free: a=%d b=%d c=%d d=%d e=%d g=%d", a.destroyed, b.destroyed,
          c.destroyed, d.destroyed, e.destroyed, g.destroyed);
}

/* ---------- main ---------- */

int main(int argc, char **argv) {
    const hb_color_t fg = HB_COLOR(0x10, 0x20, 0x30, 0xFF);
    recorder_t *r = (recorder_t *)calloc(1, sizeof *r);
    hb_paint_funcs_t *funcs;
    hb_blob_t *blob;
    hb_face_t *face;
    hb_font_t *font;
    bytes_t colr_font;
    char expected[1024];

    if (argc < 2 || !r) {
        fprintf(stderr, "usage: %s <font.ttf>\n", argv[0]);
        return 2;
    }

    /* HB_COLOR / hb_color_get_*: blue high, alpha low. */
    {
        hb_color_t probe = HB_COLOR(0x11, 0x22, 0x33, 0x44);
        CHECK(probe == 0x11223344u, "HB_COLOR byte order: %08x", (unsigned)probe);
        CHECK(hb_color_get_blue(probe) == 0x11 && hb_color_get_alpha(probe) == 0x44,
              "hb_color_get_* macros");
        CHECK((hb_color_get_red)(probe) == 0x33 && (hb_color_get_green)(probe) == 0x22 &&
                  (hb_color_get_blue)(probe) == 0x11 && (hb_color_get_alpha)(probe) == 0x44,
              "hb_color_get_* functions");
    }

    test_destroy_callbacks();

    /* The color-line accessors accept NULL. */
    CHECK(hb_color_line_get_color_stops(NULL, 0, NULL, NULL) == 0 &&
              hb_color_line_get_extend(NULL) == HB_PAINT_EXTEND_PAD,
          "NULL color line accessors misbehaved");

    funcs = recording_funcs();

    /* A monochrome glyph: clip to its outline, fill with the foreground. */
    blob = hb_blob_create_from_file(argv[1]);
    if (!blob || hb_blob_get_length(blob) == 0) {
        fprintf(stderr, "could not load font %s\n", argv[1]);
        return 3;
    }
    face = hb_face_create(blob, 0);
    font = hb_font_create(face);
    paint(r, font, 36, funcs, 0, HB_COLOR(1, 2, 3, 0x40));
    CHECK(strcmp(r->log, "C36 K1/01020340 PC") == 0, "fallback: %s", r->log);
    CHECK(r->wrong_user_data == 0 && r->wrong_font == 0, "fallback user_data/font");
    hb_font_destroy(font);
    hb_face_destroy(face);
    hb_blob_destroy(blob);

    /* The in-memory COLRv1 font. No head table: upem 1000, scale 1000. */
    build_font(&colr_font);
    blob = hb_blob_create((const char *)colr_font.b, (unsigned int)colr_font.len,
                          HB_MEMORY_MODE_DUPLICATE, NULL, NULL);
    face = hb_face_create(blob, 0);
    hb_blob_destroy(blob);
    font = hb_font_create(face);
    hb_face_destroy(face);

#define ID "T[1 0 0 1 0 0]"
    snprintf(expected, sizeof expected,
             ID " G " ID " C1 " ID
             " W[50 60 0.0000 4.7124] e1 n2 s0:0/0000ffff s1:1/1020307f"
             " P PC P G " ID " C2 " ID " K0/00ff007f P PC P PG23 PG3 P");
    paint(r, font, 7, funcs, 0, fg);
    CHECK(strcmp(r->log, expected) == 0, "COLRv1:\n got  %s\n want %s", r->log, expected);
    CHECK(r->wrong_user_data == 0 && r->wrong_font == 0, "COLRv1 user_data/font");

    /* A palette the font lacks: every palette entry is the foreground,
     * unflagged, alpha applied and truncated. */
    paint(r, font, 7, funcs, 3, fg);
    CHECK(strstr(r->log, "s0:0/102030ff s1:1/1020307f") != NULL, "palette 3 stops: %s", r->log);
    CHECK(strstr(r->log, "K0/1020307f") != NULL, "palette 3 solid: %s", r->log);

    /* custom_palette_color overrides entry 1. */
    hb_paint_funcs_set_custom_palette_color_func(funcs, on_custom_palette, &tag_custom, NULL);
    paint(r, font, 7, funcs, 0, fg);
    CHECK(strstr(r->log, "K0/aabbcc7f") != NULL, "custom palette: %s", r->log);
    CHECK(strstr(r->log, "s0:0/0000ffff") != NULL, "entry 0 still from CPAL: %s", r->log);
    CHECK(r->wrong_user_data == 0, "custom palette user_data");

    /* The root transform follows the font scale. */
    hb_font_set_scale(font, 2000, 500);
    paint(r, font, 7, funcs, 0, fg);
    {
        const char *want = "T[2 0 0 0.5 0 0] G T[0.5 0 0 2 0 0] C1 T[2 0 0 0.5 0 0]";
        CHECK(strncmp(r->log, want, strlen(want)) == 0, "scaled: %s", r->log);
    }
#undef ID

    /* The remaining setters exist and accept NULL. */
    hb_paint_funcs_set_push_clip_rectangle_func(funcs, NULL, NULL, NULL);
    hb_paint_funcs_set_image_func(funcs, NULL, NULL, NULL);

    /* Set / has / population on a fresh hb_set_t: proves the set API
     * symbols all link. */
    {
        hb_set_t *set = hb_set_create();
        hb_codepoint_t cp = HB_SET_VALUE_INVALID;
        hb_set_add(set, 7);
        hb_set_add(set, 11);
        CHECK(hb_set_has(set, 7) == 1 && hb_set_has(set, 9) == 0, "hb_set_has");
        CHECK(hb_set_get_population(set) == 2, "hb_set_get_population");
        CHECK(hb_set_next(set, &cp) == 1 && cp == 7, "hb_set_next 1");
        CHECK(hb_set_next(set, &cp) == 1 && cp == 11, "hb_set_next 2");
        CHECK(hb_set_next(set, &cp) == 0, "hb_set_next end");
        hb_set_destroy(set);
    }

    hb_paint_funcs_destroy(funcs);
    hb_font_destroy(font);
    free(r);

    if (failures) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    return 0;
}
