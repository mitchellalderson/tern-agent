import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/cron_words.dart';
import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'widgets.dart';

/// The routines section of the side panel. With agent profiles it shows
/// the open agent's jobs (agent profiles §4); in a topic channel, the
/// routines that post there. **All** shows every routine, so what runs at
/// night stays visible in one place.
class RoutinesSection extends StatefulWidget {
  const RoutinesSection({super.key, required this.store});

  final TernStore store;

  @override
  State<RoutinesSection> createState() => _RoutinesSectionState();
}

class _RoutinesSectionState extends State<RoutinesSection> {
  var _all = false;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final store = widget.store;
    final agent = store.hasAgents
        ? store.streamAgent ?? (store.topicOpen ? null : store.defaultAgent)
        : null;
    final topic = store.topicOpen ? store.openChannel : null;
    final scoped = agent != null || topic != null;
    final shown = !scoped || _all
        ? store.schedules
        : agent != null
        ? store.schedulesOf(agent)
        : store.schedulesIn(topic!.id);
    final scope = agent != null
        ? agent.name.toUpperCase()
        : topic != null
        ? '#${topic.slug.toUpperCase()}'
        : null;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                scope == null || _all ? 'ROUTINES' : 'ROUTINES · $scope',
                style: Theme.of(context).textTheme.labelSmall,
              ),
            ),
            if (scoped &&
                (topic != null || store.otherAgents.isNotEmpty) &&
                store.schedules.isNotEmpty)
              SizedBox(
                height: 22,
                child: TextButton(
                  onPressed: () => setState(() => _all = !_all),
                  style: TextButton.styleFrom(
                    padding: const EdgeInsets.symmetric(horizontal: 6),
                    minimumSize: Size.zero,
                  ),
                  child: Text(
                    _all ? (agent?.name ?? '#${topic!.slug}') : 'All',
                    style: TextStyle(fontSize: 11.5, color: c.muted),
                  ),
                ),
              ),
          ],
        ),
        const SizedBox(height: 6),
        for (final s in shown)
          RoutineRow(
            store: store,
            schedule: s,
            showAgent: _all || agent == null,
          ),
        if (shown.isEmpty && scoped && !_all)
          Text(
            agent != null
                ? 'Ask ${agent.name} to do something on a schedule, or add one.'
                : 'No routines post here yet. Ask for one, or add one.',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            // In a topic, the sheet files a new routine there.
            onPressed: () => showRoutineSheet(context, store, agent: agent),
            icon: Icon(Icons.add, size: 16, color: c.muted),
            label: Text('New routine', style: TextStyle(color: c.muted)),
            style: TextButton.styleFrom(
              padding: const EdgeInsets.symmetric(horizontal: 6),
            ),
          ),
        ),
      ],
    );
  }
}

/// Where a routine's results are posted, in words: `#topic`, `Home`, or an
/// agent's DM.
String routinePostsTo(TernStore store, Schedule s) {
  final ch = store.channels.where((c) => c.id == s.channelId).firstOrNull;
  if (ch == null || s.channelId == TernStore.homeId) return 'Home';
  if (!ch.isMain) return '#${ch.slug}';
  return '${store.agentOf(ch.id)?.name ?? ch.title} DM';
}

/// The agent a routine runs as, by name.
String routineAgent(TernStore store, Schedule s) =>
    store.agents.where((a) => a.id == s.agentId).firstOrNull?.name ??
    (s.agentId == Agent.defaultId ? 'tern' : s.agentId);

/// One routine: what it does, when, whose and where it posts, with a menu
/// to run, edit, pause or delete it.
class RoutineRow extends StatelessWidget {
  const RoutineRow({
    super.key,
    required this.store,
    required this.schedule,
    this.showAgent = false,
    this.wide = false,
  });

  final TernStore store;
  final Schedule schedule;

  /// Name the agent whose job it is (the All list).
  final bool showAgent;

