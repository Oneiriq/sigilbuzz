/*
 * hb.h: sigilbuzz's HarfBuzz-symbol-compatible C header.
 *
 * Subset of the upstream HarfBuzz public API covering the symbols
 * sigilbuzz-capi implements. A binary previously compiled against
 * `<harfbuzz/hb.h>` will link against `libsigilbuzz` because every
 * `hb_*` symbol resolves to the same name and the same enum/struct
 * layout used here.
 *
 * Constants and struct layouts are kept verbatim from the HarfBuzz
 * spec (HB_DIRECTION_LTR == 4, HB_SCRIPT_LATIN == HB_TAG('L','a','t','n'),
 * etc.) so a binary compiled against the original `hb.h` continues to
 * work without re-linking against this header.
 */

#ifndef SIGILBUZZ_HB_H
#define SIGILBUZZ_HB_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---------- Primitive types ---------- */

typedef int           hb_bool_t;
typedef uint32_t      hb_codepoint_t;
typedef int32_t       hb_position_t;
typedef uint32_t      hb_mask_t;
typedef uint32_t      hb_tag_t;

#define HB_TAG(c1,c2,c3,c4) \
    ((hb_tag_t)((((uint32_t)(c1)&0xFFu)<<24) | \
                (((uint32_t)(c2)&0xFFu)<<16) | \
                (((uint32_t)(c3)&0xFFu)<<8)  | \
                ((uint32_t)(c4)&0xFFu)))

typedef void (*hb_destroy_func_t)(void *user_data);

/* ---------- Memory mode ---------- */

typedef enum {
    HB_MEMORY_MODE_DUPLICATE                    = 0,
    HB_MEMORY_MODE_READONLY                     = 1,
    HB_MEMORY_MODE_WRITABLE                     = 2,
    HB_MEMORY_MODE_READONLY_MAY_MAKE_WRITABLE   = 3
} hb_memory_mode_t;

/* ---------- Direction ---------- */

typedef enum {
    HB_DIRECTION_INVALID = 0,
    HB_DIRECTION_LTR     = 4,
    HB_DIRECTION_RTL     = 5,
    HB_DIRECTION_TTB     = 6,
    HB_DIRECTION_BTT     = 7
} hb_direction_t;

/* ---------- Script (alias for hb_tag_t) ---------- */

typedef hb_tag_t hb_script_t;

#define HB_SCRIPT_INVALID    ((hb_script_t)0)
#define HB_SCRIPT_COMMON     HB_TAG('Z','y','y','y')
#define HB_SCRIPT_INHERITED  HB_TAG('Z','i','n','h')
#define HB_SCRIPT_LATIN      HB_TAG('L','a','t','n')
#define HB_SCRIPT_GREEK      HB_TAG('G','r','e','k')
#define HB_SCRIPT_CYRILLIC   HB_TAG('C','y','r','l')
#define HB_SCRIPT_ARABIC     HB_TAG('A','r','a','b')
#define HB_SCRIPT_HEBREW     HB_TAG('H','e','b','r')
#define HB_SCRIPT_DEVANAGARI HB_TAG('D','e','v','a')
#define HB_SCRIPT_BENGALI    HB_TAG('B','e','n','g')
#define HB_SCRIPT_HAN        HB_TAG('H','a','n','i')
#define HB_SCRIPT_HANGUL     HB_TAG('H','a','n','g')
#define HB_SCRIPT_KHMER      HB_TAG('K','h','m','r')
#define HB_SCRIPT_MYANMAR    HB_TAG('M','y','m','r')
#define HB_SCRIPT_THAI       HB_TAG('T','h','a','i')
#define HB_SCRIPT_LAO        HB_TAG('L','a','o','o')

/* ---------- Language (opaque interned pointer) ---------- */

typedef const char *hb_language_t;

