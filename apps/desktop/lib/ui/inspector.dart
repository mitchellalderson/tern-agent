import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/cron_words.dart';
import '../state/store.dart';
import '../state/transcript.dart' show summarize;
import '../theme.dart';
import 'widgets.dart';

/// The run inspector (gui-spec §4.4): a conversation's runs, and the
/// selected run's journal steps.
Future<void> showInspector(
  BuildContext context,
  TernStore store, {
  required String session,
  String? run,
}) => showDialog<void>(
  context: context,
  builder: (_) => RunInspector(store: store, session: session, run: run),
);

class RunInspector extends StatefulWidget {
  const RunInspector({
    super.key,
    required this.store,
    required this.session,
    this.run,
  });

  final TernStore store;
  final String session;
  final String? run;

  @override
  State<RunInspector> createState() => _RunInspectorState();
}

class _RunInspectorState extends State<RunInspector> {
  List<RunItem>? _runs;
  String? _selected;
  List<StepItem>? _steps;
  String? _error;
  final _open = <int>{};
  Timer? _refresh;

  @override
  void initState() {
    super.initState();
    _load();
    // The journal grows while a run goes; keep up with it.
    _refresh = Timer.periodic(const Duration(seconds: 2), (_) {
      final run = _selected;
      if (run != null && _live(run)) {
        _pick(run, quiet: true);
        _reloadRuns();
      }
    });
  }

  @override
  void dispose() {
    _refresh?.cancel();
    super.dispose();
  }

  bool _live(String run) =>
      widget.store.transcripts.values.any((t) => t.activeRun == run) ||
      (_runs?.any(
            (r) =>
                r.id == run && (r.status == 'running' || r.status == 'queued'),
          ) ??
          false);

