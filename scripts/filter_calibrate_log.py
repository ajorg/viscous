#!/usr/bin/env python3
"""Distil a calibrate_speed.exe log down to what's worth reading.

PowerShell's `*>&1 | Tee-Object` writes UTF-16LE with ANSI colour codes, and
`grafton_visca=trace` logs several routine lines per byte on the wire — a
multi-thousand-line file where almost everything is expected noise. This
decodes it, strips the colour codes, and splits what's left into:

  - READINGS: the example's own printed output (unindented lines have no
    timestamp: "COM5 at...", "speed N:", "  pan ...", "  tilt ...", etc.)
  - ANOMALIES: every timestamped trace/debug/warn line that doesn't match a
    known-routine pattern (successful ACK/completion bookkeeping, the benign
    I/F Clear echo warning, resend-state updates at attempt=0).

Usage:
    python3 filter_calibrate_log.py <path> [--anomalies-only] [--context N]

`--context N` (default 3) prints N lines of raw context (still de-ANSI'd,
not otherwise filtered) around each anomaly, so a real error is seen with
what led up to it rather than in isolation.
"""

import argparse
import re
import sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Lines that are routine bookkeeping for a command/inquiry that completed
# normally. Anything NOT matching one of these, on a timestamped line, is
# printed as a possible anomaly.
ROUTINE = re.compile(
    r"Commands in flight: "
    r"|TX id=\d+ kind=(INQ|CMD)"
    r"|Started inquiry \(phase="
    r"|Started tracking inquiry"
    r"|Sent \d+ bytes: "
    r"|Successfully sent (inquiry|command)"
    r"|Read \d+ bytes from serial port"
    r"|Received \d+ bytes from transport"
    r"|Decoded VISCA response kind=(DataReply|Ack|Completion)"
    r"|Matched cmd_id="
    r"|Content match successful"
    r"|Cleaned up \d+ Sony sequence"
    r"|Inquiry \d+ completed with response"
    r"|Command \d+ completed"
    r"|Sending I/F Clear command"
    r"|Updated existing inquiry state for resend .*attempt=0\b"
    r"|Updated existing command state for resend .*attempt=0\b"
    r"|Malformed VISCA response ignored error=InvalidResponse \{ expected: \"Valid VISCA response\", actual: \[136, 1, 0, 1, 255\]"
    r"|Registered pending ACK for command"
    r"|Registered command as pending ACK \(phase=AwaitingAck\)"
    r"|Command \d+ assigned to socket"
    r"|Assigned command \d+ to S\d+ per camera ACK"
)

# The already-diagnosed "camera briefly refuses a position query sent right
# after a move" cluster (VISCA error 0x41, "Command is not executable") —
# real, expected, and handled by the settle-retry already. Counted rather
# than dumped in full: seeing 1 kept the anomaly `attempt=0` was noise, but
# stayed a fresh discovery.
KNOWN_BUSY_WINDOW = re.compile(
    r"Decoded VISCA response kind=Error\(65\)"
    r"|No match cmd_id=.*response_spec=Builtin\(PanTiltPosition\)"
    r"|Received error 0x41 for socket"
    r"|Content matching failed, using FIFO fallback"
    r"|PanTiltPosition: Payload length \d+ doesn't match pan/tilt format"
)

TIMESTAMPED = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z")


def load(path: str) -> list[str]:
    with open(path, encoding="utf-16") as f:
        text = f.read()
    return ANSI.sub("", text).splitlines()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("path")
    parser.add_argument("--anomalies-only", action="store_true")
    parser.add_argument("--context", type=int, default=3)
    args = parser.parse_args()

    lines = load(args.path)

    known_busy_count = sum(
        1
        for line in lines
        if TIMESTAMPED.match(line) and KNOWN_BUSY_WINDOW.search(line)
    )
    anomaly_indices = [
        i
        for i, line in enumerate(lines)
        if TIMESTAMPED.match(line)
        and not ROUTINE.search(line)
        and not KNOWN_BUSY_WINDOW.search(line)
    ]

    if not args.anomalies_only:
        print("=== READINGS (the example's own output) ===")
        for line in lines:
            if not TIMESTAMPED.match(line):
                print(line)
        print()

    print(
        f"=== KNOWN busy-window (0x41) occurrences: {known_busy_count} "
        "(already diagnosed, settle-retry handles it) ==="
    )
    print()
    print(f"=== ANOMALIES, everything else unexplained ({len(anomaly_indices)} found) ===")
    printed = set()
    for i in anomaly_indices:
        lo, hi = max(0, i - args.context), min(len(lines), i + args.context + 1)
        if lo in printed:
            print("...")
        for j in range(lo, hi):
            marker = ">> " if j == i else "   "
            print(f"{marker}{lines[j]}")
            printed.add(j)
        print()


if __name__ == "__main__":
    main()
