/* The soft-float kernel: the Chumby has no FPU, so every f32 and f64
 * operation is a call into the soft-float helpers natively, and the same
 * calls (or the interpreter's own) under wasm. Two parts, each timed:
 *   nbody   f64: the five-body step of the Benchmarks Game, with sqrt
 *   matmul  f32: a 32x32 matrix multiply, repeated
 * Prints one line per part: its name, the work done, the milliseconds and a
 * checksum (the same under every runtime, or something is wrong). */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

static double now_ms(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

#define PI 3.141592653589793
#define SOLAR_MASS (4 * PI * PI)
#define DAYS 365.24

struct body {
    double x, y, z, vx, vy, vz, m;
};

static struct body bodies[5] = {
    {0, 0, 0, 0, 0, 0, SOLAR_MASS},
    {4.84143144246472090e+00, -1.16032004402742839e+00, -1.03622044471123109e-01,
     1.66007664274403694e-03 * DAYS, 7.69901118419740425e-03 * DAYS,
     -6.90460016972063023e-05 * DAYS, 9.54791938424326609e-04 * SOLAR_MASS},
    {8.34336671824457987e+00, 4.12479856412430479e+00, -4.03523417114321381e-01,
     -2.76742510726862411e-03 * DAYS, 4.99852801234917238e-03 * DAYS,
     2.30417297573763929e-05 * DAYS, 2.85885980666130812e-04 * SOLAR_MASS},
    {1.28943695621391310e+01, -1.51111514016986312e+01, -2.23307578892655734e-01,
     2.96460137564761618e-03 * DAYS, 2.37847173959480950e-03 * DAYS,
     -2.96589568540237556e-05 * DAYS, 4.36624404335156298e-05 * SOLAR_MASS},
    {1.53796971148509165e+01, -2.59193146099879641e+01, 1.79258772950371181e-01,
     2.68067772490389322e-03 * DAYS, 1.62824170038242295e-03 * DAYS,
     -9.51592254519715870e-05 * DAYS, 5.15138902046611451e-05 * SOLAR_MASS},
};

static void advance(double dt)
{
    for (int i = 0; i < 5; i++) {
        struct body *a = &bodies[i];
        for (int j = i + 1; j < 5; j++) {
            struct body *b = &bodies[j];
            double dx = a->x - b->x, dy = a->y - b->y, dz = a->z - b->z;
            double d2 = dx * dx + dy * dy + dz * dz;
            double mag = dt / (d2 * sqrt(d2));
            a->vx -= dx * b->m * mag;
            a->vy -= dy * b->m * mag;
            a->vz -= dz * b->m * mag;
            b->vx += dx * a->m * mag;
            b->vy += dy * a->m * mag;
            b->vz += dz * a->m * mag;
        }
    }
    for (int i = 0; i < 5; i++) {
        bodies[i].x += dt * bodies[i].vx;
        bodies[i].y += dt * bodies[i].vy;
        bodies[i].z += dt * bodies[i].vz;
    }
}

static double energy(void)
{
    double e = 0;
    for (int i = 0; i < 5; i++) {
        struct body *a = &bodies[i];
        e += 0.5 * a->m * (a->vx * a->vx + a->vy * a->vy + a->vz * a->vz);
        for (int j = i + 1; j < 5; j++) {
            struct body *b = &bodies[j];
            double dx = a->x - b->x, dy = a->y - b->y, dz = a->z - b->z;
            e -= a->m * b->m / sqrt(dx * dx + dy * dy + dz * dz);
        }
    }
    return e;
}

#define N 32
static float ma[N][N], mb[N][N], mc[N][N];

int main(int argc, char **argv)
{
    int steps = argc > 1 ? atoi(argv[1]) : 20000;
    int reps = argc > 2 ? atoi(argv[2]) : 20;

    double px = 0, py = 0, pz = 0;
    for (int i = 0; i < 5; i++) {
        px += bodies[i].vx * bodies[i].m;
        py += bodies[i].vy * bodies[i].m;
        pz += bodies[i].vz * bodies[i].m;
    }
    bodies[0].vx = -px / SOLAR_MASS;
    bodies[0].vy = -py / SOLAR_MASS;
    bodies[0].vz = -pz / SOLAR_MASS;
    double t0 = now_ms();
    for (int i = 0; i < steps; i++)
        advance(0.01);
    double t1 = now_ms();
    printf("nbody steps=%d ms=%.1f energy=%.9f\n", steps, t1 - t0, energy());

    for (int i = 0; i < N; i++)
        for (int j = 0; j < N; j++) {
            ma[i][j] = (float)(i + j) / N;
            mb[i][j] = (float)(i - j) / N;
        }
    t0 = now_ms();
    for (int r = 0; r < reps; r++) {
        for (int i = 0; i < N; i++)
            for (int j = 0; j < N; j++) {
                float s = 0;
                for (int k = 0; k < N; k++)
                    s += ma[i][k] * mb[k][j];
                mc[i][j] = s;
            }
        ma[r % N][r % N] += 0.5f;
    }
    t1 = now_ms();
    float sum = 0;
    for (int i = 0; i < N; i++)
        for (int j = 0; j < N; j++)
            sum += mc[i][j];
    printf("matmul reps=%d ms=%.1f sum=%.4f\n", reps, t1 - t0, sum);
    return 0;
}
