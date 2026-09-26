/*
 * test_refcount.c: HarfBuzz ownership rules, driven from C.
 *
 * Runs HarfBuzz-style reference / destroy sequences on every
 * refcounted type: hb_x_reference(p) must return p itself, and a
 * reference followed by two destroys must be balanced. The blob's
 * destroy callback proves the blob, the face, and the font were each
 * released exactly once: it must not fire while any of them is alive
 * and must fire exactly once after the last one goes. Under the old
 * copy-per-reference handles this sequence double-freed.
 *
 * Exit 0 means PASS.
 */

#include "hb.h"
#include <stdio.h>
#include <stdlib.h>

#define CHECK(cond, code, msg)                                   \
    do {                                                         \
        if (!(cond)) {                                           \
            fprintf(stderr, "check failed (%d): %s\n", (code), (msg)); \
            return (code);                                       \
        }                                                        \
    } while (0)

/* Owns the font bytes handed to hb_blob_create in READONLY mode. The
 * destroy callback frees them and counts how often it ran. */
typedef struct {
    char *data;
    int destroyed;
} blob_owner_t;

static void release_blob_data(void *user_data) {
    blob_owner_t *owner = (blob_owner_t *)user_data;
    free(owner->data);
    owner->data = NULL;
    owner->destroyed += 1;
}

static void count_destroy(void *user_data) {
    int *counter = (int *)user_data;
    *counter += 1;
}

static char *read_file(const char *path, unsigned int *length) {
    FILE *f = fopen(path, "rb");
    long size;
    char *data;
    if (!f) {
        return NULL;
    }
    if (fseek(f, 0, SEEK_END) != 0 || (size = ftell(f)) <= 0 || fseek(f, 0, SEEK_SET) != 0) {
        fclose(f);
        return NULL;
    }
    data = (char *)malloc((size_t)size);
    if (data && fread(data, 1, (size_t)size, f) != (size_t)size) {
        free(data);
        data = NULL;
    }
    fclose(f);
    *length = (unsigned int)size;
    return data;
}

static int check_blob_face_font(const char *path) {
    blob_owner_t owner;
    unsigned int length = 0;
    hb_blob_t *blob;
    hb_face_t *face;
    hb_font_t *font;
    hb_buffer_t *buffer;

    owner.destroyed = 0;
    owner.data = read_file(path, &length);
    CHECK(owner.data != NULL, 10, "could not read the font file");

    blob = hb_blob_create(owner.data, length, HB_MEMORY_MODE_READONLY, &owner, release_blob_data);
    CHECK(hb_blob_get_length(blob) == length, 11, "blob length");

    /* Blob: reference returns the same pointer; ref + destroy x2 is balanced. */
    CHECK(hb_blob_reference(blob) == blob, 12, "hb_blob_reference must return its argument");
    hb_blob_destroy(blob);
    CHECK(owner.destroyed == 0, 13, "blob freed while a reference was still out");
    CHECK(hb_blob_get_length(blob) == length, 14, "blob unusable after one destroy");

    /* Face: holds its blob, so our blob reference can go right away. */
    face = hb_face_create(blob, 0);
    hb_blob_destroy(blob);
    CHECK(owner.destroyed == 0, 15, "blob freed while the face still uses it");
    CHECK(hb_face_reference(face) == face, 16, "hb_face_reference must return its argument");
    hb_face_destroy(face);
    CHECK(hb_face_get_glyph_count(face) > 0, 17, "face unusable after one destroy");

    /* Font: holds its face. */
    font = hb_font_create(face);
    hb_face_destroy(face);
    CHECK(owner.destroyed == 0, 18, "blob freed while the font still uses it");
    CHECK(hb_font_reference(font) == font, 19, "hb_font_reference must return its argument");
    hb_font_destroy(font);

    buffer = hb_buffer_create();
    CHECK(hb_buffer_reference(buffer) == buffer, 20, "hb_buffer_reference must return its argument");
    hb_buffer_destroy(buffer);
    hb_buffer_add_utf8(buffer, "Hello", -1, 0, -1);
    hb_buffer_guess_segment_properties(buffer);
    hb_shape(font, buffer, NULL, 0);
    CHECK(hb_buffer_get_length(buffer) >= 5, 21, "shaping through surviving references failed");
    hb_buffer_destroy(buffer);

    /* Last font reference: font, face, and blob all go now, once. */
    hb_font_destroy(font);
    CHECK(owner.destroyed == 1, 22, "blob destroy callback must fire exactly once");
    return 0;
}

