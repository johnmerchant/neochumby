/* The framebuffer-style memory kernel: 320x240 RGB565 buffers, drawn as an
 * app would, each part timed:
 *   fill   every pixel written by a loop (a gradient that changes per frame)
 *   copy   the whole back buffer copied to the front (memcpy, which is
 *          memory.copy under wasm with bulk memory)
 *   blit   a 64x64 sprite copied row by row to 48 places on the frame
 *   rect   solid rectangles: 32 of 80x60, one u16 at a time
 * Prints one line per part: its name, the frames, the milliseconds, and a
 * checksum of the buffer. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define W 320
#define H 240

static uint16_t back[W * H], front[W * H], sprite[64 * 64];

static double now_ms(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static uint32_t sum(const uint16_t *b)
{
    uint32_t s = 0;
    for (int i = 0; i < W * H; i++)
        s = s * 31 + b[i];
    return s;
}

int main(int argc, char **argv)
{
    int frames = argc > 1 ? atoi(argv[1]) : 100;
    double t0, t1;

    t0 = now_ms();
    for (int f = 0; f < frames; f++)
        for (int y = 0; y < H; y++) {
            uint16_t *row = back + y * W;
            for (int x = 0; x < W; x++)
                row[x] = (uint16_t)(((x + f) >> 3) << 11 | ((y + f) >> 2) << 5 | (x ^ y) >> 4);
        }
    t1 = now_ms();
    printf("fill frames=%d ms=%.1f sum=%u\n", frames, t1 - t0, sum(back));

    t0 = now_ms();
    for (int f = 0; f < frames; f++) {
        back[f] = (uint16_t)f;
        memcpy(front, back, sizeof front);
    }
    t1 = now_ms();
    printf("copy frames=%d ms=%.1f sum=%u\n", frames, t1 - t0, sum(front));

    for (int i = 0; i < 64 * 64; i++)
        sprite[i] = (uint16_t)(i * 7);
    t0 = now_ms();
    for (int f = 0; f < frames; f++)
        for (int n = 0; n < 48; n++) {
            int ox = (n * 37 + f) % (W - 64), oy = (n * 23 + f) % (H - 64);
            for (int y = 0; y < 64; y++)
                memcpy(front + (oy + y) * W + ox, sprite + y * 64, 64 * 2);
        }
    t1 = now_ms();
    printf("blit frames=%d ms=%.1f sum=%u\n", frames, t1 - t0, sum(front));

    t0 = now_ms();
    for (int f = 0; f < frames; f++)
        for (int n = 0; n < 32; n++) {
            int ox = (n * 53 + f) % (W - 80), oy = (n * 29 + f) % (H - 60);
            uint16_t c = (uint16_t)(n * 2048 + f);
            for (int y = 0; y < 60; y++) {
                uint16_t *p = front + (oy + y) * W + ox;
                for (int x = 0; x < 80; x++)
                    p[x] = c;
            }
        }
    t1 = now_ms();
    printf("rect frames=%d ms=%.1f sum=%u\n", frames, t1 - t0, sum(front));
    return 0;
}
