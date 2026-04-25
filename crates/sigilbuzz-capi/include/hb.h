/*
 * hb.h — sigilbuzz's HarfBuzz-symbol-compatible C header.
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

/* ---------- Opaque handles ---------- */

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
void         hb_buffer_clear_contents(hb_buffer_t *buffer);

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

void         hb_buffer_set_direction(hb_buffer_t *buffer, hb_direction_t direction);
void         hb_buffer_set_script(hb_buffer_t *buffer, hb_script_t script);
void         hb_buffer_set_language(hb_buffer_t *buffer, hb_language_t language);
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

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* SIGILBUZZ_HB_H */
