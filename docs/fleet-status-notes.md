# Fleet status and spend bar

Status: items 1 and 2 built, item 3 is a design note only. Source of the ideas: the herdr competitor note (ideas 1, 3 and 7).

## 1. Fleet status (built)

One glanceable state per agent, derived from data the core already sends. Nothing is stored.

**Derivation** (`apps/desktop/lib/state/fleet.dart`, `agentStatus`). First match wins:

| Order | State | From |
|---|---|---|
| 1 | offline | `Store.link != online` (every live count is stale, so this wins) |
| 2 | needs you | `waiting > 0` (cards and outbox in the agent's DM, or Home's pending approvals and secrets for the default agent) or `agent.needsYou > 0` |
| 3 | working | the store's live run tracking, or `agent.activeRuns > 0` |
| 4 | over budget | any routine of the agent with `paused == 'budget'` |
| 5 | archived | `agent.archived` |
| 6 | done recently | `lastActivity` within 15 minutes (`doneRecentlyFor`) |
| 7 | idle | otherwise |

Difference from the agent-profile header sketch (`agent-profile-ui.md` section 2): offline is first, not fourth, because needs-you and running counts cannot be trusted without a link. The header can call `agentStatus` and map the enum to its pill (labels are on `AgentStatus.label`). "Done recently" is new; the pill may show it as IDLE with the last-active line.

**Desktop.** `StatusMark` (`ui/fleet_status.dart`) is a monotone mark whose shape differs per state, so it never relies on colour: diamond needs you, filled circle working (pulses; held still when the system asks for reduced motion), square over budget, dash offline or archived, ring done recently or idle. It carries a tooltip and a semantics label. It sits on each AGENTS row (next to or instead of the count badge and unread dot) and on the Home row. `FleetStrip` ("1 needs you, 2 working") sits under Home in the sidebar and under the header on Home; a click opens the first agent that needs you. Hidden when quiet or offline.

**TUI.** `/agents` marks each agent with the same shapes (diamond, filled circle, ring). The status line adds "2 working" (and "1 needs you" when Home's own count is zero) when there is more than one agent. The TUI only has needs-you and active-run counts, so it shows three states, not seven.

**Not done.** Tray dot and Dock badge (the tray dot already exists and could read `fleet.needsYou`). Per-channel rollups.

## 2. Usage and spend bar (built, with one core addition)

What existed: per-session token counts and a context meter, per-routine caps (`usd_per_day`, `runs_per_day`) that pause a routine, and the scheduler's private `spend_since`. There is no `[budgets]` config table; the only USD defaults are under `[schedules]`. No spend figure reached any client.

Core addition (minimal, tested in `t1a.rs`): the schedule view gets `spent_usd` and `usd_cap` (today's spend and the effective cap, the routine's own or the `[schedules]` default). Both are absent without a cap or a known model price, so a client shows nothing rather than a guess.

Clients sum the routines that have both numbers. Desktop: `SpendBar` above the connection footer, "Routines today $0.31 of $0.50 cap" with a hairline bar (accent, warn from 80%, error at 100%). TUI: the right side of the status line, "$0.31 of $0.50 today" (daemon-backed TUI only; the in-process backend has no schedules). Hidden when there is no data.

Limits: this is routine spend only, not interactive chat spend; there is no provider quota ("remaining on the plan") and no per-agent figure. An all-spend figure needs the core to price every run, which the ledger could do later.

## 3. Notification attention budget (design note, not built)

Idea (herdr, "interrupt only when needed"): one setting that says how much the user wants to be pulled in, instead of tuning initiative, quiet hours and digests per routine.

- **Setting.** `[notifications] attention = "blocked" | "balanced" | "everything"`, default balanced. Desktop: Settings, Notifications, one three-way control.
- **blocked.** Interrupt (Telegram push, desktop notification, tray dot) only for needs-you: approvals, secret requests, asks. Everything else goes to the digest and the in-app unread dots.
- **balanced.** Today's behaviour: each routine's `initiative` decides.
- **everything.** Raise `digest` and `notify` routines to interrupt.
- **Implementation sketch.** The dial caps or raises the effective initiative at delivery time (the same place quiet hours are applied in the scheduler and the gateway), so no routine data changes. Urgent routines still break quiet hours. Over-budget pauses count as needs-you because the user must act.
- **Optional second part.** A rate limit ("at most N interrupts per hour, then batch"), reusing held deliveries.
- **Open questions.** Is it global only, or per agent? Should "blocked" still ping for failed runs? How does it interact with `urgent`? Needs a decision before building.
- **Effort.** S for the three-way dial; M with the rate limit. Tests: delivery decision table per mode, quiet-hours and urgent interplay.
