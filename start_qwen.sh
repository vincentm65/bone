#!/usr/bin/env bash
set -euo pipefail

# ── start_qwen.sh ──
# Launches vLLM server for nvidia/Qwen3.6-27B-NVFP4 on two heterogenous GPUs:
#   GPU 0: RTX 3090 (24 GB, Ampere, SM 8.6)
#   GPU 1: RTX 5070 Ti (16 GB, Blackwell, SM 12.0)
#
# Pipeline parallelism (PP=2) splits model layers across GPUs so there is
# no per-step AllReduce sync.  Tensor parallelism (TP=1) is deliberately
# avoided, preventing the Ampere card from bottlenecking Blackwell sync.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/.venv/bin/activate"

export CUDA_VISIBLE_DEVICES=0,1
export CUDA_DEVICE_ORDER=PCI_BUS_ID

echo "=== Launching vLLM: nvidia/Qwen3.6-27B-NVFP4 ==="
echo "GPUs: $(nvidia-smi --query-gpu=name --format=csv,noheader | paste -sd ',')"
echo "Pipeline parallelism: 2  |  Tensor parallelism: 1  |  Context: 32k"
echo ""

vllm serve nvidia/Qwen3.6-27B-NVFP4 \
  --host 0.0.0.0 \
  --port 8000 \
  --trust-remote-code \
  --quantization modelopt \
  --dtype bfloat16 \
  --pipeline-parallel-size 2 \
  --tensor-parallel-size 1 \
  --max-model-len 32768 \
  --max-num-seqs 2 \
  --max-num-batched-tokens 32768 \
  --kv-cache-dtype fp8 \
  --gpu-memory-utilization 0.88 \
  --enable-chunked-prefill \
  --enforce-eager \
  --reasoning-parser qwen3
