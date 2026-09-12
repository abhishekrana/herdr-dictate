# Remote delivery

Dictating into a pane on a saved SSH machine. The microphone, model and GPU stay on the client; only the transcript
crosses.

## Problem

A dictation resolves the focused pane from local context and writes it through the local socket. When a saved machine
is selected, the key is still handled by the client, which runs the local plugin against the local server, so the
words land locally. Selecting a machine retargets nothing.

## Why not in-band

Verified against Herdr 0.9.0:

- The socket API is single-server. Nothing in its schema addresses another machine, and no command takes a machine
  flag. Pane ids are meaningful only to the server that issued them.
- Herdr does not copy plugins or executables onto SSH hosts. Plugins advertised by the selected server run there,
  which for dictation is the machine without a microphone.
- There is no client-side input injection. The input routing that follows selection is the terminal path, which has
  no API surface.

Delivery therefore leaves the process over SSH, the way Herdr reaches its own remote servers.

## Options

Measured against a host at 22.8 ms round-trip; the local socket is today's baseline.

| Option                                       | Per delivery | Needs on the remote  | Outcome    |
| -------------------------------------------- | ------------ | -------------------- | ---------- |
| Local socket (today)                         | 0.13 ms      | -                    | -          |
| Plugin installed remotely                    | -            | microphone, model    | Rejected   |
| Client-side input routing                    | -            | -                    | Rejected   |
| SSH exec driving the remote CLI              | 61 ms        | nothing new          | **Chosen** |
| Persistent stdio bridge to the remote socket | 23 ms        | this binary          | Rejected   |
| `ssh -L` socket forward                      | 23 ms        | streamlocal or socat | Rejected   |

The first two are impossible rather than slow. The remote spawn is not the cost - driving the remote CLI measured
61.6 ms against 61.4 ms for a bare remote `true` - so the overhead is SSH exec channel setup.

## Decision

Drive the remote Herdr CLI over SSH exec, resolving the target from scratch every dictation.

The deciding property is statelessness, not speed. A held-open bridge owns reconnect, health, backoff and teardown,
and caches a view that goes stale when the remote server restarts. Re-resolving has none of that: a dropped
connection leaves nothing to recover.

A warm channel is an optimisation, never a dependency - the rule this codebase already applies to the model server.
Warm costs about 61 ms, cold about 512 ms, both invisible against seconds of speech and transcription. Only a cold
connect on the keypress path is perceptible, so the channel is warmed from a startup hook and held with
`ControlPersist`.

## Resolving

Per dictation, nothing cached:

1. `herdr machine list --json` on the client. The entry with `selected` true gives the SSH target.
2. `herdr api snapshot` on that machine. `focused_pane_id` is the pane the user is looking at.
3. `herdr agent list` on that machine, for whether that pane hosts an agent.

No selected machine collapses to the existing local path.

## Delivering

An agent pane takes `herdr agent prompt <name>`: it submits text and Enter as one operation honouring bracketed
paste, and refuses with `agent_blocked` at an approval dialog, so a transcript is never typed into a prompt as stray
keys. The name follows the pane's occupant, so a stale target fails loudly.

Anything else takes `herdr pane send-text`, then `herdr pane send-keys <pane> enter` when submitting.

Pass the transcript on stdin, never argv: dictated text contains quotes.

## Latching and disconnects

The target is latched at record start and never recomputed, as the local pane is today - `(target, pane)` rather than
`pane`. The resolve round trips overlap with speech, so they cost nothing perceptible.

The remote server owns the panes and their agents, which survive the client disconnecting and the SSH dropping; pane
ids stay valid because that server does not restart. If it does, ids are not reused, so a stale target fails visibly
rather than delivering into an unrelated pane. If the connection dies between latch and delivery, reconnect and
deliver to the latched target.

## SSH options

Herdr's own, rather than a new set:

```
-T -o BatchMode=yes -o NumberOfPasswordPrompts=0 -o StrictHostKeyChecking=yes
-o ConnectTimeout=10 -o ConnectionAttempts=1 -o ServerAliveInterval=15 -o ServerAliveCountMax=4
-S <control-path> -o ControlMaster=auto -o ControlPersist=yes
```

Non-interactive key auth is a precondition; a passphrased key needs an agent.

## Doctor

- A selected machine resolves, and its target parses as an SSH destination.
- `ssh -o BatchMode=yes <target> true` succeeds without a prompt.
- The remote Herdr binary resolves to an absolute path. It commonly lives under the user's home, which a
  non-interactive shell does not have on PATH.
- Client and remote report compatible protocol generations.

## Non-goals

Standalone `herdr --remote`, where plugin actions come from the remote server, which has no copy of this plugin.
Saved machines are the supported topology.

Streaming partial transcripts. Delivery is one message per utterance; if that changes, the transport is one seam.

## Open questions

- Whether `selected` is current at keypress. It is server-side catalog state, but it is load-bearing and wants a test.
- Focus is per-server: with two clients on one remote session, the last to focus wins. Latching by agent name
  sidesteps this for agent panes.
- No machine-selection event exists, so the channel cannot be warmed when the user switches machines.
