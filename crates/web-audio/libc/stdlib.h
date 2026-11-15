/* Minimal libc for libopus on wasm32: allocation is provided by Rust (src/libc.rs) */
#pragma once
#include <stddef.h>
void *malloc(size_t n);
void *calloc(size_t n, size_t size);
void *realloc(void *p, size_t n);
void free(void *p);
void abort(void);
int abs(int x);
#define NULL ((void*)0)
