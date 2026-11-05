import 'package:tern_client/tern_client.dart';

import 'store.dart';

/// What an agent is doing right now, in the one vocabulary the sidebar,
/// Home, the agent page header and the TUI share. Derived from data the
/// core already sends; nothing here is stored.
enum AgentStatus {
  /// The app can't reach the core, so every other number is stale.
  offline,

  /// Approvals or secret requests wait on the user.
  needsYou,

  /// A run is in progress.
  working,

  /// One of its routines is paused by a daily cap.
  overBudget,

  /// Archived: its routines are paused and its DM is hidden.
  archived,

  /// Idle, but it was active a moment ago.
  doneRecently,
  idle;

  /// Short words for tooltips, pills and the summary strip.
  String get label => switch (this) {
    offline => 'offline',
    needsYou => 'needs you',
    working => 'working',
    overBudget => 'over budget',
    archived => 'archived',
    doneRecently => 'done recently',
    idle => 'idle',
  };
}

/// How long after its last activity an idle agent counts as "done
/// recently".
const doneRecentlyFor = Duration(minutes: 15);

/// The status of one agent; the first match wins, in the order of
/// [AgentStatus]:
///
/// 1. [linkOnline] false: offline (live counts can't be trusted).
/// 2. [waiting] > 0 (or the agent's own `needsYou`): needs you.
/// 3. [busy] or `activeRuns > 0`: working.
/// 4. any of [schedules] paused by a budget: over budget.
/// 5. archived.
/// 6. `lastActivity` within [doneRecentlyFor] of [now]: done recently.
/// 7. otherwise idle.
///
/// The agent page header (docs/agent-profile-ui.md section 2) reads this
/// too.
AgentStatus agentStatus(
  Agent agent, {
  required bool linkOnline,
  required DateTime now,
  int waiting = 0,
  bool busy = false,
  Iterable<Schedule> schedules = const [],
}) {
  if (!linkOnline) return AgentStatus.offline;
  if (waiting > 0 || agent.needsYou > 0) return AgentStatus.needsYou;
  if (busy || agent.activeRuns > 0) return AgentStatus.working;
  if (schedules.any((s) => s.pausedByBudget)) return AgentStatus.overBudget;
  if (agent.archived) return AgentStatus.archived;
  final last = agent.lastActivity;
  if (last != null && now.difference(last) <= doneRecentlyFor) {
    return AgentStatus.doneRecently;
  }
  return AgentStatus.idle;
}

/// Counts over the fleet, and the first agent that needs the user.
class FleetSummary {
  const FleetSummary(this.entries);

  final List<(Agent, AgentStatus)> entries;

  int count(AgentStatus s) => entries.where((e) => e.$2 == s).length;
  int get working => count(AgentStatus.working);
  int get needsYou => count(AgentStatus.needsYou);
  int get overBudget => count(AgentStatus.overBudget);

  /// What a click on the strip opens.
  Agent? get firstNeedingYou =>
      entries.where((e) => e.$2 == AgentStatus.needsYou).firstOrNull?.$1;

  /// "1 needs you, 2 working"; empty when nothing is worth a line.
  String get text {
    final parts = [
      if (needsYou > 0) '$needsYou ${needsYou == 1 ? 'needs' : 'need'} you',
      if (working > 0) '$working working',
      if (overBudget > 0) '$overBudget over budget',
    ];
    return parts.join(', ');
  }
}

/// Today's routine spend against the daily caps: the sum over routines that
/// have both numbers. Null when none does (no cap, or no known price).
({double spent, double cap})? spendToday(Iterable<Schedule> schedules) {
  var spent = 0.0;
  var cap = 0.0;
  var any = false;
  for (final s in schedules) {
    final u = s.spentUsd;
    final c = s.usdCap;
    if (u == null || c == null || c <= 0) continue;
    spent += u;
    cap += c;
    any = true;
  }
  return any ? (spent: spent, cap: cap) : null;
}

extension FleetStatusOf on TernStore {
  AgentStatus statusOf(Agent a, {DateTime? now}) => agentStatus(
    a,
    linkOnline: link == Link.online,
    now: now ?? DateTime.now(),
    waiting: a.isDefault ? pendingApprovals + pendingSecrets : waitingFor(a),
    busy: agentBusy(a),
    schedules: schedulesOf(a),
  );

  /// Every agent that isn't archived.
  FleetSummary get fleet => FleetSummary([
    for (final a in agents)
      if (!a.archived) (a, statusOf(a)),
  ]);
}
