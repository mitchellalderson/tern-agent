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
    // The panel is narrow: the schedule in words here, the next run on hover.
    final detail = !s.enabled
        ? 'Paused'
        : failed
        ? 'Last run failed · ${cronInWords(s.cron)}'
        : cronInWords(s.cron);
    final next = s.nextRunAt;
    return InkWell(
      borderRadius: BorderRadius.circular(8),
      onTap: () => store.open(s.sessionId),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 6),
        child: Row(
          children: [
            Icon(
              Icons.schedule,
              size: 16,
              color: s.enabled ? c.accent : c.muted,
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
                      color: s.enabled ? c.text : c.muted,
                    ),
                  ),
                  Tooltip(
                    message: next == null
                        ? s.cron
                        : 'Next run ${whenInWords(next)} · ${s.cron}',
                    child: Text(
                      detail,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 11.5,
                        color: failed && s.enabled ? c.error : c.muted,
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
                'pause' => store.setScheduleEnabled(s, !s.enabled),
                'delete' => _confirmDelete(context),
                _ => null,
              },
              itemBuilder: (_) => [
                const PopupMenuItem(value: 'run', child: Text('Run now')),
                const PopupMenuItem(value: 'edit', child: Text('Edit…')),
                PopupMenuItem(
                  value: 'pause',
                  child: Text(s.enabled ? 'Pause' : 'Resume'),
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

  bool get _valid =>
      _prompt.text.trim().isNotEmpty &&
      _preview != null &&
      (!_telegram || int.tryParse(_chat.text.trim()) != null);

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

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final labels = Theme.of(context).textTheme.labelSmall;
    final cron = _cron.text.trim();
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: Padding(
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
                'tern runs this on its own while the daemon is running. '
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
