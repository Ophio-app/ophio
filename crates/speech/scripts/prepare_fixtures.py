#!/usr/bin/env python3
"""Explicitly prepare public benchmark audio; never called by the engine.

JFK: public-domain U.S. federal government recording, 1961-01-20.
https://www.jfklibrary.org/asset-viewer/archives/usg-17
Mirror: whisper.cpp's 11-second PCM16 excerpt, pinned below.
The technical text and its generated waveform are dedicated to CC0-1.0.
Requires preinstalled espeak and ffmpeg; installs no software.
"""
import argparse
import hashlib
from pathlib import Path
import subprocess
import urllib.request

JFK_URL = (
    "https://raw.githubusercontent.com/ggml-org/whisper.cpp/"
    "b0a11594aec50892a02cd8d129eee2dfe93a8bb8/samples/jfk.wav"
)
JFK_SHA256 = "59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e"
TECHNICAL = (
    "The button is disabled. Open the terminal and run cargo test. "
    "The request returned status five hundred. "
    "Fix the Rust compiler error in the parser."
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    args.directory.mkdir(parents=True, exist_ok=True)
    # Prevent downloaded/generated audio or model weights from entering git.
    ignore = args.directory / ".gitignore"
    if not ignore.exists():
        ignore.write_text("*\n")
    audio = urllib.request.urlopen(JFK_URL, timeout=30).read()
    if len(audio) != 352078 or hashlib.sha256(audio).hexdigest() != JFK_SHA256:
        raise ValueError("public fixture failed verification")
    (args.directory / "jfk.wav").write_bytes(audio)
    original = args.directory / "technical-original.wav"
    subprocess.run(["espeak", "-v", "en-us", "-s", "155", "-w", str(original), TECHNICAL], check=True)
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(original),
        "-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le",
        str(args.directory / "technical.wav"),
    ], check=True)
    print("Verified public-domain speech and generated CC0 technical audio.")


if __name__ == "__main__":
    main()
