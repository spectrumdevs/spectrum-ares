#ifndef SPECTRUM_ARES_H
#define SPECTRUM_ARES_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ARES_ABI_VERSION 1
#define ARES_DEFAULT_BAND_COUNT 64

#define ARES_OK 0
#define ARES_STATUS_TRUNCATED 1

#define ARES_ERROR_NULL_POINTER -1
#define ARES_ERROR_INVALID_LENGTH -2
#define ARES_ERROR_BACKEND -3

int ares_get_abi_version(void);
int ares_start(void);
int ares_stop(void);
int ares_is_running(void);
int ares_get_band_count(void);
int ares_get_bands(float *out, int len);
float ares_get_rms(void);
float ares_get_peak(void);
int ares_get_last_error(char *buffer, size_t len);

#ifdef __cplusplus
}
#endif

#endif