/* ---------- Opaque handles ----------
 *
 * Ownership follows HarfBuzz exactly: the pointer is the object.
 *
 *   - Every *_create call and every *_reference call gives the caller
 *     one reference, which it releases with the matching *_destroy.
 *   - hb_x_reference(p) returns p itself (not a copy), so
 *     `hb_blob_reference(b); ... hb_blob_destroy(b); hb_blob_destroy(b);`
 *     is balanced.
 *   - Referencing NULL returns NULL; destroying NULL does nothing.
 *   - A face references its blob and a font references its face, so the
 *     caller may destroy a blob right after hb_face_create() and a face
 *     right after hb_font_create().
 *   - hb_subset_input_unicode_set() / hb_subset_input_glyph_set() return
 *     a set owned by the input: never destroy it (see below).
 *   - Where HarfBuzz would return its inert "empty" object (for example
 *     from hb_blob_create() with length 0), sigilbuzz returns a fresh
 *     empty object. Destroy it as usual; the same code is correct with
 *     HarfBuzz, where destroying the inert object is a no-op.
 *
 * References may be taken and released from any thread. */

typedef struct hb_blob_t   hb_blob_t;
typedef struct hb_face_t   hb_face_t;
typedef struct hb_font_t   hb_font_t;
typedef struct hb_buffer_t hb_buffer_t;

/* ---------- Glyph info / position ---------- */

typedef struct hb_glyph_info_t {
    hb_codepoint_t codepoint;
    hb_mask_t      mask;
    uint32_t       cluster;
    uint32_t       var1;
    uint32_t       var2;
} hb_glyph_info_t;

typedef struct hb_glyph_position_t {
    hb_position_t x_advance;
    hb_position_t y_advance;
    hb_position_t x_offset;
    hb_position_t y_offset;
    uint32_t      var;
} hb_glyph_position_t;

/* ---------- Feature / variation ---------- */

typedef struct hb_feature_t {
    hb_tag_t tag;
    uint32_t value;
    unsigned int start;
    unsigned int end;
} hb_feature_t;

typedef struct hb_variation_t {
    hb_tag_t tag;
    float    value;
} hb_variation_t;

/* ---------- Blob ---------- */

/* sigilbuzz always copies `data`. `destroy` runs immediately when the
 * blob ends up empty (length 0 or NULL data) or for
 * HB_MEMORY_MODE_DUPLICATE, and otherwise once the last reference to
 * the blob (including those held by faces) is gone, as in HarfBuzz. */
hb_blob_t *hb_blob_create(const char       *data,
                          unsigned int      length,
                          hb_memory_mode_t  mode,
                          void             *user_data,
                          hb_destroy_func_t destroy);

hb_blob_t *hb_blob_create_from_file(const char *file_name);

void       hb_blob_destroy(hb_blob_t *blob);
hb_blob_t *hb_blob_reference(hb_blob_t *blob);
const char *hb_blob_get_data(hb_blob_t *blob, unsigned int *length);
unsigned int hb_blob_get_length(hb_blob_t *blob);

/* ---------- Face ---------- */

hb_face_t *hb_face_create(hb_blob_t *blob, unsigned int index);
void       hb_face_destroy(hb_face_t *face);
hb_face_t *hb_face_reference(hb_face_t *face);
unsigned int hb_face_get_glyph_count(hb_face_t *face);
unsigned int hb_face_get_upem(hb_face_t *face);

/* ---------- Font ---------- */

hb_font_t *hb_font_create(hb_face_t *face);
void       hb_font_destroy(hb_font_t *font);
hb_font_t *hb_font_reference(hb_font_t *font);
void       hb_font_set_scale(hb_font_t *font, int x_scale, int y_scale);
void       hb_font_get_scale(hb_font_t *font, int *x_scale, int *y_scale);
void       hb_font_set_ppem(hb_font_t *font, unsigned int x_ppem, unsigned int y_ppem);
void       hb_font_set_variations(hb_font_t            *font,
                                  const hb_variation_t *variations,
                                  unsigned int          variations_length);

/* ---------- Buffer ---------- */

hb_buffer_t *hb_buffer_create(void);
void         hb_buffer_destroy(hb_buffer_t *buffer);
hb_buffer_t *hb_buffer_reference(hb_buffer_t *buffer);
void         hb_buffer_reset(hb_buffer_t *buffer);
/* Drops the text and output and, as in HarfBuzz, resets direction,
 * script, language, and the pre- and post-context. */
void         hb_buffer_clear_contents(hb_buffer_t *buffer);

