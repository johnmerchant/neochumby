"""Writes touchscreen event files for the Chumby Classic: one press and one
release per screen point, in the 2.6.16 kernel's 16-byte input_event, raw
coordinates from the stock calibration (/psp/ts_settings 169,3733,4082,-4054).
`cat <name>.down > /dev/input/event0` injects a press.

    python3 -I taps.py outdir name=x,y ...
"""
import os
import struct
import sys

EV_SYN, EV_KEY, EV_ABS = 0, 1, 3
ABS_X, ABS_Y, ABS_PRESSURE, BTN_TOUCH = 0, 1, 0x18, 0x14A


def ev(t, c, v):
    return struct.pack("<IIHHi", 0, 0, t, c, v)


def raw(x, y):
    return round(x * 3733 / 320 + 169), round(4082 - y * 4054 / 240)


def main(out, points):
    os.makedirs(out, exist_ok=True)
    for p in points:
        name, xy = p.split("=")
        x, y = map(int, xy.split(","))
        rx, ry = raw(x, y)
        down = ev(EV_ABS, ABS_X, rx) + ev(EV_ABS, ABS_Y, ry) + ev(EV_ABS, ABS_PRESSURE, 200) \
            + ev(EV_KEY, BTN_TOUCH, 1) + ev(EV_SYN, 0, 0)
        up = ev(EV_ABS, ABS_PRESSURE, 0) + ev(EV_KEY, BTN_TOUCH, 0) + ev(EV_SYN, 0, 0)
        open(os.path.join(out, name + ".down"), "wb").write(down)
        open(os.path.join(out, name + ".up"), "wb").write(up)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2:])
