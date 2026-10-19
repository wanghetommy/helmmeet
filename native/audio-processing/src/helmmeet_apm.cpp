#include "helmmeet_apm.h"
#include "api/audio/audio_processing.h"
#include "rtc_base/logging.h"
#include <algorithm>
#include <array>
#include <cmath>
#include <memory>
#include <mutex>

struct hm_apm {
  rtc::scoped_refptr<webrtc::AudioProcessing> engine;
  bool echo;
  bool reference = false;
};
namespace {
bool valid(const float *input, size_t samples) {
  return input && samples == 160 && std::all_of(input, input + samples, [](float x) {
    return std::isfinite(x) && std::abs(x) <= 1.0f;
  });
}
}
extern "C" const char *hm_apm_version(void) {
  return "webrtc-audio-processing/2.1;abseil/20240722.1;abi=1;16k-mono";
}
extern "C" int32_t hm_apm_create(const hm_apm_config *config, hm_apm **output) {
  if (!output) return HM_APM_INVALID;
  *output = nullptr;
  if (!config || config->abi_version != 1 || config->noise_level < 0 || config->noise_level > 2 ||
      config->echo_cancellation < 0 || config->echo_cancellation > 1) return HM_APM_INVALID;
  try {
    // Do not let upstream logs expose future conference audio diagnostics.
    static std::once_flag logging;
    std::call_once(logging, [] { rtc::LogMessage::LogToDebug(rtc::LS_NONE); rtc::LogMessage::SetLogToStderr(false); });
    webrtc::AudioProcessing::Config settings;
    settings.echo_canceller.enforce_high_pass_filtering = false;
    settings.echo_canceller.enabled = config->echo_cancellation != 0;
    settings.noise_suppression.enabled = config->noise_level != 0;
    settings.noise_suppression.level = config->noise_level == 1
      ? webrtc::AudioProcessing::Config::NoiseSuppression::kLow
      : webrtc::AudioProcessing::Config::NoiseSuppression::kModerate;
    settings.gain_controller1.enabled = false;
    settings.gain_controller2.enabled = false;
    settings.high_pass_filter.enabled = false;
    auto engine = webrtc::AudioProcessingBuilder().SetConfig(settings).Create();
    if (!engine || engine->Initialize() != 0) return HM_APM_ENGINE;
    auto state = std::make_unique<hm_apm>();
    state->engine = std::move(engine);
    state->echo = settings.echo_canceller.enabled;
    *output = state.release();
    return HM_APM_OK;
  } catch (...) { return HM_APM_ENGINE; }
}
extern "C" int32_t hm_apm_process(hm_apm *state, const float *input, float *output, size_t samples) {
  if (!state || !output || !valid(input, samples)) return HM_APM_INVALID;
  if (state->echo && !state->reference) return HM_APM_REFERENCE_REQUIRED;
  try {
    std::array<float, 160> temporary{};
    const float *in[] = {input}; float *out[] = {temporary.data()};
    const webrtc::StreamConfig format(16000, 1);
    if (state->engine->ProcessStream(in, format, format, out) != 0 || !valid(temporary.data(), 160)) return HM_APM_ENGINE;
    std::copy(temporary.begin(), temporary.end(), output);
    state->reference = false;
    return HM_APM_OK;
  } catch (...) { return HM_APM_ENGINE; }
}
extern "C" int32_t hm_apm_reverse(hm_apm *state, const float *input, size_t samples, int32_t delay_ms) {
  if (!state) return HM_APM_INVALID;
  state->reference = false;
  if (!state->echo || !valid(input, samples) || delay_ms < 0 || delay_ms > 1000) return HM_APM_INVALID;
  try {
    std::array<float, 160> ignored{};
    const float *in[] = {input}; float *out[] = {ignored.data()};
    const webrtc::StreamConfig format(16000, 1);
    if (state->engine->ProcessReverseStream(in, format, format, out) != 0 ||
        state->engine->set_stream_delay_ms(delay_ms) != 0) return HM_APM_ENGINE;
    state->reference = true;
    return HM_APM_OK;
  } catch (...) { return HM_APM_ENGINE; }
}
extern "C" int32_t hm_apm_delay_ms(hm_apm *state) {
  if (!state || !state->echo) return -1;
  try { return state->engine->GetStatistics().delay_ms.value_or(-1); }
  catch (...) { return -1; }
}
extern "C" int32_t hm_apm_reset(hm_apm *state) {
  if (!state) return HM_APM_INVALID;
  try {
    state->reference = false;
    return state->engine->Initialize() == 0 ? HM_APM_OK : HM_APM_ENGINE;
  } catch (...) { return HM_APM_ENGINE; }
}
extern "C" void hm_apm_destroy(hm_apm *state) { delete state; }
