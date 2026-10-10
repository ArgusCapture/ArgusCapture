# Voice commands

Argus Capture can be controlled by voice once a camera is connected. Press a
key (or click the microphone button), say a command such as "take a picture",
and press again. Speech recognition runs **on your
own computer** with [whisper.cpp](https://github.com/ggml-org/whisper.cpp). No
audio leaves the machine, and no cloud service or account is needed.

## How it works?
 
Only two things are needed for voice to work:

1. An Argus Capture binary built with whisper.cpp (this happens automatically).
2. The speech model file (about 148 MB, downloaded once).

## Building whisper.cpp

whisper.cpp comes from the `whisper-rs` crate, which carries its own copy of the
source and compiles it with CMake during `cargo build`. You do **not** need to
clone whisper.cpp or llama.cpp, and the old
`-DWHISPER_USE_SYSTEM_GGML=ON` option does not apply.

The first build compiles `whisper.cpp` and takes several minutes. Later builds
reuse the result. Voice recognition runs on the CPU only; GPU backends are not
enabled.

### Linux (Debian / Ubuntu)

Install the build tools and libraries:

```sh
sudo apt-get update
sudo apt-get install -y \
  build-essential pkg-config cmake clang libclang-dev \
  libglib2.0-dev libasound2-dev libgtk-4-dev libgphoto2-dev
```

`cmake` and a C/C++ compiler build whisper.cpp, `libclang-dev` is needed to
generate the Rust bindings, and `libasound2-dev` provides microphone access.

Then build as usual:

```sh
cargo build --release
```

### Windows (MSYS2 UCRT64)

Install the MSYS2 packages used by the project's CI:

```sh
pacman -S --needed \
  mingw-w64-ucrt-x86_64-toolchain \
  mingw-w64-ucrt-x86_64-pkg-config \
  mingw-w64-ucrt-x86_64-libgphoto2 \
  mingw-w64-ucrt-x86_64-gtk4 \
  mingw-w64-ucrt-x86_64-cmake \
  mingw-w64-ucrt-x86_64-ninja \
  mingw-w64-ucrt-x86_64-clang \
  mingw-w64-ucrt-x86_64-clang-libs
```

Build with the GNU Rust toolchain (`stable-x86_64-pc-windows-gnu`) and point
`LIBCLANG_PATH` at the folder that contains `libclang.dll`, normally
`C:\msys64\ucrt64\bin`.

On Windows, whisper.cpp names its static libraries `ggml.a`, `ggml-base.a` and
`ggml-cpu.a` (no `lib` prefix), but the Rust linker looks for `libggml.a`. The
CI job `build-windows` in `ci.yml` works around this: it builds once, copies the
`.a` files into a folder with a `lib` prefix added, and passes that folder to
rustc with `RUSTFLAGS`. Follow that job when building on Windows, since it is
the tested reference.

## Getting the speech model

The model is **not** bundled with the program. Argus Capture uses the English
`base` model (`ggml-base.en.bin`, about 148 MB). It is downloaded only when you
ask for it.

### Download inside the app (recommended)

1. Open **Edit → Configuration → Voice**.
2. Click **Download**.
3. Wait for the progress bar to finish. The status changes to **Installed**.

You can do this at any time, even before a camera is connected. If you start a
voice command before the model is installed, a **Voice model required** dialog
offers the same download. Closing the Configuration window
while a download runs is safe; progress continues in the status bar.

The Voice tab also shows where the model is stored. The download is written to
a temporary `.part` file and only renamed when it is complete, so an
interrupted download never leaves a broken model behind. Just click Download
again to retry.

### Where the model is stored

| System  | Default folder                                  |
| ------- | ----------------------------------------------- |
| Linux   | `~/.argus-capture/models/`                      |
| Windows | `%USERPROFILE%\.argus-capture\models\`          |

Two environment variables change this:

| Variable                      | Purpose                                            |
| ----------------------------- | -------------------------------------------------- |
| `ARGUS_CAPTURE_VOICE_CACHE`   | Use another folder for the downloaded model        |
| `ARGUS_CAPTURE_VOICE_MODEL`   | Use your own ggml model file and skip the download |

### Offline or manual install

On a machine without internet access, get the same model on another computer.
Either download
`https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin`
directly, or use the script that ships with whisper.cpp:

```sh
cd models
./download-ggml-model.sh base.en
```

Copy `ggml-base.en.bin` into the models folder above, or keep it anywhere and
point to it:

```sh
export ARGUS_CAPTURE_VOICE_MODEL=/path/to/ggml-base.en.bin
```

If `ARGUS_CAPTURE_VOICE_MODEL` points to a file that does not exist, Argus
Capture reports an error. It never downloads over a path you chose.

Other ggml models (for example `large-v3` from the old manual) can be used this
way, but only the `base.en` model is tested. Larger models are much slower on
a CPU, and the language is always set to English.

## Using voice commands

1. **Connect the camera first.** Voice control is only available while the
   camera is connected and the live view is showing. Before that, the
   **Camera → Voice Command** menu item and the microphone button are greyed
   out.
2. Press the voice shortcut (default **`v`**), click the **Voice command**
   button in the right panel (under *Take Picture*), or choose **Camera →
   Voice Command**. The button turns red and reads **Stop listening**, and the
   status bar shows *Listening...*.
3. Say one command, for example "take a picture".
4. Press the shortcut or the button again to stop. The status bar shows *Recognizing...*,
   then what was heard and what was done:

   ```text
   Heard "Take a picture.": capture
   ```

Notes:

- It is push to talk, not hands-free. You press the key to start and again to
  stop.
- If the camera disconnects while you are listening, the microphone is turned
  off automatically.
- Only the first 15 seconds of a recording are used. Commands are short, so
  press the shortcut again as soon as you have spoken.
- Very short or silent recordings are ignored (*No speech detected*).
- The system's default microphone is used. Your operating system must allow the
  app to use it.
- The first command after starting the app also loads the model, so it takes a
  little longer. Later commands are faster. Speed depends on your CPU.
- Recordings are kept in memory only and discarded after recognition. The only
  network use is the one-time model download.

## Available commands

There are three voice commands. Each runs the same action as its menu item and
keyboard shortcut.

| Command    | Say any one of these words                                    | Same as menu item | Default key |
| ---------- | ------------------------------------------------------------- | ----------------- | ----------- |
| Capture    | capture, shoot, photo, picture, snap, shutter                 | Take Picture      | `p`         |
| Focus      | focus, autofocus                                              | Focus             | `f`         |
| Disconnect | disconnect                                                    | Disconnect        | `d`         |

Examples that work: "capture", "take a picture", "snap a photo", "autofocus",
"focus", "disconnect".

There is no voice command to connect. Voice control only exists once the camera
is connected, so use **Camera → Connect** (default key `c`) first.

### How a phrase is understood

- Words are matched **whole**: only the exact words in the table count.
- Capital letters and punctuation do not matter.
- If the phrase contains **more than one** command ("focus and capture"),
  nothing happens.
- If the phrase contains a **negation or stop word** (*not, don't, no, cancel,
  stop, wait*), nothing happens, for example "don't capture" or "wait".
- Anything else is shown in the status bar as *no matching voice command*, so
  you can see what was heard.

Argus Capture deliberately prefers to do nothing over doing the wrong thing, so
a missed command costs you a repeat instead of an unwanted photo.

A command is ignored when its menu item is currently greyed out. Capture uses
the same action as **Take Picture**, so it behaves exactly like that button.

## Changing the voice shortcut

Use **Edit → Configuration → Shortcuts**, click the *Voice Command* shortcut and
press the new key. Or edit the configuration file
(`~/.argus-capture/argus-capture.conf`):

```ini
[ArgusCapture]
voice-listen = v
```

## Troubleshooting

| What you see                                           | What to do                                                                                      |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------- |
| **Voice command** button or menu item is greyed out    | Connect the camera first. Voice is available only while the live view is showing.               |
| **Voice model required** dialog                        | The model is missing. Click Download, or use **Edit → Configuration → Voice**.                  |
| `Voice model not found at ...`                         | The model file was deleted. Download it again from the Voice tab.                               |
| `ARGUS_CAPTURE_VOICE_MODEL points to ..., which does not exist` | Fix the path, or remove the variable to use the normal download.                      |
| *No speech detected*                                   | Speak closer to the microphone, check the system's default input device and its volume.         |
| `Heard "...": no matching voice command`               | The words did not match a command. Use the words in the table, one command at a time.           |
| *Voice input error*                                    | The microphone could not be opened. Check that it is connected and that the app may use it.     |
| Download fails                                         | Check the connection and try again, or use the offline install above.                           |

## Limits

- English only.
- Three commands: capture, focus, disconnect.
- Only available while the camera is connected.
- One command per recording.
- CPU only, using up to 8 threads.