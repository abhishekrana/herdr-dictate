#!/bin/sh
# Record assets/demo.gif: a real Herdr session, driven end to end.
#
# Creates a throwaway sample repo and Herdr session, speaks the instruction into
# a temporary virtual microphone, lets the plugin dictate it into the agent's
# pane, and renders what the terminal drew. Everything it creates is removed.
#
#   ./assets/make-demo.sh
#   DEMO_LINE="Rename the parser module." ./assets/make-demo.sh
set -eu

cd "$(dirname "$0")"
LINE=${DEMO_LINE:-"Add a comment to the parse config function explaining what it returns."}
REPO=${DEMO_REPO:-/tmp/config-parser}
VENV=${DEMO_VENV:-/tmp/herdr-dictate-demo-venv}
export DEMO_REPO

missing=
for tool in uv herdr claude pactl parec paplay spd-say ffmpeg fc-match git; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
[ -z "$missing" ] || {
    echo "missing:$missing" >&2
    exit 1
}

# pyte renders what Herdr draws; Pillow turns each screen into a frame.
[ -x "$VENV/bin/python" ] || uv venv --quiet "$VENV"
uv pip install --quiet --python "$VENV/bin/python" pyte pillow

# A repo small enough to read in one frame, so the agent has real work to do.
if [ "$REPO" = "/tmp/config-parser" ] && [ ! -d "$REPO" ]; then
    mkdir -p "$REPO/src"
    cat >"$REPO/src/parser.py" <<'EOF'
def parse_config(text):
    settings = {}
    for line in text.splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            settings[key.strip()] = value.strip()
    return settings
EOF
    printf '# config-parser\n\nA tiny config parser.\n' >"$REPO/README.md"
    git -C "$REPO" init -q
    git -C "$REPO" add -A
    git -C "$REPO" -c user.name=demo -c user.email=demo@example.com commit -qm init
fi

./say.py "$LINE" prompt.wav
"$VENV/bin/python" record.py
"$VENV/bin/python" render.py
rm -f frames.pkl
echo "wrote $(pwd)/demo.gif"
