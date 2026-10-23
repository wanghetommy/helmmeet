#include "helmmeet_voice.h"
#include "sherpa-onnx-c-api.h"
#include <cmath>
#include <cstring>
#include <memory>
#include <string>
struct Voice {
 const SherpaOnnxKeywordSpotter *kws=nullptr;
 const SherpaOnnxOnlineStream *stream=nullptr;
 const SherpaOnnxVoiceActivityDetector *vad=nullptr;
 ~Voice(){if(stream)SherpaOnnxDestroyOnlineStream(stream);if(kws)SherpaOnnxDestroyKeywordSpotter(kws);if(vad)SherpaOnnxDestroyVoiceActivityDetector(vad);}
};
extern "C" void *hm_kws_create(const char *directory,const char *tokens,float silence){
 try {
  if(!directory||!tokens||silence<.8f||silence>2.f)return nullptr;
  auto v=std::make_unique<Voice>(); std::string root(directory);
  auto encoder=root+"/encoder.onnx",decoder=root+"/decoder.onnx",joiner=root+"/joiner.onnx",vocabulary=root+"/tokens.txt",vad=root+"/silero_vad.onnx";
  SherpaOnnxKeywordSpotterConfig c{};
  c.feat_config.sample_rate=16000;c.feat_config.feature_dim=80;
  c.model_config.transducer.encoder=encoder.c_str();c.model_config.transducer.decoder=decoder.c_str();c.model_config.transducer.joiner=joiner.c_str();
  c.model_config.tokens=vocabulary.c_str();c.model_config.num_threads=1;c.model_config.provider="cpu";
  c.max_active_paths=4;c.num_trailing_blanks=1;c.keywords_score=1.5f;c.keywords_threshold=.3f;
  c.keywords_buf=tokens;c.keywords_buf_size=static_cast<int32_t>(strlen(tokens));
  v->kws=SherpaOnnxCreateKeywordSpotter(&c);if(!v->kws)return nullptr;
  v->stream=SherpaOnnxCreateKeywordStream(v->kws);if(!v->stream)return nullptr;
  SherpaOnnxVadModelConfig d{};d.silero_vad.model=vad.c_str();d.silero_vad.threshold=.5f;
  d.silero_vad.min_silence_duration=silence;d.silero_vad.min_speech_duration=.16f;d.silero_vad.window_size=512;d.silero_vad.max_speech_duration=32;
  d.sample_rate=16000;d.num_threads=1;d.provider="cpu";
  v->vad=SherpaOnnxCreateVoiceActivityDetector(&d,35.f);if(!v->vad)return nullptr;
  return v.release();
 }catch(...){return nullptr;}
}
extern "C" void hm_kws_destroy(void *handle){delete static_cast<Voice*>(handle);}
extern "C" void hm_kws_reset(void *handle){if(!handle)return;auto*v=static_cast<Voice*>(handle);SherpaOnnxResetKeywordStream(v->kws,v->stream);SherpaOnnxVoiceActivityDetectorReset(v->vad);}
extern "C" int32_t hm_kws_accept(void *handle,const float *samples,int32_t count,int32_t detect_keyword){
 if(!handle||!samples||count<1||count>8192)return -1;
 for(int32_t i=0;i<count;++i)if(!std::isfinite(samples[i])||std::abs(samples[i])>1.01f)return -1;
 auto*v=static_cast<Voice*>(handle);int32_t flags=0;
 if(detect_keyword){
  SherpaOnnxOnlineStreamAcceptWaveform(v->stream,16000,samples,count);
  while(SherpaOnnxIsKeywordStreamReady(v->kws,v->stream)){
   SherpaOnnxDecodeKeywordStream(v->kws,v->stream);
   const auto*r=SherpaOnnxGetKeywordResult(v->kws,v->stream);
   if(r&&r->keyword&&strlen(r->keyword))flags|=1;
   if(r)SherpaOnnxDestroyKeywordResult(r);
   if(flags&1){SherpaOnnxResetKeywordStream(v->kws,v->stream);break;}
  }
 }
 SherpaOnnxVoiceActivityDetectorAcceptWaveform(v->vad,samples,count);
 if(SherpaOnnxVoiceActivityDetectorDetected(v->vad))flags|=2;
 while(!SherpaOnnxVoiceActivityDetectorEmpty(v->vad)){flags|=4;SherpaOnnxVoiceActivityDetectorPop(v->vad);}
 return flags;
}
// Meeting boundary VAD is independent of keyword activation and owns no device.
extern "C" void *hm_meeting_vad_create(const char *directory){
 try {
  if(!directory)return nullptr;
  auto v=std::make_unique<Voice>();std::string model=std::string(directory)+"/silero_vad.onnx";
  SherpaOnnxVadModelConfig d{};d.silero_vad.model=model.c_str();d.silero_vad.threshold=.5f;
  d.silero_vad.min_silence_duration=.1f;d.silero_vad.min_speech_duration=.16f;
  d.silero_vad.window_size=512;d.silero_vad.max_speech_duration=60;
  d.sample_rate=16000;d.num_threads=1;d.provider="cpu";
  v->vad=SherpaOnnxCreateVoiceActivityDetector(&d,65.f);if(!v->vad)return nullptr;
  return v.release();
 }catch(...){return nullptr;}
}