  /// Room for the next run in the line itself (the Routines page).
  final bool wide;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = schedule;
    final title = s.prompt.split('\n').first;
    final failed = s.lastStatus == 'failed';
    final budget = s.enabled && s.pausedByBudget;
    final active = s.enabled && !budget;
    // The panel is narrow: the schedule in words here, the next run on hover.
    final when = routineWhen(s);
    final detail = !s.enabled
        ? 'Paused'
        : budget
        ? 'Paused: budget · resumes ${_resumes(s.pausedUntil)}'
        : failed
        ? 'Last run failed · $when'
        : s.lastQuiet
        ? 'Last run quiet · $when'
        : wide && s.nextRunAt != null && s.trigger != 'watch'
        ? '$when · next ${whenInWords(s.nextRunAt!)}'
        : when;
    // Topic channels always; Home and DMs in lists that mix agents.
    final postsTo = routinePostsTo(store, s);
    final showPostsTo =
        postsTo.startsWith('#') || (showAgent && store.hasChannels);
    final owner = showAgent && store.otherAgents.isNotEmpty
        ? store.agents.where((a) => a.id == s.agentId).firstOrNull
        : null;
    final next = s.nextRunAt;
    final tip = budget
        ? 'A daily cap (${s.pausedReason ?? 'budget'}) was reached. '
              'Resume it now from the menu, or it continues on its own.'
        : s.lastQuiet && s.enabled
        ? 'The last run had nothing to report, so nothing was sent.'
        : s.trigger == 'watch'
        ? [
            'Polls ${s.watch} every ${_every(s.everySecs ?? 300)}; each new '
                'event starts a run, and nothing found costs nothing.',
            if (s.lastCheck case final c?) 'Last check: $c.',
          ].join(' ')
        : s.trigger == 'webhook'
        ? [
            'Each delivery to its webhook starts a run, read as untrusted.',
            if (s.lastCheck case final c?) 'Last: $c.',
          ].join(' ')
        : next == null
        ? when
        : [
            'Next run ${whenInWords(next)} · $when',
            if (s.lastCheck case final c?) 'Last check: $c.',
          ].join('\n');
    return InkWell(
      borderRadius: BorderRadius.circular(8),
      // Its own conversation: beside Home or a channel, with channels.
      onTap: () => store.hasChannels
          ? store.openThread(s.sessionId)
          : store.open(s.sessionId),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 6),
        child: Row(
          children: [
            Icon(
              budget
                  ? Icons.pause_circle_outline
                  : s.trigger == 'watch'
                  ? Icons.sensors
                  : s.trigger == 'webhook'
                  ? Icons.webhook
                  : Icons.schedule,
              size: 16,
              color: active ? c.accent : c.muted,
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 13.5,
                      color: active ? c.text : c.muted,
                    ),
                  ),
                  Tooltip(
                    message: tip,
                    child: Text(
                      [
                        ?owner?.name,
                        detail,
                        if (showPostsTo) '→ $postsTo',
                      ].join(' · '),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 11.5,
                        color: failed && active ? c.error : c.muted,
                        fontStyle: s.lastQuiet && active && !failed
                            ? FontStyle.italic
                            : null,
                      ),
                    ),
                  ),
                ],
              ),
            ),
            PopupMenuButton<String>(
              tooltip: 'Routine actions',
              icon: Icon(Icons.more_horiz, size: 16, color: c.muted),
              padding: EdgeInsets.zero,
              splashRadius: 14,
              onSelected: (a) => switch (a) {
                'run' => store.runSchedule(s),
                'edit' => showRoutineSheet(context, store, editing: s),
                'pause' => store.setScheduleEnabled(s, !active),
                'delete' => _confirmDelete(context),
                _ => null,
              },
              itemBuilder: (_) => [
                const PopupMenuItem(value: 'run', child: Text('Run now')),
                const PopupMenuItem(value: 'edit', child: Text('Edit…')),
                PopupMenuItem(
                  value: 'pause',
                  child: Text(active ? 'Pause' : 'Resume'),
                ),
                const PopupMenuDivider(),
                const PopupMenuItem(value: 'delete', child: Text('Delete…')),
              ],
            ),
          ],
        ),
      ),
    );
  }

  /// "tomorrow" for a pause ending at the next midnight.
  String _resumes(DateTime? until) {
    if (until == null) return 'tomorrow';
    final now = DateTime.now();
    final tomorrow = DateTime(now.year, now.month, now.day + 1);
    return until.isAfter(tomorrow.add(const Duration(minutes: 1)))
        ? whenInWords(until)
        : 'tomorrow';
  }

  Future<void> _confirmDelete(BuildContext context) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Delete this routine?'),
        content: Text(
          '“${schedule.prompt.split('\n').first}” will stop running. '
          'Its past runs stay in the conversation.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (ok ?? false) await store.deleteSchedule(schedule);
  }
}

/// Open the routine a `tern://routine/<id>` link names: its editor, after
/// re-reading the list if it's newer than ours. False if [link] isn't one.
bool openTernLink(BuildContext context, TernStore store, String? link) {
  final id = link == null ? null : routineIdOf(link);
  if (id == null) return false;
  unawaited(openRoutine(context, store, id));
  return true;
}

Future<void> openRoutine(
  BuildContext context,
  TernStore store,
  String id,
) async {
  var s = store.schedules.where((x) => x.id == id).firstOrNull;
  if (s == null) {
    await store.refreshSchedules();
    s = store.schedules.where((x) => x.id == id).firstOrNull;
  }
  if (!context.mounted) return;
  if (s == null) {
    ScaffoldMessenger.maybeOf(
      context,
    )?.showSnackBar(const SnackBar(content: Text('That routine was deleted.')));
    return;
  }
  await showRoutineSheet(context, store, editing: s);
}

/// A routine an agent made or changed in this conversation: what it does,
/// when, who runs it and where results go, and a way to open it.
class RoutineCard extends StatelessWidget {
  const RoutineCard({super.key, required this.item, required this.store});

