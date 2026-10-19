# -*- coding: utf-8 -*-
"""Drive a real console-hosted TUI process (spec 140 s7 evidence helper).

Launches a process in its own console window, attaches to that console,
injects keystrokes via WriteConsoleInput (key-down records only, matching
what crossterm treats as one Press), reads the live screen buffer via
ReadConsoleOutputCharacterW, and optionally captures a window screenshot.

The screen-buffer text is the primary evidence: it is exactly what a human
operator would see in the terminal. Screenshots are supplementary.

Usage (JSON step script):
  python -I scripts/tui-console-drive.py drive.json

drive.json:
{
  "exe": "C:/.../zealous.exe",
  "args": ["tui"],
  "cwd": "C:/.../repo",
  "env": {"ZEALOUS_BASE_URL": "http://127.0.0.1:PORT/v1", ...},
  "out_dir": "C:/.../dist/tui-evidence",
  "conhost": true,
  "steps": [
    {"wait": 2.0},
    {"expect": "text", "timeout": 15},
    {"expect_not": "text"},
    {"capture": "name.png"},
    {"key": "down"},
    {"key": "a"},
    {"wait_exit": 10}
  ]
}

Keys: single chars ("q","a","d","R","r","n","e","x") or names
(down/up/pgdn/pgup/enter/esc/space).
"""
import ctypes
import json
import os
import subprocess
import sys
import time
from ctypes import wintypes

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
user32 = ctypes.WinDLL("user32", use_last_error=True)

GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
FILE_SHARE_READ = 0x1
FILE_SHARE_WRITE = 0x2
OPEN_EXISTING = 3
INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value

CREATE_NEW_CONSOLE = 0x00000010
KEY_EVENT = 0x0001

VK = {
    "down": 0x28,
    "up": 0x26,
    "pgdn": 0x22,
    "pgup": 0x21,
    "enter": 0x0D,
    "esc": 0x1B,
    "space": 0x20,
}


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class SMALL_RECT(ctypes.Structure):
    _fields_ = [
        ("Left", ctypes.c_short),
        ("Top", ctypes.c_short),
        ("Right", ctypes.c_short),
        ("Bottom", ctypes.c_short),
    ]


class CONSOLE_SCREEN_BUFFER_INFO(ctypes.Structure):
    _fields_ = [
        ("dwSize", COORD),
        ("dwCursorPosition", COORD),
        ("wAttributes", wintypes.WORD),
        ("srWindow", SMALL_RECT),
        ("dwMaximumWindowSize", COORD),
    ]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [
        ("bKeyDown", wintypes.BOOL),
        ("wRepeatCount", wintypes.WORD),
        ("wVirtualKeyCode", wintypes.WORD),
        ("wVirtualScanCode", wintypes.WORD),
        ("UnicodeChar", wintypes.WCHAR),
        ("dwControlKeyState", wintypes.DWORD),
    ]


class INPUT_RECORD_UNION(ctypes.Union):
    _fields_ = [("KeyEvent", KEY_EVENT_RECORD), ("pad", ctypes.c_byte * 16)]


class INPUT_RECORD(ctypes.Structure):
    _anonymous_ = ("u",)
    _fields_ = [
        ("EventType", wintypes.WORD),
        ("u", INPUT_RECORD_UNION),
        ("_pad", ctypes.c_byte * 2),
    ]


def send_key(handle, char=None, name=None):
    """Send a single key-down INPUT_RECORD (one crossterm Press)."""
    rec = INPUT_RECORD()
    rec.EventType = KEY_EVENT
    rec.KeyEvent.bKeyDown = True
    rec.KeyEvent.wRepeatCount = 1
    if name is not None:
        rec.KeyEvent.wVirtualKeyCode = VK[name]
        rec.KeyEvent.UnicodeChar = "\r" if name == "enter" else "\x1b" if name == "esc" else " "
        if name == "down":
            rec.KeyEvent.UnicodeChar = " "
    else:
        rec.KeyEvent.wVirtualKeyCode = ord(char.upper())
        rec.KeyEvent.UnicodeChar = char
    written = wintypes.DWORD(0)
    ok = kernel32.WriteConsoleInputW(handle, ctypes.byref(rec), 1, ctypes.byref(written))
    if not ok:
        raise OSError("WriteConsoleInputW failed: %s" % ctypes.get_last_error())


CAPTURE_TITLE = "zealous-drive-capture"


