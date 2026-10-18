#ifndef HELMMEET_WHISPER_H
#define HELMMEET_WHISPER_H
#include <stddef.h>
#include <stdint.h>

/* Planned ABI, not implemented or linked in M0. No dependency on terminal output. */
#ifdef __cplusplus
extern "C" {
#endif
typedef struct hm_whisper_context hm_whisper_context;
typedef enum {
    HM_ASR_OK = 0,
    HM_ASR_UNAVAILABLE = 1,
    HM_ASR_INVALID_INPUT = 2,
    HM_ASR_CANCELLED = 3
} hm_asr_status;
/* Model path UTF-8. Caller owns path; context owns model until destroy. */
hm_asr_status hm_whisper_create(const char *model_path, hm_whisper_context **out);
/* 16 kHz mono float PCM; input owned by caller for this synchronous call.
 * Result callback and revision protocol will be finalized in M2 before use. */
hm_asr_status hm_whisper_process(hm_whisper_context *ctx, const float *pcm, size_t samples);
void hm_whisper_cancel(hm_whisper_context *ctx);
void hm_whisper_destroy(hm_whisper_context *ctx);
#ifdef __cplusplus
}
#endif
#endif
