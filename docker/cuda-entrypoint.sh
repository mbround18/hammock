#!/bin/sh
# Entrypoint for the accelerated image only.
#
# The binary is linked against libcuda.so.1 — the NVIDIA *driver* library, which
# is deliberately not in this image. The NVIDIA Container Toolkit injects the
# host's copy at run time when the container is given a GPU.
#
# When it is not given one, that library is simply absent and the dynamic loader
# refuses to start the process at all:
#
#   hammock: error while loading shared libraries: libcuda.so.1: cannot open
#   shared object file: No such file or directory
#
# That is a failure to start, which FR-009 forbids: a requested GPU being
# unavailable must degrade to CPU, not prevent the bot from running. So when no
# driver is present we put CUDA's own stub library on the search path. It
# resolves the symbols, every CUDA call fails cleanly, ggml reports zero
# devices, and the bot warns `no_device` and transcribes on the CPU — which is
# exactly what an operator who forgot `--gpus all` should see.
#
# The stub is added ONLY when no real driver was found, so it can never shadow
# an injected driver on a host that does have one — which is the failure mode
# that would make this fix worse than the bug it repairs.

set -e

if ! ldconfig -p 2>/dev/null | grep -q 'libcuda\.so\.1'; then
    echo "hammock: no NVIDIA driver found in this container; using the CUDA stub" \
         "so the bot can start and fall back to CPU. Pass --gpus to use a GPU." >&2
    LD_LIBRARY_PATH="/opt/cuda-stubs${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
    export LD_LIBRARY_PATH
fi

exec hammock "$@"
