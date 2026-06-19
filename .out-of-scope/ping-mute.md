# Ping Mute / Snooze

The ping system does not support a per-user "mute" or "snooze" state that keeps
someone in a ping group while excluding them from the actual notification.

## Why this is out of scope

The need this asks for — "I don't want to be pinged right now, but I don't want
to leave the command" — is already served by the existing join/leave commands.
Leaving a ping and rejoining later is the supported workflow, and it costs the
user one message.

A mute state would add real complexity for marginal benefit:

- **A second membership state.** Today a ping's membership is a flat set
  (`Ping.members: HashSet<String>`). "Member but muted" splits that into two
  states, and every consumer of membership has to decide which it means —
  `{mentions}` resolution must skip muted users, while the member list/count
  shown to users becomes ambiguous (do muted members count?).
- **Timer machinery, if timed.** A *snooze* variant ("mute for 30 minutes")
  needs per-member expiry timestamps and a sweep to auto-restore — a whole
  scheduling concern bolted onto a data model that is currently just a set.
- **No agreed shape.** The request never resolved into a concrete design:
  persistent toggle vs. timed snooze, per-ping vs. global, command syntax, and
  whether muted users still count toward the group — all left open. Leaving and
  rejoining sidesteps every one of those questions.

The ping system's value is its simplicity: you're in a group or you're not. A
mute state trades that clarity for a workflow the existing leave/rejoin commands
already cover.

## Prior requests

- #145 — "Add mute mode for command participation without pings" (requested by `lesh`)
