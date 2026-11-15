/* Minimal libm declarations for libopus on wasm32 (implemented in Rust, src/libc.rs) */
#pragma once
double sqrt(double); float sqrtf(float);
double exp(double); float expf(float);
double log(double); float logf(float);
double log10(double); float log10f(float);
double pow(double, double); float powf(float, float);
double sin(double); float sinf(float);
double cos(double); float cosf(float);
double atan(double); float atanf(float);
double acos(double); float acosf(float);
double atan2(double, double); float atan2f(float, float);
double tanh(double); float tanhf(float);
double floor(double); float floorf(float);
double ceil(double); float ceilf(float);
double fabs(double); float fabsf(float);
double fmod(double, double);
long lrint(double); long lrintf(float);
double frexp(double, int*); double ldexp(double, int);
#define M_PI 3.14159265358979323846
#define fabs(x) __builtin_fabs(x)
#define fabsf(x) __builtin_fabsf(x)
#define sqrt(x) __builtin_sqrt(x)
#define sqrtf(x) __builtin_sqrtf(x)
#define floor(x) __builtin_floor(x)
#define floorf(x) __builtin_floorf(x)
#define ceil(x) __builtin_ceil(x)
#define ceilf(x) __builtin_ceilf(x)
#define HUGE_VAL __builtin_huge_val()
#define INFINITY __builtin_inff()
#define NAN __builtin_nanf("")
#define isnan(x) __builtin_isnan(x)
