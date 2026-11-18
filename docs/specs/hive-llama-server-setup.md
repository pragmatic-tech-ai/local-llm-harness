# llama-server on the hive-laptop

Status: running, reachable from the dev machine.

- URL: http://192.168.1.101:8080 (OpenAI-compatible at /v1/chat/completions, raw at /completion)
- Build: llama.cpp b12470 (commit dde9de82c), official ggml-org release `llama-b12470-bin-win-cuda-12.4-x64.zip` plus `cudart-llama-bin-win-cuda-12.4-x64.zip` (CUDA 12.4 runtime DLLs). Driver 621.24 is far newer, so compatible.
- Path on hive: `C:\llm-harness\llama\llama-server.exe` (zips also left in that dir).
- Model (reused in place): `C:\Users\visor\.lmstudio\models\lmstudio-community\gemma-4-12B-it-QAT-GGUF\gemma-4-12B-it-QAT-Q4_0.gguf`

## Launch command
```
llama-server.exe -m "<gguf>" --host 0.0.0.0 --port 8080 -ngl 28 -c 4096 --parallel 1
```
`-ngl 28` chosen after testing: 16 gave ~3.7 GB VRAM and ~7 tok/s; 28 uses ~5.3 GB of 6 GB and gives ~10.9 tok/s. Higher was not tried (headroom is small).

## Keeping it running / restart
Launched via a scheduled task `llama-server` that runs `C:\llm-harness\llama\start-llama.cmd` (holds the command above, logs to `C:\llm-harness\llama\server.log`). The task is a one-shot, so it does not auto-start at boot.
- Restart: `ssh pragmatic-hive 'Stop-Process -Name llama-server -Force; schtasks /Run /TN llama-server'`
- Change -ngl: edit `start-llama.cmd` on the hive, then restart. Model load takes about 1 to 2 minutes.

## Firewall
- Added inbound allow rule "llama-server 8080" (TCP 8080, any profile).
- Windows had auto-created two Block rules named `llama-server` (TCP and UDP, program `C:\llm-harness\llama\llama-server.exe`, Private/Public) when the exe first ran. Block beats Allow, so they made the port unreachable. I deleted those two rules (they covered only our new exe).

## Verification
From the dev machine, `curl http://192.168.1.101:8080/props` returns `"n_ctx":4096`, `"model_alias"` and `"model_path"` equal to the gguf path above, `"build_info":"b12470-dde9de82c"`.
Generation works. Throughput at -ngl 28: about 10.9 tok/s decode (about 6.5 to 7.5 tok/s at -ngl 16); prompt processing is slow (about 10 to 14 tok/s on tiny prompts, not representative).

## Caveats
- Gemma 4 is a thinking model: `/v1/chat/completions` puts output in `reasoning_content` first, and `content` stays empty until thinking ends, so small max_tokens yields empty `content`. Raw `/completion` without the chat template produces degenerate output; use the chat endpoint.
- Server has no API key and CORS allows all origins; it is open to the whole LAN.
- Not auto-started on reboot; only 4096 context.
- Do not start LM Studio concurrently (VRAM contention).

## In-process build toolchain (for the `llamacpp` engine)

Installed 2026-11-18 so the hive can build the backend's in-process llama.cpp
engine (`cargo build --features llamacpp`) itself. GPU is the RTX 3060 Laptop
(6 GB, Ampere `sm_86`), driver 621.24.

- **Rust** 1.100 (already present).
- **Visual Studio Build Tools 2022** 17.14 with the C++ workload — MSVC
  `cl.exe` 14.44 at
  `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\`.
  Install: `winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"`.
- **CUDA Toolkit** 13.5 (`nvcc` at
  `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5\bin\nvcc.exe`).
  Install: `winget install --id Nvidia.CUDA -e`. CUDA 13 still supports Ampere
  `sm_86`; driver 621.24 supports CUDA 13.5.
