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
