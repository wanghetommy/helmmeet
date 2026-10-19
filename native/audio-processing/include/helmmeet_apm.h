#ifndef HELMMEET_APM_H
#define HELMMEET_APM_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* ABI 1: single-owner instances, normalized finite float PCM, 16kHz mono,
 * exactly 160 samples per call. No capture, file/network I/O or logging.
 * Destroy only after all calls return. Input/output may alias. */
typedef struct hm_apm hm_apm;
typedef struct { uint32_t abi_version; int32_t noise_level; int32_t echo_cancellation; } hm_apm_config;
enum { HM_APM_OK = 0, HM_APM_INVALID = -1, HM_APM_ENGINE = -2, HM_APM_REFERENCE_REQUIRED = -3 };
const char *hm_apm_version(void);
int32_t hm_apm_create(const hm_apm_config *config, hm_apm **output);
int32_t hm_apm_process(hm_apm *state, const float *input, float *output, size_t samples);
int32_t hm_apm_reverse(hm_apm *state, const float *input, size_t samples, int32_t delay_ms);
/* Optional engine delay estimate, -1 means unavailable. */
int32_t hm_apm_delay_ms(hm_apm *state);
int32_t hm_apm_reset(hm_apm *state);
void hm_apm_destroy(hm_apm *state);
#ifdef __cplusplus
}
#endif
#endif