/* Adds text[item_offset, item_offset + item_length) (item_length -1
 * means to the end). As in HarfBuzz: glyph clusters are offsets into
 * `text` in its own code units (bytes for UTF-8 and Latin-1, 16-bit
 * units for UTF-16, 32-bit units for UTF-32 and code points), up to
 * five characters before the item become the pre-context when the
 * buffer is empty, up to five after it become the post-context, and
 * malformed sequences become U+FFFD. */
void         hb_buffer_add_utf8(hb_buffer_t *buffer,
                                const char  *text,
                                int          text_length,
                                unsigned int item_offset,
                                int          item_length);

void         hb_buffer_add_utf16(hb_buffer_t    *buffer,
                                 const uint16_t *text,
                                 int             text_length,
                                 unsigned int    item_offset,
                                 int             item_length);

/* Surrogates and values past U+10FFFF become U+FFFD. */
void         hb_buffer_add_utf32(hb_buffer_t    *buffer,
                                 const uint32_t *text,
                                 int             text_length,
                                 unsigned int    item_offset,
                                 int             item_length);

/* HarfBuzz passes code points through unchecked; sigilbuzz stores
 * text as Unicode scalar values, so invalid ones become U+FFFD as in
 * hb_buffer_add_utf32. */
void         hb_buffer_add_codepoints(hb_buffer_t          *buffer,
                                      const hb_codepoint_t *text,
                                      int                   text_length,
                                      unsigned int          item_offset,
                                      int                   item_length);

/* Each byte is the code point of the same value (U+0000..U+00FF). */
void         hb_buffer_add_latin1(hb_buffer_t   *buffer,
                                  const uint8_t *text,
                                  int            text_length,
                                  unsigned int   item_offset,
                                  int            item_length);

/* HB_DIRECTION_INVALID returns the buffer to an unset direction, so
 * the shaper chooses the layout again (vertical for Mongolian-dominant
 * text, left to right otherwise). */
void         hb_buffer_set_direction(hb_buffer_t *buffer, hb_direction_t direction);
/* The whole buffer shapes as `script`. Scripts sigilbuzz has no shaper
 * for (and Common, Inherited, Unknown) leave the text split into
 * per-script runs instead. */
void         hb_buffer_set_script(hb_buffer_t *buffer, hb_script_t script);
/* Selects the OpenType language system (a font's `locl` forms, ...). */
void         hb_buffer_set_language(hb_buffer_t *buffer, hb_language_t language);
/* Fills unset properties in HarfBuzz's order: the script of the first
 * character that has one, the direction from that script (right to
 * left for Arabic, Hebrew, ...), and "und" for the language. */
void         hb_buffer_guess_segment_properties(hb_buffer_t *buffer);

hb_glyph_info_t     *hb_buffer_get_glyph_infos(hb_buffer_t *buffer, unsigned int *length);
hb_glyph_position_t *hb_buffer_get_glyph_positions(hb_buffer_t *buffer, unsigned int *length);
unsigned int         hb_buffer_get_length(hb_buffer_t *buffer);

/* ---------- Shape ---------- */

void      hb_shape(hb_font_t          *font,
                   hb_buffer_t        *buffer,
                   const hb_feature_t *features,
                   unsigned int        num_features);

hb_bool_t hb_shape_full(hb_font_t           *font,
                        hb_buffer_t         *buffer,
                        const hb_feature_t  *features,
                        unsigned int         num_features,
                        const char * const  *shaper_list);

/* ---------- Tag / Direction / Script / Language helpers ---------- */

hb_tag_t       hb_tag_from_string(const char *str, int len);
void           hb_tag_to_string(hb_tag_t tag, char *buf);
hb_direction_t hb_direction_from_string(const char *str, int len);
hb_script_t    hb_script_from_iso15924_tag(hb_tag_t tag);
hb_language_t  hb_language_from_string(const char *str, int len);

/* ---------- Version ---------- */

void        hb_version(unsigned int *major,
                       unsigned int *minor,
                       unsigned int *micro);
const char *hb_version_string(void);

/* ---------- hb_set_t (opaque integer set) ---------- */

typedef struct hb_set_t hb_set_t;

#define HB_SET_VALUE_INVALID ((hb_codepoint_t)0xFFFFFFFFu)

