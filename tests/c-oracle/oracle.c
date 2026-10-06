/*
 * C oracle for spiceport's decoder tests: encodes raw images with the
 * original spice-common QUIC and LZ encoders so the Rust decoders can be
 * checked byte-for-byte against them.
 *
 *   oracle quic <QuicImageType> <width> <height> <in.raw> <out.bin>
 *   oracle lz   <LzImageType> <width> <height> <stride> <top_down> <in.raw> <out.bin>
 *
 * Input rows are tightly packed at the given stride (QUIC: width * bytes
 * per pixel). Exit status is non-zero on encoder errors.
 *
 * This file is LGPL-2.1-or-later like the code it links.
 */
#include <setjmp.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "quic.h"
#include "lz.h"

static jmp_buf jb;

static void verr(const char *fmt, va_list ap)
{
    fprintf(stderr, "oracle: ");
    vfprintf(stderr, fmt, ap);
}

#define USR_FUNCS(prefix, T)                                                   \
    static void prefix##_error(T *u, const char *fmt, ...)                     \
    {                                                                          \
        va_list ap; va_start(ap, fmt); verr(fmt, ap); va_end(ap);              \
        longjmp(jb, 1);                                                        \
    }                                                                          \
    static void prefix##_warn(T *u, const char *fmt, ...)                      \
    {                                                                          \
        va_list ap; va_start(ap, fmt); verr(fmt, ap); va_end(ap);              \
    }                                                                          \
    static void prefix##_info(T *u, const char *fmt, ...) {}                   \
    static void *prefix##_malloc(T *u, int size) { return malloc(size); }      \
    static void prefix##_free(T *u, void *p) { free(p); }

USR_FUNCS(q, QuicUsrContext)
USR_FUNCS(l, LzUsrContext)

static int q_more_space(QuicUsrContext *u, uint32_t **io, int rows) { return 0; }
static int q_more_lines(QuicUsrContext *u, uint8_t **lines) { return 0; }
static int l_more_space(LzUsrContext *u, uint8_t **io) { return 0; }
static int l_more_lines(LzUsrContext *u, uint8_t **lines) { return 0; }

static uint8_t *read_file(const char *path, long *len)
{
    FILE *f = fopen(path, "rb");
    if (!f) { perror(path); exit(2); }
    fseek(f, 0, SEEK_END);
    *len = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc(*len + 1);
    if (fread(buf, 1, *len, f) != (size_t)*len) { perror(path); exit(2); }
    fclose(f);
    return buf;
}

static void write_file(const char *path, const void *buf, size_t len)
{
    FILE *f = fopen(path, "wb");
    if (!f || fwrite(buf, 1, len, f) != len) { perror(path); exit(2); }
    fclose(f);
}

int main(int argc, char **argv)
{
    if (argc >= 7 && strcmp(argv[1], "quic") == 0) {
        int type = atoi(argv[2]), w = atoi(argv[3]), h = atoi(argv[4]);
        long len;
        uint8_t *in = read_file(argv[5], &len);
        int stride = len / h;
        QuicUsrContext usr = { q_error, q_warn, q_info, q_malloc, q_free, q_more_space, q_more_lines };
        QuicContext *q = quic_create(&usr);
        unsigned int nwords = (unsigned)(len / 2 + 4096);
        uint32_t *out = calloc(nwords, 4);
        if (setjmp(jb)) return 1;
        int n = quic_encode(q, type, w, h, in, h, stride, out, nwords);
        if (n <= 0) return 1;
        write_file(argv[6], out, (size_t)n * 4);
        return 0;
    }
    if (argc >= 9 && strcmp(argv[1], "lz") == 0) {
        int type = atoi(argv[2]), w = atoi(argv[3]), h = atoi(argv[4]);
        int stride = atoi(argv[5]), top_down = atoi(argv[6]);
        long len;
        uint8_t *in = read_file(argv[7], &len);
        LzUsrContext usr = { l_error, l_warn, l_info, l_malloc, l_free, l_more_space, l_more_lines };
        LzContext *lz = lz_create(&usr);
        unsigned int nbytes = (unsigned)(len * 2 + 4096);
        uint8_t *out = calloc(nbytes, 1);
        if (setjmp(jb)) return 1;
        int n = lz_encode(lz, type, w, h, top_down, in, h, stride, out, nbytes);
        if (n <= 0) return 1;
        write_file(argv[8], out, (size_t)n);
        return 0;
    }
    fprintf(stderr, "usage: see oracle.c\n");
    return 2;
}

/* log.c wants this from backtrace.c, which drags in more dependencies. */
void spice_backtrace(void) {}