static int check_blob_callback_timing(void) {
    int zero_length = 0;
    int duplicate = 0;
    const char bytes[4] = {1, 2, 3, 4};
    hb_blob_t *blob;

    /* HarfBuzz runs `destroy` at once for an empty blob... */
    blob = hb_blob_create(bytes, 0, HB_MEMORY_MODE_READONLY, &zero_length, count_destroy);
    CHECK(zero_length == 1, 30, "zero-length blob must run destroy immediately");
    hb_blob_destroy(blob);
    CHECK(zero_length == 1, 31, "zero-length blob ran destroy twice");

    /* ...and for DUPLICATE, which copies up front. */
    blob = hb_blob_create(bytes, 4, HB_MEMORY_MODE_DUPLICATE, &duplicate, count_destroy);
    CHECK(duplicate == 1, 32, "DUPLICATE blob must run destroy immediately");
    CHECK(hb_blob_get_length(blob) == 4, 33, "DUPLICATE blob length");
    hb_blob_destroy(blob);
    CHECK(duplicate == 1, 34, "DUPLICATE blob ran destroy twice");
    return 0;
}

static int check_set_and_subset_input(void) {
    hb_set_t *set = hb_set_create();
    hb_subset_input_t *input;
    hb_set_t *first;
    hb_set_t *second;

    CHECK(hb_set_reference(set) == set, 40, "hb_set_reference must return its argument");
    hb_set_add(set, 7);
    hb_set_destroy(set);
    CHECK(hb_set_has(set, 7) == 1, 41, "set unusable after one destroy");
    hb_set_destroy(set);

    /* Each accessor call is a new reference to the same set. */
    input = hb_subset_input_create();
    first = hb_subset_input_unicode_set(input);
    second = hb_subset_input_unicode_set(input);
    CHECK(first == second, 45, "unicode set pointer changed");
    hb_set_destroy(first);
    hb_set_destroy(second);
    hb_subset_input_destroy(input);
    return 0;
}

static int check_paint_funcs(void) {
    hb_paint_funcs_t *funcs = hb_paint_funcs_create();
    CHECK(hb_paint_funcs_reference(funcs) == funcs, 60,
          "hb_paint_funcs_reference must return its argument");
    hb_paint_funcs_destroy(funcs);
    hb_paint_funcs_set_color_func(funcs, NULL);
    hb_paint_funcs_destroy(funcs);
    return 0;
}

static int check_null_handles(void) {
    CHECK(hb_blob_reference(NULL) == NULL, 70, "hb_blob_reference(NULL)");
    CHECK(hb_face_reference(NULL) == NULL, 71, "hb_face_reference(NULL)");
    CHECK(hb_font_reference(NULL) == NULL, 72, "hb_font_reference(NULL)");
    CHECK(hb_buffer_reference(NULL) == NULL, 73, "hb_buffer_reference(NULL)");
    CHECK(hb_set_reference(NULL) == NULL, 74, "hb_set_reference(NULL)");
    CHECK(hb_paint_funcs_reference(NULL) == NULL, 76, "hb_paint_funcs_reference(NULL)");
    hb_blob_destroy(NULL);
    hb_face_destroy(NULL);
    hb_font_destroy(NULL);
    hb_buffer_destroy(NULL);
    hb_set_destroy(NULL);
    hb_subset_input_destroy(NULL);
    hb_paint_funcs_destroy(NULL);
    return 0;
}

int main(int argc, char **argv) {
    int rc;
    if (argc < 2) {
        fprintf(stderr, "usage: %s <font.ttf>\n", argv[0]);
        return 2;
    }
    if ((rc = check_blob_face_font(argv[1])) != 0) {
        return rc;
    }
    if ((rc = check_blob_callback_timing()) != 0) {
        return rc;
    }
    if ((rc = check_set_and_subset_input()) != 0) {
        return rc;
    }
    if ((rc = check_paint_funcs()) != 0) {
        return rc;
    }
    return check_null_handles();
}