- **CMake** 4.4.4 (`C:\Program Files\CMake\bin\cmake.exe`). `winget install --id Kitware.CMake -e`.
- **LLVM/clang** 22.1.8 (`libclang.dll` at `C:\Program Files\LLVM\bin`), needed
  by bindgen. `winget install --id LLVM.LLVM -e`. Machine env var
  `LIBCLANG_PATH=C:\Program Files\LLVM\bin` is set so bindgen finds it.

A CUDA build must run from an MSVC developer environment (so `cl.exe` is on
PATH for nvcc's host compiler). From a plain shell, initialise it with
`vcvars64.bat` (under the BuildTools `VC\Auxiliary\Build\`) and ensure the CUDA
and CMake `bin` directories and `LIBCLANG_PATH` are on PATH before invoking
`cargo`.

## In-process engine: build, run, switch

Source + build live at `C:\llm-harness\harness-build\backend` (a snapshot of the
backend crate; the git branch is the source of truth). Two helper scripts sit in
`C:\llm-harness\harness-build\`:

- **`build-llamacpp.cmd`** — calls `vcvars64.bat`, puts CUDA 13.5 + CMake on PATH,
  sets `CMAKE_CUDA_ARCHITECTURES=86` (the 3060), and runs
  `cargo build --release --features llamacpp`. First build ~11 min (compiles
  llama.cpp + ggml-cuda). Binary: `backend\target\release\backend.exe`.
- **`start-llama-backend.cmd`** — sets `HARNESS_ENGINE=llamacpp`,
  `HARNESS_MODEL=<gguf>`, `HARNESS_N_CTX=4096`, `HARNESS_NGL=28`,
  `HARNESS_BIND=0.0.0.0:8777`, and runs the exe (logs to `backend.out`/`backend.err`).

Run via scheduled task **`harness-backend`** (mirrors the `llama-server` task;
`Run As visor`, interactive, one-time). It must be a task, not a bare ssh launch —
OpenSSH puts session children in a job object that is killed on disconnect.

Firewall: inbound allow rule **"harness-backend 8777"** (TCP 8777) was added.

**The two engines are mutually exclusive on the 6 GB GPU** — only one GPU server
runs at a time (llama-server uses ~5.1 GB at `-ngl 28`; the in-process engine uses
~4.7 GB).

- Switch to the in-process (`llamacpp`) engine:
  `ssh pragmatic-hive 'Stop-Process -Name llama-server -Force; schtasks /Run /TN harness-backend'`
  then point the app at it with env `HARNESS_BACKEND_URL=ws://192.168.1.101:8777`
  (the app then connects remotely instead of spawning its local sidecar).
- Switch back to `llama-server` (the HTTP engine):
  `ssh pragmatic-hive 'schtasks /End /TN harness-backend; Stop-Process -Name backend -Force; schtasks /Run /TN llama-server'`

Verified 2026-11-18: `apply_template` output is byte-identical to llama-server's
`/apply-template` for both `enable_thinking` values; `model_props`, streaming
`turn_chunk`s with top-5 per-token logprobs, and the `max_tokens` cap-stop all work;
model offloaded to GPU (~4.7 GB VRAM).

## candle engine assets (Phase B)

The pure-Rust `candle` engine runs a smaller model (candle has no layer-offload, so
the 12B Q4_0 does not fit 6 GB). Downloaded 2026-11-18, both non-gated:

- GGUF: `C:\llm-harness\models\gemma-3-4B-it-QAT-Q4_0.gguf` (2.2 GB) from
  `lmstudio-community/gemma-3-4B-it-qat-GGUF` (QAT Q4_0, same source family as the 12B).
- Tokenizer: `C:\llm-harness\models\gemma-3-4b-tokenizer.json` (31.8 MB) from
  `unsloth/gemma-3-4b-it` (candle's Gemma gguf loader does not build a tokenizer from
  the gguf, so it is supplied separately). NB: PowerShell `ConvertFrom-Json` wrongly
  rejects this file (case-insensitive key clash on Gemma byte tokens); the Rust
  `tokenizers` crate reads it fine — round-trip confirmed at engine load.