hb_set_t    *hb_set_create(void);
void         hb_set_destroy(hb_set_t *set);
hb_set_t    *hb_set_reference(hb_set_t *set);
void         hb_set_add(hb_set_t *set, hb_codepoint_t codepoint);
void         hb_set_del(hb_set_t *set, hb_codepoint_t codepoint);
hb_bool_t    hb_set_has(const hb_set_t *set, hb_codepoint_t codepoint);
unsigned int hb_set_get_population(const hb_set_t *set);
hb_bool_t    hb_set_next(const hb_set_t *set, hb_codepoint_t *codepoint);

/* ---------- Subset (gated on the `subset` cargo feature) ---------- */

typedef struct hb_subset_input_t hb_subset_input_t;

hb_subset_input_t *hb_subset_input_create(void);
/* HarfBuzz's name. Never returns NULL in sigilbuzz. */
hb_subset_input_t *hb_subset_input_create_or_fail(void);
hb_subset_input_t *hb_subset_input_reference(hb_subset_input_t *input);
void               hb_subset_input_destroy(hb_subset_input_t *input);

/* The returned sets are owned by the input ("transfer none"): every call
 * returns the same pointer, valid until the input is destroyed. Do NOT
 * call hb_set_destroy() on them. Adding to or removing from them changes
 * what hb_subset_or_fail() keeps. Call hb_set_reference() to keep one
 * past the input's lifetime. Return NULL for a NULL input. */
hb_set_t          *hb_subset_input_unicode_set(hb_subset_input_t *input);
hb_set_t          *hb_subset_input_glyph_set(hb_subset_input_t *input);

/* Returns a new face reference (destroy it), or NULL on failure. */
hb_face_t         *hb_subset_or_fail(hb_face_t *face, hb_subset_input_t *input);

/* ---------- Paint (gated on the `paint` cargo feature) ----------
 *
 * The declarations below match HarfBuzz's hb-paint.h (8.0 API; the 8.2
 * color_glyph callback is not provided). Every callback receives the
 * funcs object, the `paint_data` passed to hb_font_paint_glyph(), its
 * arguments, and the `user_data` it was installed with. A setter's
 * `destroy` runs on `user_data` when the callback is replaced, when the
 * funcs object is freed, or right away when `func` is NULL or the funcs
 * object is immutable. */

typedef struct hb_paint_funcs_t hb_paint_funcs_t;

/* Colors are 8 bits per channel, packed as HB_TAG(b, g, r, a): blue in
 * the most significant byte, alpha in the least significant one. */
typedef uint32_t hb_color_t;

#define HB_COLOR(b,g,r,a) ((hb_color_t) HB_TAG ((b),(g),(r),(a)))

uint8_t hb_color_get_alpha(hb_color_t color);
#define hb_color_get_alpha(color) ((color) & 0xFF)
uint8_t hb_color_get_red(hb_color_t color);
#define hb_color_get_red(color)   (((color) >> 8) & 0xFF)
uint8_t hb_color_get_green(hb_color_t color);
#define hb_color_get_green(color) (((color) >> 16) & 0xFF)
uint8_t hb_color_get_blue(hb_color_t color);
#define hb_color_get_blue(color)  (((color) >> 24) & 0xFF)

typedef struct hb_glyph_extents_t {
    hb_position_t x_bearing;
    hb_position_t y_bearing;
    hb_position_t width;
    hb_position_t height;
} hb_glyph_extents_t;

hb_paint_funcs_t *hb_paint_funcs_create(void);
hb_paint_funcs_t *hb_paint_funcs_reference(hb_paint_funcs_t *funcs);
void              hb_paint_funcs_destroy(hb_paint_funcs_t *funcs);
void              hb_paint_funcs_make_immutable(hb_paint_funcs_t *funcs);
hb_bool_t         hb_paint_funcs_is_immutable(hb_paint_funcs_t *funcs);

typedef void (*hb_paint_push_transform_func_t) (hb_paint_funcs_t *funcs,
                                                void *paint_data,
                                                float xx, float yx,
                                                float xy, float yy,
                                                float dx, float dy,
                                                void *user_data);

typedef void (*hb_paint_pop_transform_func_t) (hb_paint_funcs_t *funcs,
                                               void *paint_data,
                                               void *user_data);