class Console:
    """Attach to a child console and read its screen buffer."""

    def __init__(self, pid):
        kernel32.FreeConsole()
        if not kernel32.AttachConsole(pid):
            raise OSError("AttachConsole failed: %s" % ctypes.get_last_error())
        # Tag the console window so screenshot lookup finds exactly this
        # console (not some other terminal window on the desktop)
        kernel32.SetConsoleTitleW(CAPTURE_TITLE)
        self.hout = kernel32.CreateFileW(
            "CONOUT$", GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, 0, None)
        self.hin = kernel32.CreateFileW(
            "CONIN$", GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, 0, None)
        if self.hout == INVALID_HANDLE_VALUE or self.hin == INVALID_HANDLE_VALUE:
            raise OSError("CONIN$/CONOUT$ open failed: %s" % ctypes.get_last_error())

    def size(self):
        csbi = CONSOLE_SCREEN_BUFFER_INFO()
        if not kernel32.GetConsoleScreenBufferInfo(self.hout, ctypes.byref(csbi)):
            raise OSError("GetConsoleScreenBufferInfo failed")
        return csbi.dwSize.X, csbi.dwSize.Y

    def screen_text(self):
        cols, rows = self.size()
        lines = []
        for y in range(rows):
            buf = ctypes.create_unicode_buffer(cols)
            read = wintypes.DWORD(0)
            pos = COORD(0, y)
            ok = kernel32.ReadConsoleOutputCharacterW(
                self.hout, buf, cols, pos, ctypes.byref(read))
            if not ok:
                raise OSError("ReadConsoleOutputCharacterW failed")
            lines.append(buf.value.rstrip())
        return "\n".join(lines)

    def key_char(self, char):
        send_key(self.hin, char=char)

    def key_name(self, name):
        send_key(self.hin, name=name)

    def close(self):
        try:
            kernel32.CloseHandle(self.hout)
            kernel32.CloseHandle(self.hin)
        finally:
            kernel32.FreeConsole()


def _rect_ok(hwnd):
    rect = wintypes.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
        return False
    return (rect.right - rect.left) > 0 and (rect.bottom - rect.top) > 0


def window_of_pid(pid):
    """Find the tagged console window (SetConsoleTitleW at attach time).
    Falls back to pid/class lookup. Skips 0x0 pseudo windows."""
    candidates = []
    title_buf = ctypes.create_unicode_buffer(256)

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def enum(hwnd, _):
        if not user32.IsWindowVisible(hwnd) or not _rect_ok(hwnd):
            return True
        n = user32.GetWindowTextW(hwnd, title_buf, 256)
        if n and CAPTURE_TITLE in title_buf.value:
            candidates.append(hwnd)
            return False
        return True

    user32.EnumWindows(enum, 0)
    if candidates:
        return candidates[0]

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def enum2(hwnd, _):
        if not user32.IsWindowVisible(hwnd) or not _rect_ok(hwnd):
            return True
        wpid = wintypes.DWORD(0)
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(wpid))
        if wpid.value == pid:
            candidates.append(hwnd)
        return True

    user32.EnumWindows(enum2, 0)
    return candidates[0] if candidates else None


def screenshot(out_dir, name, pid):
    """Capture the console window as PNG via PowerShell + System.Drawing.

    The PowerShell process inherits the attached console (DETACHED_PROCESS
    silently fails to create the bitmap in this environment). It prints
    nothing, so the TUI screen is not disturbed; the TUI repaints every
    100ms anyway. Keystrokes after a capture remain functional.
    """
    hwnd = window_of_pid(pid)
    if hwnd is None:
        return "no visible window (skipped)"
    os.makedirs(out_dir, exist_ok=True)
    path = os.path.join(out_dir, name).replace("\\", "/")
    rect = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    x, y = rect.left, rect.top
    w, h = rect.right - rect.left, rect.bottom - rect.top
    if w <= 0 or h <= 0:
        return "window rect invalid (%dx%d)" % (w, h)
    # HWND_TOPMOST = -1 during capture so the window is unoccluded
    user32.SetWindowPos(hwnd, ctypes.c_void_p(-1), 0, 0, 0, 0, 0x0001 | 0x0002)
    time.sleep(0.3)
    ps = (
        "$ErrorActionPreference = 'Stop'; "
        "Add-Type -AssemblyName System.Drawing; "
        "$b = New-Object -TypeName System.Drawing.Bitmap -ArgumentList {0}, {1}; "
        "$g = [System.Drawing.Graphics]::FromImage($b); "
        "$g.CopyFromScreen({2}, {3}, 0, 0, $b.Size); "
        "$b.Save('{4}'); $g.Dispose(); $b.Dispose()"
    ).format(w, h, x, y, path)
    r = subprocess.run(
        ["powershell", "-NoProfile", "-Command", ps],
        capture_output=True,
    )
    user32.SetWindowPos(hwnd, ctypes.c_void_p(-2), 0, 0, 0, 0, 0x0001 | 0x0002)
    if r.returncode == 0 and os.path.exists(path):
        return "saved %s (%dx%d)" % (path, w, h)
    err = (r.stderr or b"")[-300:].decode("utf-8", "replace")
    return "screenshot failed rc=%s: %s" % (r.returncode, err)