  Future<void> _load() async {
    try {
      final runs = await widget.store.client.runs(
        widget.session,
        subagents: true,
      );
      if (!mounted) return;
      setState(() => _runs = _tree(runs));
      await _pick(widget.run ?? runs.lastOrNull?.id);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _reloadRuns() async {
    try {
      final runs = await widget.store.client.runs(
        widget.session,
        subagents: true,
      );
      if (mounted) setState(() => _runs = _tree(runs));
    } on TernException {
      // Keep the list we have.
    }
  }

  /// Newest first, each run followed by the sub-agents it started.
  static List<RunItem> _tree(List<RunItem> runs) {
    final kids = <String, List<RunItem>>{};
    for (final r in runs) {
      if (r.parentRunId case final p?) (kids[p] ??= []).add(r);
    }
    final ids = runs.map((r) => r.id).toSet();
    final out = <RunItem>[];
    void add(RunItem r) {
      out.add(r);
      for (final k in kids[r.id] ?? const <RunItem>[]) {
        add(k);
      }
    }

    // Roots: runs whose parent isn't listed (normally the user's).
    for (final r in runs.reversed) {
      if (r.parentRunId == null || !ids.contains(r.parentRunId)) add(r);
    }
    return out;
  }

  Future<void> _pick(String? run, {bool quiet = false}) async {
    if (run == null) return;
    if (!quiet) {
      setState(() {
        _selected = run;
        _steps = null;
        _open.clear();
      });
    }
    try {
      final steps = await widget.store.client.steps(run);
      if (!mounted || _selected != run) return;
      setState(() {
        // Open what's in flight, so its input shows without a click.
        final fresh = _steps == null;
        _steps = steps;
        if (fresh) {
          _open.addAll(
            steps.where((s) => s.status == 'started').map((s) => s.idx),
          );
        }
      });
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      clipBehavior: Clip.antiAlias,
      child: SizedBox(
        width: 860,
        height: 560,
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 14, 8, 10),
              child: Row(
                children: [
                  const Expanded(
                    child: Text(
                      'Runs',
                      style: TextStyle(
                        fontSize: 16,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  if (_steps != null && _selected != null)
                    CopyButton(
                      key: const Key('copy-run'),
                      tooltip: 'Copy run details',
                      size: 17,
                      text: _report,
                    ),
                  QuietIconButton(
                    icon: Icons.close,
                    tooltip: 'Close',
                    onPressed: () => Navigator.pop(context),
                  ),
                ],
              ),
            ),
            const Hairline(),
            Expanded(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Container(
                    width: 230,
                    color: c.sidebar,
                    child: _runs == null
                        ? const Center(child: CircularProgressIndicator())
                        : ListView(
                            padding: const EdgeInsets.all(8),
                            children: [
                              for (final r in _runs!)
                                _RunRow(
                                  run: r,
                                  selected: r.id == _selected,
                                  onTap: () => _pick(r.id),
                                ),
                            ],
                          ),
                  ),
                  const Hairline(vertical: true),
                  Expanded(child: _stepsView(context)),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _stepsView(BuildContext context) {
    final c = context.colors;
    if (_error case final e?) {
      return Center(
        child: Text(e, style: TextStyle(color: c.error)),
      );
    }
    final steps = _steps;
    if (steps == null) return const Center(child: CircularProgressIndicator());
    if (steps.isEmpty) {
      return Center(
        child: Text(
          'No steps yet',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      );
    }
    final run = _runs?.where((r) => r.id == _selected).firstOrNull;
    final tokens = steps.fold(0, (n, s) => n + (s.usage?.total ?? 0));
    return ListView(
      padding: const EdgeInsets.fromLTRB(18, 14, 18, 18),
      children: [
        if (run != null)
          Padding(
            padding: const EdgeInsets.only(bottom: 10),
            child: Text(
              [
                run.status,
                ?run.model,
                '${steps.length} steps',
                if (tokens > 0) '${formatTokens(tokens)} tokens',
              ].join(' · '),
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
          ),
        if (run?.taint case final t?)
          Padding(
            padding: const EdgeInsets.only(bottom: 10),
            child: Row(
              children: [
                TaintBadge(reason: t, label: 'tainted'),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    'Read untrusted content ($t): its changes ask first.',
                    style: TextStyle(fontSize: 12.5, color: c.muted),
                  ),
                ),
              ],
            ),
          ),
        if (run?.error case final e?)
          Padding(
            padding: const EdgeInsets.only(bottom: 10),
            child: SelectableText(
              e,
              style: TextStyle(fontSize: 12.5, color: c.error),
            ),
          ),
        for (final s in steps) ...[
          InkWell(
            borderRadius: BorderRadius.circular(6),
            onTap: () => setState(
              () => _open.contains(s.idx)
                  ? _open.remove(s.idx)
                  : _open.add(s.idx),
            ),
            child: Padding(
              padding: const EdgeInsets.symmetric(vertical: 5),
              child: Row(
                children: [
                  SizedBox(
                    width: 26,
                    child: Text(
                      '${s.idx}',
                      style: mono.copyWith(color: c.muted, fontSize: 11),
                    ),
                  ),
                  Icon(_kindIcon(s.kind), size: 15, color: c.muted),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      _label(s),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: 13,
                        color: s.status == 'failed' ? c.error : c.text,
                      ),
                    ),
                  ),
                  if (s.usage case final u? when u.total > 0)
                    Padding(
                      padding: const EdgeInsets.only(left: 8),
                      child: Text(
                        '${formatTokens(u.total)} tok',
                        style: TextStyle(fontSize: 11.5, color: c.muted),
                      ),
                    ),
                  SizedBox(
                    width: 64,
                    child: Text(
                      s.status == 'started'
                          ? 'running'
                          : _ms(s.duration ?? Duration.zero),
                      textAlign: TextAlign.right,
                      style: TextStyle(fontSize: 11.5, color: c.muted),
                    ),
                  ),
                ],
              ),
            ),
          ),
          if (_open.contains(s.idx)) _detail(context, s),
        ],
      ],
    );
  }

  /// The tool call id of a running tool step (its input is the call).
  static String? _callId(StepItem s) => s.status == 'started' && s.input is Map
      ? (s.input as Map)['id'] as String?
      : null;

  static String _pretty(Object? v) =>
      v is String ? v : const JsonEncoder.withIndent('  ').convert(v);

  /// The selected run as plain text, for pasting into a bug report or a
  /// chat: its summary and error, every step on one line, and the input,
  /// output and error of each step that failed.
  String _report() {
    final run = _runs?.where((r) => r.id == _selected).firstOrNull;
    final steps = _steps ?? const <StepItem>[];
    final out = StringBuffer()
      ..writeln(
        [
          'Run ${run?.id ?? _selected}',
          ?run?.label,
          ?run?.status,
          ?run?.model,
          if (run != null) run.trigger,
        ].join(' · '),
      );
    if (run?.error case final e?) out.writeln('Error: $e');
    if (run?.taint case final t?) out.writeln('Tainted: $t');
    out.writeln();
    for (final s in steps) {
      out.writeln(
        '${s.idx}. ${_label(s)} [${s.status}'
        '${s.duration == null ? '' : ', ${_ms(s.duration!)}'}]',
      );
    }
    for (final s in steps.where(
      (s) => s.status == 'failed' || s.error != null,
    )) {
      out
        ..writeln()
        ..writeln('--- step ${s.idx}: ${_label(s)} ---')
        ..writeln('Input:')
        ..writeln(_pretty(s.input));
      if (s.output != null) {
        out
          ..writeln('Output:')
          ..writeln(_pretty(s.output));
      }
      if (s.error case final e?) {
        out
          ..writeln('Error:')
          ..writeln(e);
      }
    }
    return out.toString().trimRight();
  }

  Widget _detail(BuildContext context, StepItem s) {
    final c = context.colors;
    Widget block(String label, Object? v) => Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Text(label, style: Theme.of(context).textTheme.labelSmall),
            const SizedBox(width: 2),
            CopyButton(
              key: Key('copy-${s.idx}-$label'),
              tooltip: 'Copy ${label.toLowerCase()}',
              size: 13,
              text: () => _pretty(v),
            ),
          ],
        ),
        Container(
          width: double.infinity,
          constraints: const BoxConstraints(maxHeight: 220),
          decoration: BoxDecoration(
            color: c.card,
            border: Border.all(color: c.hairline),
            borderRadius: BorderRadius.circular(8),
          ),
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(10),
            child: SelectableText(
              _pretty(v),
              style: mono.copyWith(color: c.text),
            ),
          ),
        ),
        const SizedBox(height: 8),
      ],
    );
    return Padding(
      padding: const EdgeInsets.fromLTRB(26, 2, 0, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            'Started ${whenInWords(s.startedAt)}',
            style: TextStyle(fontSize: 11.5, color: c.muted),
          ),
          const SizedBox(height: 6),
          block('INPUT', s.input),
          if (s.output != null)
            block('OUTPUT', s.output)
          else if (_callId(s) case final id?)
            // Not journaled until it ends; show what the daemon streamed.
            ListenableBuilder(
              listenable: widget.store,
              builder: (context, _) {
                final live = widget.store.transcripts.values
                    .map((t) => t.liveOutput(id))
                    .nonNulls
                    .firstOrNull;
                return live == null || live.isEmpty
                    ? const SizedBox.shrink()
                    : block('OUTPUT SO FAR', live);
              },
            ),
          if (s.error case final e?) block('ERROR', e),
        ],
      ),
    );
  }

  static IconData _kindIcon(String kind) => switch (kind) {
    'llm' => Icons.auto_awesome_outlined,
    'tool' => Icons.build_outlined,
    'approval' => Icons.front_hand_outlined,
    'compaction' => Icons.compress,
    _ => Icons.circle_outlined,
  };

  /// A one-line description from the journaled input, whose shape depends
  /// on the step kind.
  static String _label(StepItem s) {
    final input = s.input;
    if (input is Map) {
      if (s.kind == 'inbox') {
        final mail = switch (s.output) {
          {'mail': final List m} => m.whereType<Map>(),
          _ => const <Map>[],
        };
        final what = [
          for (final m in mail)
            '${m['agent'] ?? 'message'} ${switch (m['kind']) {
              'finished' => m['status'] ?? 'finished',
              'question' => 'asked',
              final k => k,
            }}',
        ].join(', ');
        return what.isEmpty
            ? (input['wait'] == true ? 'Waiting on sub-agents' : 'Inbox')
            : 'Inbox: $what';
      }
      if (s.kind == 'llm') {
        return 'Model call${input['model'] is String ? ' · ${input['model']}' : ''}';
      }
      if (input['name'] case final String name) {
        final call = summarize(name, input['input']);
        return s.kind == 'approval'
            ? 'Approval: $call${s.output is String ? ' → ${s.output}' : ''}'
            : call;
      }
    }
    return s.kind[0].toUpperCase() + s.kind.substring(1);
  }

  static String _ms(Duration d) => d.inMilliseconds < 1000
      ? '${d.inMilliseconds} ms'
      : '${(d.inMilliseconds / 1000).toStringAsFixed(1)} s';
}