/* The clip outline is the glyph as hb_font_draw_glyph() would draw it on
 * `font`, that is at font scale; sigilbuzz does not export
 * hb_font_draw_glyph(). */
typedef void (*hb_paint_push_clip_glyph_func_t) (hb_paint_funcs_t *funcs,
                                                 void *paint_data,
                                                 hb_codepoint_t glyph,
                                                 hb_font_t *font,
                                                 void *user_data);

/* Never called by sigilbuzz yet. */
typedef void (*hb_paint_push_clip_rectangle_func_t) (hb_paint_funcs_t *funcs,
                                                     void *paint_data,
                                                     float xmin, float ymin,
                                                     float xmax, float ymax,
                                                     void *user_data);

typedef void (*hb_paint_pop_clip_func_t) (hb_paint_funcs_t *funcs,
                                          void *paint_data,
                                          void *user_data);

typedef void (*hb_paint_color_func_t) (hb_paint_funcs_t *funcs,
                                       void *paint_data,
                                       hb_bool_t is_foreground,
                                       hb_color_t color,
                                       void *user_data);

#define HB_PAINT_IMAGE_FORMAT_PNG  HB_TAG('p','n','g',' ')
#define HB_PAINT_IMAGE_FORMAT_SVG  HB_TAG('s','v','g',' ')
#define HB_PAINT_IMAGE_FORMAT_BGRA HB_TAG('B','G','R','A')

/* Never called by sigilbuzz: it paints no SVG or bitmap glyphs. */
typedef hb_bool_t (*hb_paint_image_func_t) (hb_paint_funcs_t *funcs,
                                            void *paint_data,
                                            hb_blob_t *image,
                                            unsigned int width,
                                            unsigned int height,
                                            hb_tag_t format,
                                            float slant,
                                            hb_glyph_extents_t *extents,
                                            void *user_data);

/* One resolved gradient stop. `is_foreground` is nonzero for stops on
 * the COLR foreground entry; their `color` is the foreground color
 * passed to hb_font_paint_glyph() with the stop alpha applied. */
typedef struct hb_color_stop_t {
    float      offset;
    hb_bool_t  is_foreground;
    hb_color_t color;
} hb_color_stop_t;

typedef enum {
    HB_PAINT_EXTEND_PAD,
    HB_PAINT_EXTEND_REPEAT,
    HB_PAINT_EXTEND_REFLECT
} hb_paint_extend_t;

typedef struct hb_color_line_t hb_color_line_t;

typedef unsigned int (*hb_color_line_get_color_stops_func_t) (hb_color_line_t *color_line,
                                                              void *color_line_data,
                                                              unsigned int start,
                                                              unsigned int *count,
                                                              hb_color_stop_t *color_stops,
                                                              void *user_data);

typedef hb_paint_extend_t (*hb_color_line_get_extend_func_t) (hb_color_line_t *color_line,
                                                              void *color_line_data,
                                                              void *user_data);

/* A gradient's color line. The lines hb_font_paint_glyph() passes to the
 * gradient callbacks are valid only while the callback runs. */
struct hb_color_line_t {
    void *data;

    hb_color_line_get_color_stops_func_t get_color_stops;
    void *get_color_stops_user_data;

    hb_color_line_get_extend_func_t get_extend;
    void *get_extend_user_data;

    void *reserved0;
    void *reserved1;
    void *reserved2;
    void *reserved3;
    void *reserved5;
    void *reserved6;
    void *reserved7;
    void *reserved8;
};

/* Copies up to *count stops starting at `start` into `color_stops`,
 * stores the number copied in *count, and returns the total number of
 * stops. With a NULL `count` or `color_stops` it only returns the total.
 * Both accessors call through the line's own functions. */
unsigned int      hb_color_line_get_color_stops(hb_color_line_t *color_line,
                                                unsigned int     start,
                                                unsigned int    *count,
                                                hb_color_stop_t *color_stops);
hb_paint_extend_t hb_color_line_get_extend(hb_color_line_t *color_line);

typedef void (*hb_paint_linear_gradient_func_t) (hb_paint_funcs_t *funcs,
                                                 void *paint_data,
                                                 hb_color_line_t *color_line,
                                                 float x0, float y0,
                                                 float x1, float y1,
                                                 float x2, float y2,
                                                 void *user_data);

