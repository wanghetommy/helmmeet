#include "helmmeet_apm.h"
#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <string>
#include <sys/resource.h>
#include <vector>

namespace {
void check(bool ok, const char *message) { if (!ok) throw std::runtime_error(message); }
struct Engine {
  hm_apm *state = nullptr;
  explicit Engine(int noise, int echo = 0) {
    const hm_apm_config c{1, noise, echo};
    check(hm_apm_create(&c, &state) == 0 && state, "create failed");
  }
  ~Engine() { hm_apm_destroy(state); }
  Engine(const Engine&) = delete;
  Engine& operator=(const Engine&) = delete;
};
void self_test() {
  hm_apm *bad = reinterpret_cast<hm_apm*>(1);
  check(hm_apm_create(nullptr, &bad) == HM_APM_INVALID && !bad, "invalid create must clear output");
  for (auto c : {hm_apm_config{2,1,0}, hm_apm_config{1,3,0}, hm_apm_config{1,1,2}})
    check(hm_apm_create(&c, &bad) == HM_APM_INVALID && !bad, "invalid configuration accepted");
  std::array<float,160> in{}, out{};out.fill(.25f);
  Engine plain(0);
  check(hm_apm_process(nullptr,in.data(),out.data(),160)==HM_APM_INVALID,"null accepted");
  check(hm_apm_process(plain.state,in.data(),out.data(),159)==HM_APM_INVALID,"wrong length accepted");
  for (float invalid : {std::numeric_limits<float>::quiet_NaN(), std::numeric_limits<float>::infinity(),1.01f}) {
    in[40]=invalid;
    check(hm_apm_process(plain.state,in.data(),out.data(),160)==HM_APM_INVALID,"invalid PCM accepted");
    check(std::all_of(out.begin(),out.end(),[](float x){return x==.25f;}),"error changed output");
  }
  in.fill(0);
  check(hm_apm_reset(plain.state)==0,"reset failed");
  check(hm_apm_process(plain.state,in.data(),out.data(),160)==0,"silent frame failed");
  check(std::all_of(out.begin(),out.end(),[](float x){return x==0;}),"silence generated audio");
  for (size_t i=0;i<160;i++) in[i]=.2f*std::sin(float(i)*.15f);
  check(hm_apm_process(plain.state,in.data(),out.data(),160)==0,"bypass failed");
  for(size_t i=0;i<160;i++)check(std::abs(in[i]-out[i])<.000001f,"bypass altered waveform");
  check(hm_apm_process(plain.state,in.data(),in.data(),160)==0,"in-place failed");
  Engine echo(1,1);
  check(hm_apm_process(echo.state,in.data(),out.data(),160)==HM_APM_REFERENCE_REQUIRED,"AEC accepted no reference");
  check(hm_apm_reverse(echo.state,in.data(),160,-1)==HM_APM_INVALID,"negative delay accepted");
  check(hm_apm_reverse(echo.state,in.data(),160,40)==0,"reverse failed");
  check(hm_apm_process(echo.state,in.data(),out.data(),160)==0,"AEC capture failed");
  check(hm_apm_process(echo.state,in.data(),out.data(),160)==HM_APM_REFERENCE_REQUIRED,"stale reference accepted");
  check(hm_apm_reverse(echo.state,in.data(),160,40)==0 && hm_apm_reset(echo.state)==0,"AEC reset failed");
  check(hm_apm_process(echo.state,in.data(),out.data(),160)==HM_APM_REFERENCE_REQUIRED,"reset retained reference");
  check(hm_apm_reverse(echo.state,in.data(),160,40)==0,"reference refresh failed");
  check(hm_apm_reverse(echo.state,in.data(),160,1001)==HM_APM_INVALID,"excess delay accepted");
  check(hm_apm_process(echo.state,in.data(),out.data(),160)==HM_APM_REFERENCE_REQUIRED,"invalid reference retained old data");
  for (int i=0;i<200;i++) { Engine cycle(1);check(hm_apm_process(cycle.state,in.data(),out.data(),160)==0,"lifetime cycle failed"); }
  // Stream reset must match a fresh engine, including noise estimator state.
  Engine a(2), b(2);
  for(int i=0;i<100;i++) check(hm_apm_process(a.state,in.data(),out.data(),160)==0,"warmup failed");
  check(hm_apm_reset(a.state)==0,"NS reset failed");
  std::array<float,160> other{};
  check(hm_apm_process(a.state,in.data(),out.data(),160)==0 && hm_apm_process(b.state,in.data(),other.data(),160)==0,"reset compare failed");
  for(size_t i=0;i<160;i++) check(std::abs(out[i]-other[i])<.000001f,"reset differs from new engine");
}
void little(std::ofstream& f,uint32_t v,size_t n){for(size_t i=0;i<n;i++)f.put(char((v>>(i*8))&255));}
void wav(const std::filesystem::path& path,const std::vector<float>& x){
  std::ofstream f(path,std::ios::binary);check(bool(f),"cannot create synthetic WAV");
  f.write("RIFF",4);little(f,uint32_t(x.size()*2+36),4);f.write("WAVEfmt ",8);little(f,16,4);little(f,1,2);little(f,1,2);little(f,16000,4);little(f,32000,4);little(f,2,2);little(f,16,2);f.write("data",4);little(f,uint32_t(x.size()*2),4);
  for(float v:x)little(f,uint16_t(int16_t(std::clamp(v,-1.f,1.f)*32767)),2);
  check(bool(f),"synthetic WAV write failed");
}
double cpu(const rusage& r){return r.ru_utime.tv_sec+r.ru_utime.tv_usec/1e6+r.ru_stime.tv_sec+r.ru_stime.tv_usec/1e6;}
void benchmark(const std::filesystem::path& directory,int frames){
  std::filesystem::create_directories(directory);
  const size_t samples=size_t(frames)*160;std::vector<float> clean(samples),noisy(samples),enhanced(samples);
  uint32_t random=17391;
  for(size_t i=0;i<samples;i++){
    const double t=double(i)/16000;
    // Synthetic harmonic signal, NOT a speech quality or ASR accuracy fixture.
    const double envelope=std::fmod(t,4.0)<2.5?.5*(1+std::sin(2*3.141592653589793*t*.7)):0;
    clean[i]=float(envelope*(.12*std::sin(2*3.141592653589793*173*t)+.045*std::sin(2*3.141592653589793*346*t)));
    random^=random<<13;random^=random>>17;random^=random<<5;
    const float noise=(float(random&65535)/32767.5f-1.f)*.025f;
    noisy[i]=clean[i]+noise;
  }
  Engine engine(2); std::vector<double> times;times.reserve(frames);
  rusage before{},after{};getrusage(RUSAGE_SELF,&before);
  const auto began=std::chrono::steady_clock::now();
  for(int i=0;i<frames;i++){
    const auto at=std::chrono::steady_clock::now();
    check(hm_apm_process(engine.state,noisy.data()+i*160,enhanced.data()+i*160,160)==0,"benchmark process failed");
    const auto end=std::chrono::steady_clock::now();
    if(i>=100)times.push_back(std::chrono::duration<double,std::milli>(end-at).count());
  }
  const auto end=std::chrono::steady_clock::now();getrusage(RUSAGE_SELF,&after);
  std::sort(times.begin(),times.end());
  auto quantile=[&](double p){return times.at(size_t(p*double(times.size()-1)));};
  double raw=0,processed=0;size_t count=0;
  for(size_t i=16000;i<samples;i++)if(std::fmod(double(i)/16000,4.)>3.){raw+=noisy[i]*noisy[i];processed+=enhanced[i]*enhanced[i];count++;}
  check(count>0 && raw>0 && processed>0,"no noise measurement window");
  wav(directory/"synthetic-clean.wav",clean);wav(directory/"synthetic-noisy.wav",noisy);wav(directory/"synthetic-enhanced.wav",enhanced);
  const double seconds=double(frames)/100;
  std::cout<<"{\"engine\":\""<<hm_apm_version()<<"\",\"audio_seconds\":"<<seconds
    <<",\"frames\":"<<frames<<",\"warmup_frames\":100,\"p50_ms\":"<<quantile(.5)
    <<",\"p95_ms\":"<<quantile(.95)<<",\"p99_ms\":"<<quantile(.99)<<",\"max_ms\":"<<times.back()
    <<",\"process_wall_ms\":"<<std::chrono::duration<double,std::milli>(end-began).count()
    <<",\"cpu_seconds\":"<<cpu(after)-cpu(before)<<",\"realtime_one_core_percent\":"<<(cpu(after)-cpu(before))/seconds*100
    <<",\"process_peak_rss_bytes\":"<<after.ru_maxrss<<",\"synthetic_noise_only_reduction_db\":"<<10*std::log10(raw/processed)<<"}\n";
}
}
int main(int argc,char** argv){
  try{
    if(argc==2 && std::string(argv[1])=="--self-test"){self_test();std::cout<<"APM C ABI self-test passed\n";return 0;}
    if(argc==4 && std::string(argv[1])=="--benchmark"){
      const std::string value=argv[3];size_t used=0;const int frames=std::stoi(value,&used);
      check(used==value.size()&&frames>=1000&&frames<=360000,"frames must be 1000..360000 (10s..1h)");
      benchmark(argv[2],frames);return 0;
    }
    std::cerr<<"Usage: apm-probe --self-test | --benchmark OUTPUT_DIRECTORY FRAMES\n";return 2;
  }catch(const std::exception& e){std::cerr<<"APM probe: "<<e.what()<<'\n';return 1;}
}