def drive(cfg):
    exe = cfg["exe"]
    args = cfg.get("args", ["tui"])
    cwd = cfg["cwd"]
    out_dir = cfg.get("out_dir", "dist/tui-evidence")
    env = dict(os.environ)
    env.update({k: str(v) for k, v in cfg.get("env", {}).items()})
    os.makedirs(out_dir, exist_ok=True)

    launcher = cfg.get("launcher")  # e.g. ["conhost.exe"] to force classic console
    cmdline = [exe] + args
    if launcher:
        cmdline = list(launcher) + cmdline
    proc = subprocess.Popen(
        cmdline, cwd=cwd, env=env,
        creationflags=CREATE_NEW_CONSOLE)
    print("[drive] launched pid=%d: %s" % (proc.pid, " ".join(cmdline)))
    time.sleep(1.5)

    con = Console(proc.pid)
    print("[drive] attached; console %dx%d" % con.size())

    log = []
    exit_code = None
    try:
        for i, step in enumerate(cfg["steps"]):
            if "wait" in step:
                time.sleep(float(step["wait"]))
                print("[step %d] wait %s" % (i, step["wait"]))
            elif "expect" in step:
                needle = step["expect"]
                deadline = time.time() + float(step.get("timeout", 15))
                found = False
                while time.time() < deadline:
                    text = con.screen_text()
                    if needle in text:
                        found = True
                        break
                    time.sleep(0.4)
                status = "OK" if found else "MISS"
                print("[step %d] expect %r -> %s" % (i, needle, status))
                log.append((i, "expect", needle, status))
                if not found:
                    snap = con.screen_text()
                    with open(os.path.join(out_dir, "screen-at-fail-%d.txt" % i),
                              "w", encoding="utf-8") as f:
                        f.write(snap)
                    print("[drive] screen dump written (step %d)" % i)
                    return 2, log, exit_code
            elif "expect_not" in step:
                text = con.screen_text()
                needle = step["expect_not"]
                status = "MISS" if needle not in text else "FOUND"
                print("[step %d] expect_not %r -> %s" % (i, needle, status))
                log.append((i, "expect_not", needle, status))
                if needle in text:
                    return 2, log, exit_code
            elif "capture" in step:
                name = step["capture"]
                snap = con.screen_text()
                with open(os.path.join(out_dir, name + ".txt"), "w",
                          encoding="utf-8") as f:
                    f.write(snap)
                msg = screenshot(out_dir, name + ".png", proc.pid)
                print("[step %d] capture %s | %s" % (i, name, msg))
                log.append((i, "capture", name, msg))
            elif "key" in step:
                k = step["key"]
                if len(k) == 1:
                    con.key_char(k)
                else:
                    con.key_name(k)
                print("[step %d] key %r" % (i, k))
                time.sleep(step.get("after", 0.5))
            elif "keys" in step:
                for k in step["keys"]:
                    if len(k) == 1:
                        con.key_char(k)
                    else:
                        con.key_name(k)
                    time.sleep(step.get("after", 0.35))
                print("[step %d] keys %r" % (i, step["keys"]))
            elif "wait_exit" in step:
                try:
                    exit_code = proc.wait(timeout=float(step["wait_exit"]))
                    print("[step %d] exit code %s" % (i, exit_code))
                    log.append((i, "exit", "", exit_code))
                except subprocess.TimeoutExpired:
                    proc.kill()
                    print("[step %d] exit TIMEOUT (killed)" % i)
                    log.append((i, "exit", "", "TIMEOUT"))
                    return 3, log, exit_code
            else:
                raise ValueError("unknown step: %r" % step)
    finally:
        con.close()
        if proc.poll() is None:
            proc.kill()
    return 0, log, exit_code


def main():
    if len(sys.argv) != 2:
        print("usage: tui-console-drive.py drive.json")
        return 1
    with open(sys.argv[1], encoding="utf-8") as f:
        cfg = json.load(f)
    code, log, exit_code = drive(cfg)
    print(json.dumps({"result": code, "exit_code": exit_code, "log": log},
                     ensure_ascii=False))
    return code


if __name__ == "__main__":
    sys.exit(main())
