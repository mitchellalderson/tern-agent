import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/cron_words.dart';
import '../state/store.dart';
import '../theme.dart';

/// The routines section of the side panel. With agent profiles it shows
/// the open agent's jobs (agent profiles §4), and **All** shows every
/// agent's, so what runs at night stays visible in one place.
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
    final shown = agent == null || _all
        ? store.schedules
        : store.schedulesOf(agent);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                agent == null || _all
                    ? 'ROUTINES'
                    : 'ROUTINES · ${agent.name.toUpperCase()}',
                style: Theme.of(context).textTheme.labelSmall,
              ),
            ),
            if (agent != null && store.otherAgents.isNotEmpty)
              SizedBox(
                height: 22,
                child: TextButton(
                  onPressed: () => setState(() => _all = !_all),
                  style: TextButton.styleFrom(
                    padding: const EdgeInsets.symmetric(horizontal: 6),
                    minimumSize: Size.zero,
                  ),
                  child: Text(
                    _all ? agent.name : 'All',
                    style: TextStyle(fontSize: 11.5, color: c.muted),
                  ),
                ),
              ),
          ],
        ),
        const SizedBox(height: 6),
        for (final s in shown)
          _RoutineRow(
            store: store,
            schedule: s,
            showAgent: _all || agent == null,
          ),
        if (shown.isEmpty && agent != null && !_all)
          Text(
            'Ask ${agent.name} to do something on a schedule, or add one.',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
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

class _RoutineRow extends StatelessWidget {
  const _RoutineRow({
    required this.store,
    required this.schedule,
    this.showAgent = false,
  });

  final TernStore store;
  final Schedule schedule;

  /// Name the agent whose job it is (the All list).
  final bool showAgent;

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
        : when;
    final postsTo = store.channels
        .where((ch) => ch.id == s.channelId && !ch.isMain)
        .firstOrNull;
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
                        if (postsTo != null) '#${postsTo.slug}',
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

  /// `cron`, `once` or `watch`.
  late var _trigger = widget.editing?.trigger ?? 'cron';
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
          'Checked before each run at no cost; false skips that run. With a '
          'connector it can call its tools, e.g. '
          '#connector.tools.unread.run(ctx, {}) > 0.',
          style: small,
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
              SegmentedButton<String>(
                key: const Key('routine-trigger'),
                segments: const [
                  ButtonSegment(value: 'cron', label: Text('Repeating')),
                  ButtonSegment(value: 'once', label: Text('Once')),
                  ButtonSegment(value: 'watch', label: Text('On events')),
                ],
                selected: {_trigger},
                showSelectedIcon: false,
                onSelectionChanged: (v) => setState(() => _trigger = v.first),
              ),
              const SizedBox(height: 8),
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
