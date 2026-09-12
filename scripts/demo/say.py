#!/usr/bin/env python3
"""Speak a line and capture it as 16 kHz mono WAV, for the demo's microphone.

The line is played through the speakers and recorded off the output monitor,
so no real microphone is involved and the result is identical on every run.
"""

import os
import struct
import subprocess
import sys
import time
import wave


def sh(*args):
    return subprocess.run(args, capture_output=True, text=True).stdout.strip()


def trim(src, dst):
    """Keep the speech, with a short lead-in and tail."""
    with wave.open(src) as w:
        rate, count = w.getframerate(), w.getnframes()
        samples = list(struct.unpack("<%dh" % count, w.readframes(count)))
    peak = max(abs(s) for s in samples)
    if peak == 0:
        sys.exit("captured silence: is the output monitor readable?")
    floor = peak * 0.04
    lo = next(i for i, s in enumerate(samples) if abs(s) > floor)
    hi = len(samples) - next(i for i, s in enumerate(reversed(samples)) if abs(s) > floor)
    pad = rate // 5
    samples = samples[max(0, lo - pad):min(len(samples), hi + pad)]
    with wave.open(dst, "w") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes(struct.pack("<%dh" % len(samples), *samples))
    return len(samples) / rate


def main():
    line, dst = sys.argv[1], sys.argv[2]
    raw = dst + ".raw"
    sink = sh("pactl", "get-default-sink")
    rec = subprocess.Popen(
        ["parec", f"--device={sink}.monitor", "--rate=16000", "--channels=1",
         "--format=s16le", "--file-format=wav", raw],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    time.sleep(0.8)
    subprocess.run(["spd-say", "-w", "-r", "-20", "-i", "-45", line], check=True)
    time.sleep(0.8)
    rec.terminate()
    rec.wait()
    seconds = trim(raw, dst)
    os.remove(raw)
    print(f"{dst}  {seconds:.1f}s")


if __name__ == "__main__":
    main()