typedef void (*hb_paint_radial_gradient_func_t) (hb_paint_funcs_t *funcs,
                                                 void *paint_data,
                                                 hb_color_line_t *color_line,
                                                 float x0, float y0, float r0,
                                                 float x1, float y1, float r1,
                                                 void *user_data);

/* Angles are radians, counter-clockwise from the positive x axis. */
typedef void (*hb_paint_sweep_gradient_func_t) (hb_paint_funcs_t *funcs,
                                                void *paint_data,
                                                hb_color_line_t *color_line,
                                                float x0, float y0,
                                                float start_angle,
                                                float end_angle,
                                                void *user_data);

typedef enum {
    HB_PAINT_COMPOSITE_MODE_CLEAR,
    HB_PAINT_COMPOSITE_MODE_SRC,
    HB_PAINT_COMPOSITE_MODE_DEST,
    HB_PAINT_COMPOSITE_MODE_SRC_OVER,
    HB_PAINT_COMPOSITE_MODE_DEST_OVER,
    HB_PAINT_COMPOSITE_MODE_SRC_IN,
    HB_PAINT_COMPOSITE_MODE_DEST_IN,
    HB_PAINT_COMPOSITE_MODE_SRC_OUT,
    HB_PAINT_COMPOSITE_MODE_DEST_OUT,
    HB_PAINT_COMPOSITE_MODE_SRC_ATOP,
    HB_PAINT_COMPOSITE_MODE_DEST_ATOP,
    HB_PAINT_COMPOSITE_MODE_XOR,
    HB_PAINT_COMPOSITE_MODE_PLUS,
    HB_PAINT_COMPOSITE_MODE_SCREEN,
    HB_PAINT_COMPOSITE_MODE_OVERLAY,
    HB_PAINT_COMPOSITE_MODE_DARKEN,
    HB_PAINT_COMPOSITE_MODE_LIGHTEN,
    HB_PAINT_COMPOSITE_MODE_COLOR_DODGE,
    HB_PAINT_COMPOSITE_MODE_COLOR_BURN,
    HB_PAINT_COMPOSITE_MODE_HARD_LIGHT,
    HB_PAINT_COMPOSITE_MODE_SOFT_LIGHT,
    HB_PAINT_COMPOSITE_MODE_DIFFERENCE,
    HB_PAINT_COMPOSITE_MODE_EXCLUSION,
    HB_PAINT_COMPOSITE_MODE_MULTIPLY,
    HB_PAINT_COMPOSITE_MODE_HSL_HUE,
    HB_PAINT_COMPOSITE_MODE_HSL_SATURATION,
    HB_PAINT_COMPOSITE_MODE_HSL_COLOR,
    HB_PAINT_COMPOSITE_MODE_HSL_LUMINOSITY
} hb_paint_composite_mode_t;

typedef void (*hb_paint_push_group_func_t) (hb_paint_funcs_t *funcs,
                                            void *paint_data,
                                            void *user_data);

typedef void (*hb_paint_pop_group_func_t) (hb_paint_funcs_t *funcs,
                                           void *paint_data,
                                           hb_paint_composite_mode_t mode,
                                           void *user_data);

/* Return nonzero and store `*color` to override palette entry
 * `color_index`; consulted before CPAL for every entry but 0xFFFF. */
typedef hb_bool_t (*hb_paint_custom_palette_color_func_t) (hb_paint_funcs_t *funcs,
                                                           void *paint_data,
                                                           unsigned int color_index,
                                                           hb_color_t *color,
                                                           void *user_data);

void hb_paint_funcs_set_push_transform_func(hb_paint_funcs_t *funcs,
                                            hb_paint_push_transform_func_t func,
                                            void *user_data,
                                            hb_destroy_func_t destroy);
void hb_paint_funcs_set_pop_transform_func(hb_paint_funcs_t *funcs,
                                           hb_paint_pop_transform_func_t func,
                                           void *user_data,
                                           hb_destroy_func_t destroy);
void hb_paint_funcs_set_push_clip_glyph_func(hb_paint_funcs_t *funcs,
                                             hb_paint_push_clip_glyph_func_t func,
                                             void *user_data,
                                             hb_destroy_func_t destroy);
