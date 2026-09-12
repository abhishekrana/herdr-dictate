"""Record a real Herdr + dictate + Claude Code session to frames."""
import fcntl, getpass, os, pickle, pty, re, struct, subprocess, sys, termios, threading, time
import pyte

# pyte does not parse the kitty keyboard sequences (CSI > 1 u); a real terminal
# consumes them, so drop them rather than let the parameters reach the screen.
KEYBOARD_PROTOCOL = re.compile(rb"\x1b\[[<>=?]?[0-9;]*u")

HERE = os.path.dirname(os.path.abspath(__file__))
COLS = int(os.environ.get("DEMO_COLS", 120))
ROWS = int(os.environ.get("DEMO_ROWS", 32))
REPO = os.environ.get("DEMO_REPO", "/tmp/config-parser")
PROMPT_WAV = os.environ.get("DEMO_WAV", os.path.join(HERE, "prompt.wav"))
SESSION = os.environ.get("DEMO_SESSION", "dictatedemo")
SOCKET = os.path.expanduser(f"~/.config/herdr/sessions/{SESSION}/herdr.sock")
ANSWER_SECS = float(os.environ.get("DEMO_ANSWER_SECS", 30))
FPS = 10


class Screen(pyte.Screen):
    """pyte rejects the private forms of a few CSI sequences Herdr sends."""

    def report_device_status(self, *args, **kwargs):
        pass


class Term:
    def __init__(self, argv, cwd, env):
        self.master, slave = pty.openpty()
        size = struct.pack("HHHH", ROWS, COLS, 0, 0)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, size)
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, size)
        self.screen = Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.lock = threading.Lock()
        self.proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=slave,
                                     stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        while True:
            try:
                data = os.read(self.master, 65536)
            except OSError:
                return
            if not data:
                return
            with self.lock:
                self.stream.feed(KEYBOARD_PROTOCOL.sub(b"", data))

    def send(self, data):
        os.write(self.master, data if isinstance(data, bytes) else data.encode())

    def text(self):
        with self.lock:
            return "\n".join(self.screen.display)

    def snapshot(self):
        with self.lock:
            buf = self.screen.buffer
            rows = []
            for y in range(ROWS):
                line = buf[y]
                rows.append([(line[x].data, line[x].fg, line[x].bg, line[x].bold,
                              line[x].reverse) for x in range(COLS)])
            return rows


def wait_for(t, needle, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        if needle in t.text():
            return True
        time.sleep(0.2)
    return False


def invoke():
    subprocess.run(
        ["herdr", "plugin", "action", "invoke", "abhishekrana.dictate.toggle-send"],
        env=dict(os.environ, HERDR_SOCKET_PATH=SOCKET), capture_output=True,
    )


def sh(*args):
    return subprocess.run(args, capture_output=True, text=True).stdout.strip()


def audit(frames):
    """Refuse to ship a recording that put the operator on screen."""
    secrets = {os.path.expanduser("~"), getpass.getuser(), os.uname().nodename}
    secrets = {s for s in secrets if len(s) > 2}
    seen = set()
    for _, rows in frames:
        screen = "\n".join("".join(c[0] for c in row) for row in rows)
        seen |= {s for s in secrets if s in screen}
    if seen:
        sys.exit(f"recording shows {sorted(seen)}; not writing frames")
    print("audit: no operator identity on screen")


def main():
    module = sh("pactl", "load-module", "module-null-sink", "sink_name=dictatedemo",
                "sink_properties=device.description=dictatedemo")
    frames = []
    stop = threading.Event()
    t = None
    try:
        env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor")
        env.pop("TMUX", None)
        # A nested Claude Code would inherit this session's markers and say so.
        for key in [k for k in env if k.startswith(("CLAUDE", "ANTHROPIC"))]:
            del env[key]
        t = Term(["herdr", "--session", SESSION], REPO, env)
        time.sleep(6)
        t.send("claude\r")
        if wait_for(t, "trust this folder", 40):
            t.send("\x1b[B")          # down to "Yes, I trust this folder"
            time.sleep(0.4)
            t.send("\r")
        if not wait_for(t, "auto mode on", 60):
            print("warning: Claude prompt marker not seen", file=sys.stderr)
        time.sleep(2)
        t.send("\x0c")               # clear the shell line above Claude's UI
        time.sleep(2)

        def capture():
            while not stop.is_set():
                frames.append((time.time(), t.snapshot()))
                time.sleep(1 / FPS)

        threading.Thread(target=capture, daemon=True).start()
        time.sleep(1.5)

        t.send("\x02")                # prefix
        time.sleep(0.3)
        t.send("V")                   # toggle-send
        wait_for(t, "● dictate", 15)

        for _ in range(60):           # route only this recording stream
            ids = sh("pactl", "list", "short", "source-outputs").splitlines()
            if ids:
                subprocess.run(["pactl", "move-source-output", ids[-1].split()[0],
                                "dictatedemo.monitor"], capture_output=True)
                break
            time.sleep(0.1)
        time.sleep(0.4)
        subprocess.run(["paplay", "--device=dictatedemo", PROMPT_WAV], capture_output=True)
        time.sleep(0.5)
        invoke()                      # stop, transcribe, insert and submit
        time.sleep(ANSWER_SECS)       # let the agent answer
    finally:
        stop.set()
        time.sleep(0.5)
        if t:
            t.proc.terminate()
        time.sleep(1)
        subprocess.run(["herdr", "session", "stop", SESSION], capture_output=True)
        subprocess.run(["herdr", "session", "delete", SESSION], capture_output=True)
        if module.isdigit():
            subprocess.run(["pactl", "unload-module", module], capture_output=True)
    audit(frames)
    with open(os.environ.get("DEMO_FRAMES", "/tmp/herdr-dictate-demo-frames.pkl"), "wb") as fh:
        pickle.dump(frames, fh)
    print("frames:", len(frames))


if __name__ == "__main__":
    main()
