#!/usr/bin/env python3
"""Quick smoke-test: streaming chat completion against local vLLM server."""

from openai import OpenAI

client = OpenAI(base_url="http://localhost:8000/v1", api_key="not-needed")

PROMPT = "Explain pipeline parallelism for LLM inference in exactly three sentences."

print(f"Prompt: {PROMPT}\n")
print("Streaming response:\n" + "-" * 60)

stream = client.chat.completions.create(
    model="nvidia/Qwen3.6-27B-NVFP4",
    messages=[{"role": "user", "content": PROMPT}],
    max_tokens=256,
    temperature=0.7,
    stream=True,
)

for chunk in stream:
    delta = chunk.choices[0].delta
    if delta.content:
        print(delta.content, end="", flush=True)

print("\n" + "-" * 60 + "\nDone.")
