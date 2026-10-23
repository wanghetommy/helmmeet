#pragma once
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
// Single owner. 16kHz mono finite float PCM, <=8192 samples per input.
void *hm_kws_create(const char *directory, const char *tokens, float silence);
void hm_kws_destroy(void *handle);
void hm_kws_reset(void *handle);
// bits: 1 keyword detected, 2 inside speech, 4 completed speech segment.
int32_t hm_kws_accept(void *handle, const float *samples, int32_t count, int32_t detect_keyword);
#ifdef __cplusplus
}
#endif
// Standalone meeting VAD: accept via hm_kws_accept(...,0), destroy via hm_kws_destroy.
#ifdef __cplusplus
extern "C" {
#endif
void *hm_meeting_vad_create(const char *directory);
#ifdef __cplusplus
}
#endif
