#pragma once
#include <stddef.h>
#include <stdarg.h>
typedef struct FILE FILE;
#define stderr ((FILE*)0)
int fprintf(FILE *f, const char *fmt, ...);
