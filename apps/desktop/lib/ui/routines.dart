import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/cron_words.dart';
import '../state/store.dart';
import '../theme.dart';

/// The routines section of the side panel.
class RoutinesSection extends StatelessWidget {
  const RoutinesSection({super.key, required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text('ROUTINES', style: Theme.of(context).textTheme.labelSmall),
        const SizedBox(height: 6),
        for (final s in store.schedules) _RoutineRow(store: store, schedule: s),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: () => showRoutineSheet(context, store),
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
  const _RoutineRow({required this.store, required this.schedule});

  final TernStore store;
  final Schedule schedule;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = schedule;
    final title = s.prompt.split('\n').first;
    final failed = s.lastStatus == 'failed';
    final budget = s.enabled && s.pausedByBudget;
    final active = s.enabled && !budget;
    // The panel is narrow: the schedule in words here, the next run on hover.
    final detail = !s.enabled
        ? 'Paused'
        : budget
        ? 'Paused: budget · resumes ${_resumes(s.pausedUntil)}'
        : failed
        ? 'Last run failed · ${cronInWords(s.cron)}'
        : s.lastQuiet
        ? 'Last run quiet · ${cronInWords(s.cron)}'
        : cronInWords(s.cron);
    final postsTo = store.channels
        .where((ch) => ch.id == s.channelId && !ch.isHome)
        .firstOrNull;
    final next = s.nextRunAt;
    final tip = budget
        ? 'A daily cap (${s.pausedReason ?? 'budget'}) was reached. '
              'Resume it now from the menu, or it continues on its own.'
        : s.lastQuiet && s.enabled
        ? 'The last run had nothing to report, so nothing was sent.'
        : next == null
        ? s.cron
        : 'Next run ${whenInWords(next)} · ${s.cron}';
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
              budget ? Icons.pause_circle_outline : Icons.schedule,
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
                      postsTo == null ? detail : '$detail · #${postsTo.slug}',
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
}) => showDialog<void>(
  context: context,
  builder: (_) => RoutineSheet(store: store, editing: editing),
);

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
  const RoutineSheet({super.key, required this.store, this.editing});

  final TernStore store;
  final Schedule? editing;

  @override
  State<RoutineSheet> createState() => _RoutineSheetState();
}

class _RoutineSheetState extends State<RoutineSheet> {
  late final _prompt = TextEditingController(text: widget.editing?.prompt);
  late final _cron = TextEditingController(
    text: widget.editing?.cron ?? _presets.first.cron,
  );
  late final _chat = TextEditingController(
    text: widget.editing?.deliverTo?.replaceFirst('telegram:', ''),
  );
  late var _telegram = widget.editing?.deliverTo != null;

  /// Where results are posted: Home, or a topic channel (the one open when
  /// a new routine is made there).
  late var _channel =
      widget.editing?.channelId ??
      (widget.store.topicOpen ? widget.store.channel! : TernStore.homeId);
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
    _checkCron();
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _prompt.dispose();
    _cron.dispose();
    _chat.dispose();
    for (final c in [_quiet, _runs, _usd, _tokens]) {
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

  bool get _valid =>
      _prompt.text.trim().isNotEmpty &&
      _preview != null &&
      (!_telegram || int.tryParse(_chat.text.trim()) != null) &&
      _limitsValid;

  Future<void> _save() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.store.saveSchedule(
      id: widget.editing?.id,
      cron: _cron.text.trim(),
      prompt: _prompt.text.trim(),
      deliverTo: _telegram ? 'telegram:${_chat.text.trim()}' : null,
      limits: _newLimits,
      channel: widget.store.hasChannels ? _channel : null,
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
              const SizedBox(height: 16),
              Text('RESULTS', style: labels),
              const SizedBox(height: 6),
              if (widget.store.hasChannels) ...[
                DropdownButtonFormField<String>(
                  initialValue: _channel,
                  isDense: true,
                  decoration: _box(context),
                  items: [
                    for (final ch in widget.store.channels)
                      if (!ch.archived || ch.id == _channel)
                        DropdownMenuItem(
                          value: ch.id,
                          child: Text(
                            ch.isHome ? 'Post to Home' : 'Post to #${ch.slug}',
                          ),
                        ),
                  ],
                  onChanged: (v) =>
                      setState(() => _channel = v ?? TernStore.homeId),
                ),
                const SizedBox(height: 8),
              ],
              SegmentedButton<bool>(
                segments: const [
                  ButtonSegment(value: false, label: Text('Keep in tern')),
                  ButtonSegment(
                    value: true,
                    label: Text('Also send to Telegram'),
                  ),
                ],
                selected: {_telegram},
                showSelectedIcon: false,
                onSelectionChanged: (v) => setState(() => _telegram = v.first),
              ),
              if (_telegram) ...[
                const SizedBox(height: 8),
                TextField(
                  controller: _chat,
                  onChanged: (_) => setState(() {}),
                  decoration: _box(context, hint: 'Telegram chat id'),
                ),
              ],
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
