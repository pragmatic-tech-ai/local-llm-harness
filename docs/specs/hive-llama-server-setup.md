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
