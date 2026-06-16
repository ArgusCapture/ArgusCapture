# whisper.cpp

## Building

Compile [whisper.cpp](https://github.com/ggml-org/whisper.cpp) with

```sh
cmake -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++ -DCMAKE_INSTALL_PREFIX=/usr/local/ -DWHISPER_USE_SYSTEM_GGML=ON -B build
cmake --build build --config Debug -j 12

cd build
su
make install
```

if you have [llama.cpp](https://github.com/ggml-org/llama.cpp) installed. Otherwise remove the `-DWHISPER_USE_SYSTEM_GGML=ON` from the cmake command.

## Getting a model

```sh
cd models
./download-ggml-model.sh large-v3
```

## Running

```sh
whisper-server \
  -m /path/to/ggml-large-v3.bin \
  --host 0.0.0.0 \
  --port 8080 \
  -t 8 \
  -l auto
```
