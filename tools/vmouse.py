#!/usr/bin/env python3
"""Minimal virtual mouse over /dev/uinput (no dependencies).

Usage:
  vmouse.py drag X1 Y1 X2 Y2 [--steps N] [--hold MS] [--mod ctrl|shift]
  vmouse.py click X Y [--button left|right|middle]
  vmouse.py move X Y

Positions are Hyprland logical coordinates (what `hyprctl cursorpos` prints).
Absolute positioning is done by moving relative to the cursor position that
Hyprland reports, then correcting in a few rounds, so pointer acceleration
does not matter.
"""
import fcntl
import os
import struct
import subprocess
import sys
import time

# linux/uinput.h and linux/input-event-codes.h constants
UI_DEV_CREATE = 0x5501
UI_DEV_DESTROY = 0x5502
UI_SET_EVBIT = 0x40045564
UI_SET_KEYBIT = 0x40045565
UI_SET_RELBIT = 0x40045566
UI_DEV_SETUP = 0x405C5503
EV_SYN, EV_KEY, EV_REL = 0x00, 0x01, 0x02
SYN_REPORT = 0
REL_X, REL_Y = 0x00, 0x01
BTN_LEFT, BTN_RIGHT, BTN_MIDDLE = 0x110, 0x111, 0x112
KEY_LEFTCTRL, KEY_LEFTSHIFT = 29, 42
BUS_USB = 0x03


class VKeyboard:
    """A uinput keyboard with enough keys that udev tags it ID_INPUT_KEYBOARD,
    so libinput and the compositor treat its modifiers like a real keyboard."""

    def __init__(self):
        self.fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_KEY)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_SYN)
        for code in range(1, 128):  # ESC, digits, letters, modifiers, F-keys…
            fcntl.ioctl(self.fd, UI_SET_KEYBIT, code)
        setup = struct.pack("HHHH80sI", BUS_USB, 0x1234, 0x9abc, 1, b"fcx-test-keyboard", 0)
        fcntl.ioctl(self.fd, UI_DEV_SETUP, setup)
        fcntl.ioctl(self.fd, UI_DEV_CREATE)
        time.sleep(1.0)

    def close(self):
        fcntl.ioctl(self.fd, UI_DEV_DESTROY)
        os.close(self.fd)

    def key(self, code, down):
        os.write(self.fd, struct.pack("llHHi", 0, 0, EV_KEY, code, 1 if down else 0))
        os.write(self.fd, struct.pack("llHHi", 0, 0, EV_SYN, SYN_REPORT, 0))


class VMouse:
    def __init__(self):
        self.fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_KEY)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_REL)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_SYN)
        for code in (BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, KEY_LEFTCTRL, KEY_LEFTSHIFT):
            fcntl.ioctl(self.fd, UI_SET_KEYBIT, code)
        for code in (REL_X, REL_Y):
            fcntl.ioctl(self.fd, UI_SET_RELBIT, code)
        # struct uinput_setup { struct input_id id; char name[80]; __u32 ff_effects_max; }
        setup = struct.pack("HHHH80sI", BUS_USB, 0x1234, 0x5678, 1, b"fcx-test-mouse", 0)
        fcntl.ioctl(self.fd, UI_DEV_SETUP, setup)
        fcntl.ioctl(self.fd, UI_DEV_CREATE)
        time.sleep(0.6)  # let libinput/Hyprland pick the device up

    def close(self):
        fcntl.ioctl(self.fd, UI_DEV_DESTROY)
        os.close(self.fd)

    def emit(self, etype, code, value):
        # struct input_event { timeval time; __u16 type; __u16 code; __s32 value; }
        os.write(self.fd, struct.pack("llHHi", 0, 0, etype, code, value))

    def syn(self):
        self.emit(EV_SYN, SYN_REPORT, 0)

    def rel(self, dx, dy):
        if dx:
            self.emit(EV_REL, REL_X, dx)
        if dy:
            self.emit(EV_REL, REL_Y, dy)
        self.syn()

    def key(self, code, down):
        self.emit(EV_KEY, code, 1 if down else 0)
        self.syn()

    @staticmethod
    def pos():
        out = subprocess.run(["hyprctl", "cursorpos"], capture_output=True, text=True).stdout
        x, y = out.strip().split(",")
        return int(x), int(y)

    def move_to(self, x, y, steps=1):
        """Converge on (x, y) with relative moves, correcting for acceleration."""
        for _ in range(12):
            cx, cy = self.pos()
            dx, dy = x - cx, y - cy
            if dx == 0 and dy == 0:
                return
            for i in range(steps):
                self.rel(round(dx * (i + 1) / steps) - round(dx * i / steps),
                         round(dy * (i + 1) / steps) - round(dy * i / steps))
                time.sleep(0.01)
            time.sleep(0.05)


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    cmd, args = argv[1], argv[2:]
    opts = {"steps": 25, "hold": 150, "mod": None, "button": "left"}
    pos = []
    i = 0
    while i < len(args):
        if args[i].startswith("--"):
            opts[args[i][2:]] = args[i + 1]
            i += 2
        else:
            pos.append(int(args[i]))
            i += 1
    steps, hold = int(opts["steps"]), int(opts["hold"]) / 1000
    btn = {"left": BTN_LEFT, "right": BTN_RIGHT, "middle": BTN_MIDDLE}[opts["button"]]
    mod = {"ctrl": KEY_LEFTCTRL, "shift": KEY_LEFTSHIFT, None: None}[opts["mod"]]

    m = VMouse()
    kb = VKeyboard() if mod else None
    try:
        if cmd == "move":
            m.move_to(*pos)
        elif cmd == "click":
            m.move_to(*pos)
            time.sleep(0.1)
            m.key(btn, True)
            time.sleep(0.06)
            m.key(btn, False)
        elif cmd == "drag":
            x1, y1, x2, y2 = pos
            m.move_to(x1, y1)
            time.sleep(0.15)
            if mod:
                kb.key(mod, True)
                time.sleep(0.15)
            m.key(BTN_LEFT, True)
            time.sleep(hold)
            # Small initial nudge so GTK passes its drag threshold, then glide.
            m.rel(3, 3)
            time.sleep(0.05)
            m.move_to(x2, y2, steps=steps)
            time.sleep(0.25)
            m.key(BTN_LEFT, False)
            time.sleep(0.1)
            if mod:
                kb.key(mod, False)
        else:
            print(__doc__)
            return 2
        print("final cursor:", m.pos())
    finally:
        time.sleep(0.2)
        m.close()
        if kb:
            kb.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
