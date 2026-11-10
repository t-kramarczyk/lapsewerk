"""Checks a PQ movie Lapsewerk wrote: HDR10 boxes ('mdcv', 'clli') with the measured MaxCLL /
MaxFALL the render printed, and (--timecode N) a timecode track starting at frame N.

Usage: check_movie.py MOVIE RENDER_OUTPUT [--timecode N]
"""

import re
import struct
import sys


def boxes(data, start, end, depth=0):
    """Every box in data[start:end], recursively into the ones that hold boxes: (type, body start, body end)."""
    containers = {b"moov", b"trak", b"mdia", b"minf", b"stbl", b"dinf", b"edts", b"tref", b"udta"}
    at = start
    while at + 8 <= end and depth < 12:
        size, kind = struct.unpack(">I4s", data[at : at + 8])
        head = 8
        if size == 1:
            size = struct.unpack(">Q", data[at + 8 : at + 16])[0]
            head = 16
        elif size == 0:
            size = end - at
        if size < head or at + size > end:
            break
        yield kind, at + head, at + size
        if kind in containers:
            yield from boxes(data, at + head, at + size, depth + 1)
        elif kind == b"stsd":
            # Version/flags and count, then sample entries; a video entry holds boxes after 78 bytes.
            entry = at + head + 8
            if entry + 8 <= at + size:
                esize = struct.unpack(">I", data[entry : entry + 4])[0]
                etype = data[entry + 4 : entry + 8]
                yield etype, entry + 8, entry + esize
                if etype != b"tmcd":
                    yield from boxes(data, entry + 8 + 78, entry + esize, depth + 1)
        at += size


def main():
    movie, output = sys.argv[1], sys.argv[2]
    timecode = int(sys.argv[sys.argv.index("--timecode") + 1]) if "--timecode" in sys.argv else None
    data = open(movie, "rb").read()
    found = {}
    for kind, s, e in boxes(data, 0, len(data)):
        found.setdefault(kind, []).append((s, e))
    # What is in the movie (for the log): box types the walk found, and where the HDR10 and timecode
    # names appear in the bytes at all.
    print(f"{movie}: boxes {sorted(k.decode('latin-1') for k in found)}")
    for name in (b"mdcv", b"clli", b"tmcd", b"SmDm", b"CoLL"):
        at = [i for i in range(len(data)) if data.startswith(name, i)][:5]
        print(f"  {name.decode()}: at {at}")
    printed = open(output).read()
    m = re.search(r"MaxCLL (\d+) cd/m², MaxFALL (\d+) cd/m²", printed)
    assert m, "the render printed no HDR10 light level"
    want = (int(m.group(1)), int(m.group(2)))
    assert b"mdcv" in found, "no mastering display box"
    assert b"clli" in found, "no content light level box"
    for s, e in found[b"clli"]:
        got = struct.unpack(">HH", data[s : s + 4])
        assert got == want, f"clli {got}, measured {want}"
    print(f"{movie}: mdcv, clli {want}")
    if timecode is not None:
        assert b"tmcd" in found, "no timecode track"
        # The timecode sample: the first 4 bytes of the track's data, found through its chunk offset.
        tracks = [t for t in found.get(b"stco", [])]
        values = []
        for s, e in tracks:
            count = struct.unpack(">I", data[s + 4 : s + 8])[0]
            if count >= 1:
                off = struct.unpack(">I", data[s + 8 : s + 12])[0]
                values.append(struct.unpack(">I", data[off : off + 4])[0])
        assert timecode in values, f"timecode start {timecode} not among the tracks' first samples {values}"
        print(f"{movie}: tmcd from frame {timecode}")


main()