class _RunRow extends StatelessWidget {
  const _RunRow({
    required this.run,
    required this.selected,
    required this.onTap,
  });

  final RunItem run;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final color = switch (run.status) {
      'completed' => c.ok,
      'failed' => c.error,
      'running' => c.accent,
      _ => c.muted,
    };
    return Material(
      color: selected ? c.selection : Colors.transparent,
      borderRadius: BorderRadius.circular(8),
      child: InkWell(
        borderRadius: BorderRadius.circular(8),
        onTap: onTap,
        child: Padding(
          padding: EdgeInsets.fromLTRB(10.0 + 14 * run.depth, 8, 10, 8),
          child: Row(
            children: [
              Container(
                width: 8,
                height: 8,
                decoration: BoxDecoration(color: color, shape: BoxShape.circle),
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (run.label case final label?)
                      Text(
                        label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: mono.copyWith(fontSize: 12.5),
                      )
                    else
                      Text(
                        whenInWords(run.startedAt),
                        style: const TextStyle(fontSize: 13),
                      ),
                    Text(
                      run.label == null
                          ? '${run.status} · ${run.trigger}'
                          : '${run.status} · sub-agent',
                      style: TextStyle(fontSize: 11.5, color: c.muted),
                    ),
                  ],
                ),
              ),
              if (run.tainted) ...[
                const SizedBox(width: 6),
                TaintBadge(reason: run.taint, label: 'tainted'),
              ],
            ],
          ),
        ),
      ),
    );
  }
}