  final RoutineItem item;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = store.schedules.where((x) => x.id == item.scheduleId).firstOrNull;
    final next = s?.nextRunAt;
    final lines = s == null
        ? ['This routine was deleted.']
        : [
            [
              routineWhen(s),
              if (s.enabled && next != null && s.trigger != 'watch')
                'next ${whenInWords(next)}',
              if (!s.enabled) 'paused',
            ].join(' · '),
            'Runs as ${routineAgent(store, s)} · posts to ${routinePostsTo(store, s)}',
          ];
    return Align(
      alignment: Alignment.centerLeft,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: Container(
          key: ValueKey('routine-card-${item.scheduleId}'),
          margin: const EdgeInsets.symmetric(vertical: 4),
          padding: const EdgeInsets.fromLTRB(12, 10, 8, 10),
          decoration: BoxDecoration(
            color: c.card,
            border: Border.all(color: c.hairline),
            borderRadius: BorderRadius.circular(10),
          ),
          child: Row(
            children: [
              Icon(
                Icons.schedule,
                size: 18,
                color: s?.enabled ?? false ? c.accent : c.muted,
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      [
                        switch (item.created) {
                          true => 'New routine',
                          false => 'Routine changed',
                          null => 'Routine',
                        },
                        if (s != null) s.prompt.split('\n').first,
                      ].join(': '),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 13.5,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    for (final l in lines)
                      Text(
                        l,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(fontSize: 12, color: c.muted),
                      ),
                  ],
                ),
              ),
              if (s != null)
                TextButton(
                  onPressed: () => openRoutine(context, store, s.id),
                  child: const Text('Open'),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Every routine, by every agent, wherever it posts: the Library's
/// Routines page.
class RoutinesPage extends StatelessWidget {
  const RoutinesPage({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    // Soonest first; paused and event-driven ones after.
    final list = [...store.schedules]
      ..sort((a, b) {
        final x = a.enabled ? a.nextRunAt : null;
        final y = b.enabled ? b.nextRunAt : null;
        if (x == null || y == null) return x == null ? (y == null ? 0 : 1) : -1;
        return x.compareTo(y);
      });
    return Column(
      children: [
        WindowDragArea(
          child: Container(
            height: titleBarInset + 12,
            padding: const EdgeInsets.fromLTRB(20, 0, 10, 0),
            child: Row(
              children: [
                const Expanded(
                  child: Text(
                    'Routines',
                    style: TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
                  ),
                ),
                FilledButton.icon(
                  onPressed: () => showRoutineSheet(context, store),
                  icon: const Icon(Icons.add, size: 16),
                  label: const Text('New routine'),
                ),
                const SizedBox(width: 6),
                QuietIconButton(
                  icon: panelOpen
                      ? Icons.view_sidebar
                      : Icons.view_sidebar_outlined,
                  tooltip: panelOpen ? 'Hide activity' : 'Show activity',
                  onPressed: onTogglePanel,
                ),
              ],
            ),
          ),
        ),
        const Hairline(),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.fromLTRB(28, 18, 28, 24),
            children: [
              Text(
                'Everything that runs on its own: who runs it, when, and '
                'where its results go.',
                style: TextStyle(color: c.muted),
              ),
              const SizedBox(height: 12),
              if (list.isEmpty)
                Text(
                  'No routines yet. Ask an agent to do something on a '
                  'schedule, or add one.',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              for (final s in list)
                RoutineRow(
                  store: store,
                  schedule: s,
                  showAgent: true,
                  wide: true,
                ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A topic channel's routines, from its header.
Future<void> showChannelRoutines(
  BuildContext context,
  TernStore store,
  Channel channel,
) => showDialog<void>(
  context: context,
  builder: (context) => ListenableBuilder(
    listenable: store,
    builder: (context, _) {
      final list = store.schedulesIn(channel.id);
      return AlertDialog(
        title: Text('Routines in #${channel.slug}'),
        content: SizedBox(
          width: 420,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              if (list.isEmpty)
                Text(
                  'No routines post here yet.',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              for (final s in list)
                RoutineRow(store: store, schedule: s, showAgent: true),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => showRoutineSheet(context, store),
            child: const Text('New routine'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Close'),
          ),
        ],
      );
    },
  ),
);

Future<void> showRoutineSheet(
  BuildContext context,
  TernStore store, {
  Schedule? editing,
  Agent? agent,
}) => showDialog<void>(
  context: context,
  builder: (_) => RoutineSheet(store: store, editing: editing, agent: agent),
);

/// A routine's trigger in words: its cron, "Once, …", or "On … events".
String routineWhen(Schedule s) => switch (s.trigger) {
  'once' => s.at == null ? 'Once' : 'Once, ${whenInWords(s.at!)}',
  'watch' => 'On ${s.watch} events',
  'webhook' => 'On webhook deliveries',
  _ => cronInWords(s.cron),
};

String _every(int secs) => secs % 3600 == 0
    ? '${secs ~/ 3600}h'
    : secs % 60 == 0
    ? '${secs ~/ 60}m'
    : '${secs}s';

/// The initiative levels, as the editor offers them (T1b).
const _initiatives = {
  'silent': 'Silent: post only, never send',
  'digest': 'Digest: send with the next digest',
  'notify': 'Notify: send now if it matters, else digest',
  'interrupt': 'Interrupt: send at once',
};

class _Preset {
  const _Preset(this.label, this.cron);
  final String label;
  final String cron;
}

const _presets = [
  _Preset('Every morning', '0 8 * * *'),
  _Preset('Weekdays', '0 9 * * 1-5'),
  _Preset('Mondays', '0 9 * * 1'),
  _Preset('Hourly', '0 * * * *'),
];

/// Create or edit a routine: what to do, when, and where results go.
class RoutineSheet extends StatefulWidget {
  const RoutineSheet({
    super.key,
    required this.store,
    this.editing,
    this.agent,
  });

  final TernStore store;
  final Schedule? editing;

  /// Whose job a new routine is (the open agent's).
  final Agent? agent;

  @override
  State<RoutineSheet> createState() => _RoutineSheetState();
}

class _RoutineSheetState extends State<RoutineSheet> {
  late final _prompt = TextEditingController(text: widget.editing?.prompt);
  late final _cron = TextEditingController(
    text: widget.editing?.cron ?? _presets.first.cron,
  );

  /// `<gateway>:<chat>`: Telegram or a gateway connector (ntfy, Slack).
  late final _chat = TextEditingController(
    text: switch (widget.editing?.deliverTo?.indexOf(':')) {
      final i? when i > 0 => widget.editing!.deliverTo!.substring(i + 1),
      _ => null,
    },
  );
  late var _push = widget.editing?.deliverTo != null;
  late var _gateway = switch (widget.editing?.deliverTo?.indexOf(':')) {
    final i? when i > 0 => widget.editing!.deliverTo!.substring(0, i),
    _ => _gateways.first.name,
  };

  /// The daemon's gateways, Telegram alone for an older daemon. A routine
  /// that delivers somewhere no longer configured keeps showing it.
  List<({String name, String kind})> get _gateways {
    final known = widget.store.info?.gateways ?? const [];
    final list = known.isEmpty
        ? [(name: 'telegram', kind: 'telegram')]
        : [...known];
    final current = widget.editing?.deliverTo?.split(':').first;
    if (current != null &&
        current.isNotEmpty &&
        !list.any((g) => g.name == current)) {
      list.add((name: current, kind: current));
    }
    return list;
  }

  String get _kind =>
      _gateways.where((g) => g.name == _gateway).firstOrNull?.kind ?? _gateway;

  static String _title(String kind) => switch (kind) {
    'telegram' => 'Telegram',
    'ntfy' => 'ntfy',
    'slack' => 'Slack',
    final k => k,
  };

  String get _chatHint => switch (_kind) {
    'telegram' => 'Telegram chat id',
    'ntfy' => 'ntfy topic',
    'slack' => 'Slack channel id (C0123…)',
    final k => '${_title(k)} chat',
  };

  bool get _chatValid {
    final chat = _chat.text.trim();
    if (_kind == 'telegram') return int.tryParse(chat) != null;
    return chat.isNotEmpty && !chat.contains(RegExp(r'\s'));
  }

  /// `cron`, `once`, `watch` or `webhook`.
  late var _trigger = widget.editing?.trigger ?? 'cron';

  /// A new webhook routine's auth: `hmac` or `bearer`.
  var _auth = 'hmac';

  /// The hook of the webhook routine being edited, once loaded.
  Webhook? _hook;
  String? _hookError;
  late final _at = TextEditingController(text: _atBefore);

  /// The reminder's time as it was, `YYYY-MM-DD HH:MM`.
  String get _atBefore => switch (widget.editing?.at) {
    final t? =>
      '${t.year}-${_two(t.month)}-${_two(t.day)} ${_two(t.hour)}:${_two(t.minute)}',
    null => '',
  };
  late final _watch = TextEditingController(text: widget.editing?.watch);
  late final _every = TextEditingController(
    text: switch (widget.editing?.everySecs) {
      final s? => '${s ~/ 60}m',
      null => '',
    },
  );

  /// Null = the daemon's default (digest).
  late String? _initiative = widget.editing?.initiative;
  late final _gate = TextEditingController(text: widget.editing?.gate);
  late final _gateConnector = TextEditingController(
    text: widget.editing?.gateConnector,
  );

  static String _two(int n) => n.toString().padLeft(2, '0');
  static final _atPattern = RegExp(r'^\d{4}-\d{2}-\d{2}[ T]\d{1,2}:\d{2}$');
  static final _watchPattern = RegExp(r'^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$');

  /// Who does it: an agent profile (the default agent without profiles).
  late var _agent =
      widget.editing?.agentId ?? widget.agent?.id ?? Agent.defaultId;

  /// Where results are posted: its agent's DM (Home for the default
  /// agent), or a topic channel (the one open when it is made there).
  late var _channel =
      widget.editing?.channelId ??
      (widget.store.topicOpen ? widget.store.channel! : _dmOf(_agent));

  String _dmOf(String agent) =>
      widget.store.agents
          .where((a) => a.id == agent)
          .firstOrNull
          ?.mainChannelId ??
      TernStore.homeId;
  late final _limits = widget.editing?.limits ?? const ScheduleLimits();

  /// `null` = the daemon's default, `none`, or a custom window.
  late String? _quietMode = switch (_limits.quietHours) {
    null => null,
    'none' => 'none',
    _ => 'custom',
  };
  late final _quiet = TextEditingController(
    text: _quietMode == 'custom' ? _limits.quietHours : '22:00-07:30',
  );
  late var _urgent = _limits.urgent;
  late final _runs = TextEditingController(
    text: _limits.runsPerDay?.toString(),
  );
  late final _usd = TextEditingController(text: _limits.usdPerDay?.toString());
  late final _tokens = TextEditingController(
    text: _limits.tokensPerRun?.toString(),
  );
  late var _showLimits =
      _limits.quietHours != null ||
      _limits.urgent ||
      _limits.runsPerDay != null ||
      _limits.usdPerDay != null ||
      _limits.tokensPerRun != null;
  CronPreview? _preview;
  String? _cronError;
  String? _error;
  var _busy = false;
  Timer? _debounce;

  @override
  void initState() {
    super.initState();
    _cron.addListener(_schedulePreview);
    _prompt.addListener(() => setState(() {}));
    for (final c in [_at, _watch, _every]) {
      c.addListener(() => setState(() {}));
    }
    _checkCron();
    if (widget.editing?.trigger == 'webhook') _loadHook();
  }

  Future<void> _loadHook() async {
    try {
      final all = await widget.store.client.webhooks();
      if (!mounted) return;
      setState(
        () => _hook = all
            .where((w) => w.routineId == widget.editing!.id)
            .firstOrNull,
      );
    } on TernException catch (e) {
      if (mounted) setState(() => _hookError = e.message);
    }
  }

  Future<void> _rotate() async {
    try {
      final w = await widget.store.client.rotateWebhook(widget.editing!.id);
      if (!mounted) return;
      setState(() => _hook = w);
      await showWebhookSecret(context, w, created: false);
    } on TernException catch (e) {
      if (mounted) setState(() => _hookError = e.message);
    }
  }

  Future<void> _revoke() async {
    try {
      await widget.store.client.revokeWebhook(widget.editing!.id);
      await _loadHook();
    } on TernException catch (e) {
      if (mounted) setState(() => _hookError = e.message);
    }
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _prompt.dispose();
    _cron.dispose();
    _chat.dispose();
    for (final c in [
      _quiet,
      _runs,
      _usd,
      _tokens,
      _at,
      _watch,
      _every,
      _gate,
      _gateConnector,
    ]) {
      c.dispose();
    }
    super.dispose();
  }

  void _schedulePreview() {
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 250), _checkCron);
  }

  Future<void> _checkCron() async {
    final text = _cron.text.trim();
    try {
      final p = await widget.store.client.previewCron(text);
      if (!mounted || _cron.text.trim() != text) return;
      setState(() {
        _preview = p;
        _cronError = null;
      });
    } on TernException catch (e) {
      if (!mounted) return;
      setState(() {
        _preview = null;
        _cronError = e.message;
      });
    }
  }

  static final _window = RegExp(r'^\d{1,2}:\d{2}\s*[-–]\s*\d{1,2}:\d{2}$');

  /// Empty is the default; otherwise a positive number.
  static bool _cap(String text, {bool decimal = false}) {
    final t = text.trim();
    if (t.isEmpty) return true;
    final n = decimal ? double.tryParse(t) : int.tryParse(t);
    return n != null && n > 0;
  }

  bool get _limitsValid =>
      (_quietMode != 'custom' || _window.hasMatch(_quiet.text.trim())) &&
      _cap(_runs.text) &&
      _cap(_usd.text, decimal: true) &&
      _cap(_tokens.text);

  ScheduleLimits get _newLimits {
    int? n(TextEditingController c) => int.tryParse(c.text.trim());
    return ScheduleLimits(
      quietHours: switch (_quietMode) {
        'custom' => _quiet.text.trim().replaceAll(' ', ''),
        final m => m,
      },
      urgent: _urgent,
      runsPerDay: n(_runs),
      usdPerDay: double.tryParse(_usd.text.trim()),
      tokensPerRun: n(_tokens),
    );
  }

  bool get _triggerValid => switch (_trigger) {
    'once' => _atPattern.hasMatch(_at.text.trim()),
    'watch' => _watchPattern.hasMatch(_watch.text.trim()),
    'webhook' => true,
    _ => _preview != null,
  };

  RoutineOptions get _options {
    String? text(TextEditingController c) =>
        c.text.trim().isEmpty ? null : c.text.trim();
    return RoutineOptions(
      // A reminder's time is sent when it changes: one that already ran
      // keeps its (past) time when only its prompt is edited.
      at:
          _trigger == 'once' &&
              (widget.editing?.trigger != 'once' ||
                  _at.text.trim() != _atBefore)
          ? _at.text.trim()
          : null,
      watch: _trigger == 'watch' ? _watch.text.trim() : null,
      every: _trigger == 'watch' ? text(_every) : null,
      initiative: _initiative,
      gate: _trigger == 'watch' ? null : text(_gate),
      gateConnector: _trigger == 'watch' ? null : text(_gateConnector),
    );
  }

  bool get _valid =>
      _prompt.text.trim().isNotEmpty &&
      _triggerValid &&
      (!_push || _chatValid) &&
      _limitsValid;

  Future<void> _save() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    if (widget.editing == null && _trigger == 'webhook') {
      final r = await widget.store.createWebhook(
        prompt: _prompt.text.trim(),
        auth: _auth,
        deliverTo: _push ? '$_gateway:${_chat.text.trim()}' : null,
        limits: _newLimits,
        channel: widget.store.hasChannels ? _channel : null,
        agent: widget.store.hasAgents ? _agent : null,
        options: _options,
      );
      if (!mounted) return;
      if (r.webhook case final w?) {
        await showWebhookSecret(context, w, created: true);
        if (mounted) Navigator.pop(context);
      } else {
        setState(() {
          _busy = false;
          _error = r.error;
        });
      }
      return;
    }
    final err = await widget.store.saveSchedule(
      id: widget.editing?.id,
      cron: _trigger == 'cron' ? _cron.text.trim() : '',
      prompt: _prompt.text.trim(),
      deliverTo: _push ? '$_gateway:${_chat.text.trim()}' : null,
      limits: _newLimits,
      channel: widget.store.hasChannels ? _channel : null,
      agent: widget.store.hasAgents ? _agent : null,
      options: _options,
    );
    if (!mounted) return;
    if (err == null) {
      Navigator.pop(context);
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  /// Quiet hours, urgent, and the three caps (empty = the default).
  List<Widget> _limitsSection(BuildContext context) {
    final c = context.colors;
    final labels = Theme.of(context).textTheme.labelSmall;
    final small = TextStyle(fontSize: 12, color: c.muted);
    Widget cap(String label, TextEditingController ctl, String hint) =>
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(label, style: small),
              const SizedBox(height: 4),
              TextField(
                controller: ctl,
                onChanged: (_) => setState(() {}),
                keyboardType: TextInputType.number,
                decoration: _box(context, hint: hint),
              ),
            ],
          ),
        );
    return [
      const SizedBox(height: 6),
      Text('QUIET HOURS', style: labels),
      const SizedBox(height: 6),
      SegmentedButton<String?>(
        segments: const [
          ButtonSegment(value: null, label: Text('Default')),
          ButtonSegment(value: 'custom', label: Text('Custom')),
          ButtonSegment(value: 'none', label: Text('Off')),
        ],
        selected: {_quietMode},
        showSelectedIcon: false,
        onSelectionChanged: (v) => setState(() => _quietMode = v.first),
      ),
      if (_quietMode == 'custom') ...[
        const SizedBox(height: 8),
        TextField(
          controller: _quiet,
          onChanged: (_) => setState(() {}),
          style: mono.copyWith(fontSize: 13),
          decoration: _box(
            context,
            hint: '22:00-07:30',
            error: _window.hasMatch(_quiet.text.trim())
                ? null
                : 'Use HH:MM-HH:MM, e.g. 22:00-07:30',
          ),
        ),
      ],
      const SizedBox(height: 4),
      Text(
        'Results finishing in quiet hours are sent when they end, '
        'together in one message.',
        style: small,
      ),
      CheckboxListTile(
        value: _urgent,
        onChanged: (v) => setState(() => _urgent = v ?? false),
        dense: true,
        contentPadding: EdgeInsets.zero,
        controlAffinity: ListTileControlAffinity.leading,
        title: const Text('Urgent: send even during quiet hours'),
      ),
      const SizedBox(height: 6),
      Text('LIMITS', style: labels),
      const SizedBox(height: 6),
      Row(
        children: [
          cap('Runs per day', _runs, 'default'),
          const SizedBox(width: 8),
          cap('USD per day', _usd, 'default'),
          const SizedBox(width: 8),
          cap('Tokens per run', _tokens, 'default'),
        ],
      ),
      const SizedBox(height: 4),
      Text(
        'At a daily limit the routine pauses until tomorrow and tells you '
        'once. A run over its tokens is stopped.',
        style: small,
      ),
      if (_trigger != 'watch') ...[
        const SizedBox(height: 12),
        Text('GATE', style: labels),
        const SizedBox(height: 6),
        TextField(
          key: const Key('routine-gate'),
          controller: _gate,
          minLines: 1,
          maxLines: 4,
          style: mono.copyWith(fontSize: 13),
          decoration: _box(context, hint: 'Luau, e.g. os.date("*t").wday ~= 1'),
        ),
        const SizedBox(height: 6),
        TextField(
          key: const Key('routine-gate-connector'),
          controller: _gateConnector,
          style: mono.copyWith(fontSize: 13),
          decoration: _box(context, hint: 'connector it may use (optional)'),
        ),
        const SizedBox(height: 4),
        Text(
          _trigger == 'webhook'
              ? 'Checked on each delivery at no cost: false drops it, a table '
                    'replaces it, e.g. event.body.action == "opened" or '
                    'return { title = event.body.issue.title }.'
              : 'Checked before each run at no cost; false skips that run. '
                    'With a connector it can call its tools, e.g. '
                    '#connector.tools.unread.run(ctx, {}) > 0.',
          style: small,
        ),
      ],
    ];
  }

  /// A new webhook's auth choice, or an existing one's URL, state, and
  /// rotate and revoke.
  List<Widget> _webhookSection(BuildContext context) {
    final c = context.colors;
    final small = TextStyle(fontSize: 12, color: c.muted);
    if (widget.editing == null) {
      return [
        SegmentedButton<String>(
          key: const Key('routine-webhook-auth'),
          segments: const [
            ButtonSegment(value: 'hmac', label: Text('Signed (HMAC)')),
            ButtonSegment(value: 'bearer', label: Text('Secret in header')),
          ],
          selected: {_auth},
          showSelectedIcon: false,
          onSelectionChanged: (v) => setState(() => _auth = v.first),
        ),
        const SizedBox(height: 6),
        Text(
          'You get a URL and a secret once it is made. Each delivery starts a '
          'run with the event read as untrusted, so its writes ask first. '
          'Senders reach it only when tern has a [webhooks] listener.',
          style: small,
        ),
      ];
    }
    final w = _hook;
    return [
      if (_hookError case final e?)
        Text(e, style: TextStyle(fontSize: 12, color: c.error))
      else if (w == null)
        Text('Loading its webhook…', style: small)
      else ...[
        SelectableText(
          w.url ?? w.path,
          key: const Key('routine-webhook-url'),
          style: mono.copyWith(fontSize: 12.5),
        ),
        const SizedBox(height: 4),
        Text(
          [
            if (!w.active) 'Revoked: the URL answers 404 until you rotate it.',
            if (w.url == null)
              'No [webhooks] listener yet: only `tern hooks fire` reaches it.',
            w.howToSign,
          ].join(' '),
          style: small,
        ),
        const SizedBox(height: 6),
        Row(
          children: [
            OutlinedButton(
              key: const Key('routine-webhook-rotate'),
              onPressed: _rotate,
              child: Text(w.active ? 'Rotate secret…' : 'New secret…'),
            ),
            const SizedBox(width: 8),
            if (w.active)
              TextButton(
                key: const Key('routine-webhook-revoke'),
                onPressed: _revoke,
                child: const Text('Revoke'),
              ),
          ],
        ),
      ],
    ];
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final labels = Theme.of(context).textTheme.labelSmall;
    final cron = _cron.text.trim();
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                widget.editing == null ? 'New routine' : 'Edit routine',
                style: const TextStyle(
                  fontSize: 17,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 4),
              Text(
                'tern runs this on its own while it runs in the background. '
                'Each routine keeps its own conversation.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 18),
              Text('WHAT TO DO', style: labels),
              const SizedBox(height: 6),
              TextField(
                controller: _prompt,
                autofocus: widget.editing == null,
                minLines: 3,
                maxLines: 6,
                decoration: _box(
                  context,
                  hint: 'Summarize my unread email and flag anything urgent',
                ),
              ),
              const SizedBox(height: 16),
              Text('WHEN', style: labels),
              const SizedBox(height: 6),
              // A webhook routine keeps its trigger (its secret goes with
              // it); others can't become one, since that needs a secret.
              if (widget.editing?.trigger != 'webhook')
                SegmentedButton<String>(
                  key: const Key('routine-trigger'),
                  segments: [
                    const ButtonSegment(
                      value: 'cron',
                      label: Text('Repeating'),
                    ),
                    const ButtonSegment(value: 'once', label: Text('Once')),
                    const ButtonSegment(
                      value: 'watch',
                      label: Text('On events'),
                    ),
                    if (widget.editing == null)
                      const ButtonSegment(
                        value: 'webhook',
                        label: Text('Webhook'),
                      ),
                  ],
                  selected: {_trigger},
                  showSelectedIcon: false,
                  onSelectionChanged: (v) => setState(() => _trigger = v.first),
                ),
              const SizedBox(height: 8),
              if (_trigger == 'webhook') ..._webhookSection(context),
              if (_trigger == 'once') ...[
                TextField(
                  key: const Key('routine-at'),
                  controller: _at,
                  style: mono.copyWith(fontSize: 13),
                  decoration: _box(context, hint: 'YYYY-MM-DD HH:MM'),
                ),
                const SizedBox(height: 6),
                Text(
                  'Runs once at that local time, then turns itself off.',
                  style: TextStyle(fontSize: 12, color: c.muted),
                ),
              ],
              if (_trigger == 'watch') ...[
                Row(
                  children: [
                    Expanded(
                      flex: 3,
                      child: TextField(
                        key: const Key('routine-watch'),
                        controller: _watch,
                        style: mono.copyWith(fontSize: 13),
                        decoration: _box(context, hint: 'connector.watch'),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: TextField(
                        key: const Key('routine-every'),
                        controller: _every,
                        style: mono.copyWith(fontSize: 13),
                        decoration: _box(context, hint: 'every, e.g. 5m'),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 6),
                Text(
                  'A connector\'s watch is checked on its own, with no model '
                  'call; each new event starts a run, read as untrusted. '
                  'What is already there when it starts doesn\'t run.',
                  style: TextStyle(fontSize: 12, color: c.muted),
                ),
              ],
              if (_trigger == 'cron') ...[
                Wrap(
                  spacing: 6,
                  runSpacing: 6,
                  children: [
                    for (final p in _presets)
                      ChoiceChip(
                        label: Text(p.label),
                        selected: cron == p.cron,
                        onSelected: (_) => _cron.text = p.cron,
                        showCheckmark: false,
                        visualDensity: VisualDensity.compact,
                      ),
                  ],
                ),
                const SizedBox(height: 8),
                TextField(
                  controller: _cron,
                  style: mono.copyWith(fontSize: 13),
                  decoration: _box(
                    context,
                    hint: 'minute hour day month weekday',
                    error: _cronError,
                  ),
                ),
                const SizedBox(height: 6),
                if (_preview case final p?)
                  Text(
                    '${cronInWords(p.cron)}. Next: '
                    '${p.next.map(whenInWords).join(', ')}',
                    style: TextStyle(fontSize: 12, color: c.muted),
                  ),
              ],
              const SizedBox(height: 16),
              Text('RESULTS', style: labels),
              const SizedBox(height: 6),
              if (widget.store.hasAgents && widget.store.agents.length > 1) ...[
                DropdownButtonFormField<String>(
                  key: const Key('routine-agent'),
                  initialValue: _agent,
                  isDense: true,
                  decoration: _box(context),
                  items: [
                    for (final a in widget.store.agents)
                      if (!a.archived || a.id == _agent)
                        DropdownMenuItem(
                          value: a.id,
                          child: Text('Done by ${a.name}'),
                        ),
                  ],
                  onChanged: (v) => setState(() {
                    if (v == null) return;
                    // Results follow the job to its new agent's DM.
                    if (_channel == _dmOf(_agent)) _channel = _dmOf(v);
                    _agent = v;
                  }),
                ),
                const SizedBox(height: 8),
              ],
              if (widget.store.hasChannels) ...[
                DropdownButtonFormField<String>(
                  key: ValueKey('routine-channel-$_channel'),
                  initialValue: _channel,
                  isDense: true,
                  decoration: _box(context),
                  items: [
                    for (final ch in widget.store.channels)
                      if (!ch.archived || ch.id == _channel)
                        DropdownMenuItem(
                          value: ch.id,
                          child: Text(
                            ch.isHome
                                ? 'Post to Home'
                                : ch.isDm
                                ? 'Post to ${ch.title}\'s chat'
                                : 'Post to #${ch.slug}',
                          ),
                        ),
                  ],
                  onChanged: (v) =>
                      setState(() => _channel = v ?? TernStore.homeId),
                ),
                const SizedBox(height: 8),
              ],
              SegmentedButton<bool>(
                segments: [
                  const ButtonSegment(
                    value: false,
                    label: Text('Keep in tern'),
                  ),
                  ButtonSegment(
                    value: true,
                    label: Text(
                      _gateways.length == 1
                          ? 'Also send to ${_title(_gateways.first.kind)}'
                          : 'Also send to…',
                    ),
                  ),
                ],
                selected: {_push},
                showSelectedIcon: false,
                onSelectionChanged: (v) => setState(() => _push = v.first),
              ),
              if (_push) ...[
                const SizedBox(height: 8),
                if (_gateways.length > 1) ...[
                  DropdownButtonFormField<String>(
                    key: const Key('routine-gateway'),
                    initialValue: _gateway,
                    isDense: true,
                    isExpanded: true,
                    decoration: _box(context),
                    items: [
                      for (final g in _gateways)
                        DropdownMenuItem(
                          value: g.name,
                          child: Text(
                            g.name == g.kind
                                ? _title(g.kind)
                                : '${g.name} (${_title(g.kind)})',
                          ),
                        ),
                    ],
                    onChanged: (v) => setState(() => _gateway = v ?? _gateway),
                  ),
                  const SizedBox(height: 8),
                ],
                TextField(
                  key: const Key('routine-chat'),
                  controller: _chat,
                  onChanged: (_) => setState(() {}),
                  decoration: _box(context, hint: _chatHint),
                ),
              ],
              const SizedBox(height: 8),
              DropdownButtonFormField<String?>(
                key: const Key('routine-initiative'),
                initialValue: _initiative,
                isDense: true,
                isExpanded: true,
                decoration: _box(context),
                items: [
                  const DropdownMenuItem(
                    value: null,
                    child: Text('Default (digest)'),
                  ),
                  for (final e in _initiatives.entries)
                    DropdownMenuItem(
                      value: e.key,
                      child: Text(e.value, overflow: TextOverflow.ellipsis),
                    ),
                ],
                onChanged: (v) => setState(() => _initiative = v),
              ),
              const SizedBox(height: 4),
              Text(
                'Results are always posted; this decides when they are also '
                'sent to your chat. Digests go out at 08:30 and 17:30.',
                style: TextStyle(fontSize: 12, color: c.muted),
              ),
              const SizedBox(height: 8),
              Align(
                alignment: Alignment.centerLeft,
                child: TextButton.icon(
                  onPressed: () => setState(() => _showLimits = !_showLimits),
                  icon: Icon(
                    _showLimits ? Icons.expand_less : Icons.expand_more,
                    size: 16,
                    color: c.muted,
                  ),
                  label: Text(
                    'Quiet hours and limits',
                    style: TextStyle(color: c.muted),
                  ),
                  style: TextButton.styleFrom(
                    padding: const EdgeInsets.symmetric(horizontal: 4),
                  ),
                ),
              ),
              if (_showLimits) ..._limitsSection(context),
              if (_error case final e?) ...[
                const SizedBox(height: 12),
                Text(e, style: TextStyle(fontSize: 12.5, color: c.error)),
              ],
              const SizedBox(height: 18),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Cancel'),
                  ),
                  const SizedBox(width: 8),
                  FilledButton(
                    onPressed: _valid && !_busy ? _save : null,
                    style: FilledButton.styleFrom(
                      backgroundColor: c.accent,
                      foregroundColor: c.onAccent,
                    ),
                    child: Text(
                      widget.editing == null ? 'Create routine' : 'Save',
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Show a webhook's URL and secret, the one time the daemon gives it.
Future<void> showWebhookSecret(
  BuildContext context,
  Webhook w, {
  required bool created,
}) => showDialog<void>(
  context: context,
  builder: (context) {
    final c = context.colors;
    Widget field(String label, String value, Key key) => Row(
      children: [
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(label, style: Theme.of(context).textTheme.labelSmall),
              SelectableText(
                value,
                key: key,
                style: mono.copyWith(fontSize: 12.5),
              ),
            ],
          ),
        ),
        IconButton(
          tooltip: 'Copy',
          icon: const Icon(Icons.copy, size: 16),
          onPressed: () => Clipboard.setData(ClipboardData(text: value)),
        ),
      ],
    );
    return AlertDialog(
      title: Text(created ? 'Webhook created' : 'New webhook secret'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 480),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            field('URL', w.url ?? w.path, const Key('webhook-url')),
            const SizedBox(height: 10),
            field('SECRET', w.secret ?? '', const Key('webhook-secret')),
            const SizedBox(height: 10),
            Text(w.howToSign, style: TextStyle(fontSize: 12, color: c.muted)),
            const SizedBox(height: 6),
            Text(
              'This is the only time the secret is shown. Lost it? Rotate it.',
              style: TextStyle(fontSize: 12, color: c.muted),
            ),
          ],
        ),
      ),
      actions: [
        FilledButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Done'),
        ),
      ],
    );
  },
);

InputDecoration _box(BuildContext context, {String? hint, String? error}) {
  final c = context.colors;
  OutlineInputBorder border(Color color) => OutlineInputBorder(
    borderRadius: BorderRadius.circular(8),
    borderSide: BorderSide(color: color),
  );
  return InputDecoration(
    hintText: hint,
    hintStyle: TextStyle(color: c.muted),
    errorText: error,
    errorMaxLines: 2,
    isDense: true,
    contentPadding: const EdgeInsets.all(10),
    border: border(c.hairline),
    enabledBorder: border(c.hairline),
    focusedBorder: border(c.accent),
  );
}
