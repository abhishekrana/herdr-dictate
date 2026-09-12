# Remote delivery

Dictating into a pane on a saved SSH machine. The microphone, model and GPU stay on the client; only the transcript
crosses.

## Problem

A dictation resolves the focused pane from local context and writes it through the local socket. When a saved machine
is selected, the key is still handled by the client, which runs the local plugin against the local server, so the
words land locally. Selecting a machine retargets nothing: the local server's focused pane is unchanged by it.

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
Warm, a delivery costs about 61 ms and a latch about 79 ms; cold, about 512 ms each. All are invisible against
seconds of speech and transcription. Only a cold connect on the keypress path is perceptible, so the channel is
warmed from a startup hook and held with `ControlPersist`, and its loss degrades latency rather than failing the
dictation.

## Resolving

Per dictation, nothing cached:

1. `herdr machine list --json` on the client. The entry with `selected` true gives the SSH target and the profile's
   session name. This tracks the sidebar live, so it is the only source of truth for which machine is meant.
2. One invocation on that machine returning everything the latch needs: the session's socket path, the snapshot's
   `focused_pane_id`, and the agent list.

The remote half must be a single invocation. Each SSH exec channel costs a round trip or three, so the same three
queries issued separately measured 1550 ms cold against 76 ms batched and warm.

Two details the remote half depends on:

- **Sessions have separate sockets.** A profile may name a session other than the default, and a bare CLI call hits
  the default one. Read the session's `socket_path` from `herdr session list --json` and pass it as
  `HERDR_SOCKET_PATH` to every subsequent call.
- **The remote binary needs an absolute path.** It commonly lives under the user's home, which a non-interactive
  shell does not have on PATH, so a bare `herdr` fails. Resolve the path once at setup and store it per machine.

With no machine selected, this collapses to the existing local path.

## Delivering

An agent pane takes `herdr agent prompt <name>`: it submits text and Enter as one operation honouring bracketed
paste, and refuses with `agent_blocked` at an approval dialog, so a transcript is never typed into a prompt as stray
keys. Prefer the agent name over the pane id - a name follows the pane's occupant and is cleared when that agent
exits, so a stale target fails loudly.

Anything else takes `herdr pane send-text`, then `herdr pane send-keys <pane> enter` when submitting.

**Re-verify the occupant before submitting.** An agent that exits leaves its pane alive at a shell prompt. Submitting
a transcript into that pane would execute it as a command. If the occupant changed between latch and delivery,
deliver the text without Enter, or refuse.

Pass the transcript on stdin, never argv: dictated text contains quotes.

## Indicator

The recording indicator is a pane label with a TTL, refreshed for the duration of the recording, so a remote target
means a round trip every few seconds while the user speaks. Set it on the remote pane, best effort. Failures are
logged at debug and swallowed, as for any other nicety; recording never blocks on it.

## Failure handling

Never fall back to local delivery. A selected machine that is disabled or unreachable fails the latch visibly.
Silently delivering locally would reproduce the bug this design exists to fix - words landing on the wrong machine.

## Latching and disconnects

The target is latched at record start and never recomputed, as the local pane is today - `(target, pane)` rather than
`pane`. The resolve round trips overlap with speech, so they cost nothing perceptible.

`focused_pane_id` is last known rather than live: a server with no client attached still reports one. That is safe
because it is only consulted while that machine is selected, and worth stating so it is not trusted elsewhere.

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
- The remote Herdr binary resolves to an absolute path.
- The profile's session exists on the remote and its socket path resolves.
- Client and remote report compatible protocol generations.

## Non-goals

Standalone `herdr --remote`, where plugin actions come from the remote server, which has no copy of this plugin.
Saved machines are the supported topology.

Streaming partial transcripts. Delivery is one message per utterance; if that changes, the transport is one seam.

## Accepted trade-off

Focus is per-server, not per-client. With two clients attached to one remote session, the last to focus wins, and
`focused_pane_id` may name a pane another client is driving. Targeting agent panes by name sidesteps this; plain
shells have no equivalent, and last-focus-wins is accepted for them.