void hb_paint_funcs_set_push_clip_rectangle_func(hb_paint_funcs_t *funcs,
                                                 hb_paint_push_clip_rectangle_func_t func,
                                                 void *user_data,
                                                 hb_destroy_func_t destroy);
void hb_paint_funcs_set_pop_clip_func(hb_paint_funcs_t *funcs,
                                      hb_paint_pop_clip_func_t func,
                                      void *user_data,
                                      hb_destroy_func_t destroy);
void hb_paint_funcs_set_color_func(hb_paint_funcs_t *funcs,
                                   hb_paint_color_func_t func,
                                   void *user_data,
                                   hb_destroy_func_t destroy);
void hb_paint_funcs_set_image_func(hb_paint_funcs_t *funcs,
                                   hb_paint_image_func_t func,
                                   void *user_data,
                                   hb_destroy_func_t destroy);
void hb_paint_funcs_set_linear_gradient_func(hb_paint_funcs_t *funcs,
                                             hb_paint_linear_gradient_func_t func,
                                             void *user_data,
                                             hb_destroy_func_t destroy);
void hb_paint_funcs_set_radial_gradient_func(hb_paint_funcs_t *funcs,
                                             hb_paint_radial_gradient_func_t func,
                                             void *user_data,
                                             hb_destroy_func_t destroy);
void hb_paint_funcs_set_sweep_gradient_func(hb_paint_funcs_t *funcs,
                                            hb_paint_sweep_gradient_func_t func,
                                            void *user_data,
                                            hb_destroy_func_t destroy);
void hb_paint_funcs_set_push_group_func(hb_paint_funcs_t *funcs,
                                        hb_paint_push_group_func_t func,
                                        void *user_data,
                                        hb_destroy_func_t destroy);
void hb_paint_funcs_set_pop_group_func(hb_paint_funcs_t *funcs,
                                       hb_paint_pop_group_func_t func,
                                       void *user_data,
                                       hb_destroy_func_t destroy);
void hb_paint_funcs_set_custom_palette_color_func(hb_paint_funcs_t *funcs,
                                                  hb_paint_custom_palette_color_func_t func,
                                                  void *user_data,
                                                  hb_destroy_func_t destroy);

/* Paints `glyph` through `pfuncs`, as HarfBuzz does:
 *
 *   - COLRv1: push_transform(x_scale/upem, 0, 0, y_scale/upem, 0, 0),
 *     the paint tree, pop_transform. A PaintGlyph is push_transform
 *     (inverse), push_clip_glyph, push_transform (root), the child, then
 *     three pops. A PaintComposite is push_group, backdrop, push_group,
 *     source, pop_group(mode), pop_group(SRC_OVER). Sweep angles are
 *     (stored angle + 1) * pi.
 *   - COLRv0: push_clip_glyph, color, pop_clip for each layer.
 *   - Anything else: push_clip_glyph(glyph), color(1, foreground),
 *     pop_clip.
 *
 * Palette entry 0xFFFF paints `foreground` with is_foreground = 1. Other
 * entries try the custom_palette_color callback, then CPAL palette
 * `palette_index`, and paint `foreground` with is_foreground = 0 when
 * the font has no such palette or entry. The paint alpha multiplies the
 * alpha byte, truncated. The walk uses the font's current scale and
 * variation coordinates. Not emitted yet: HarfBuzz's clip rectangle
 * around COLRv1 glyphs, and image callbacks. */
void hb_font_paint_glyph(hb_font_t *font,
                         hb_codepoint_t glyph,
                         hb_paint_funcs_t *pfuncs,
                         void *paint_data,
                         unsigned int palette_index,
                         hb_color_t foreground);

/* ---------- Introspection ---------- */

#define HB_OT_TAG_GSUB HB_TAG('G','S','U','B')
#define HB_OT_TAG_GPOS HB_TAG('G','P','O','S')

void hb_face_collect_unicodes(const hb_face_t *face, hb_set_t *set);

void hb_ot_layout_collect_features(const hb_face_t *face,
                                   hb_tag_t          table_tag,
                                   const hb_tag_t   *scripts,
                                   const hb_tag_t   *languages,
                                   hb_set_t         *features);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* SIGILBUZZ_HB_H */
