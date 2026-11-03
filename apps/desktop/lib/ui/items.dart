import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'inspector.dart';

/// One transcript item.
class ItemView extends StatelessWidget {
  const ItemView({super.key, required this.item, required this.store});

  final Item item;
  final TernStore store;

  @override
  Widget build(BuildContext context) => switch (item) {
    UserItem(:final text) => _UserMessage(text),
    AssistantItem(:final text, :final streaming) => _AssistantMessage(
      text,
      streaming: streaming,
    ),
    final WorkItem w => WorkCard(
      work: w,
      onInspect: switch ((store.selected, w.runId)) {
        (final String session, final String run) => () => showInspector(
          context,
          store,
          session: session,
          run: run,
        ),
        _ => null,
      },
    ),
    final PillItem p => _Pill(p),
    final ApprovalItem a => ApprovalCard(item: a, store: store),
    final RunEndItem r => _RunEnd(r, store: store),
  };
}

class _UserMessage extends StatelessWidget {
  const _UserMessage(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Align(
    alignment: Alignment.centerRight,
    child: Container(
      constraints: const BoxConstraints(maxWidth: 560),
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 9),
      decoration: BoxDecoration(
        color: context.colors.userBubble,
        borderRadius: BorderRadius.circular(16),
      ),
      child: SelectableText(text),
    ),
  );
}

class _AssistantMessage extends StatelessWidget {
  const _AssistantMessage(this.text, {required this.streaming});

  final String text;
  final bool streaming;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final theme = Theme.of(context);
    final body = theme.textTheme.bodyMedium!;
    return MarkdownBody(
      data: streaming ? '$text ▍' : text,
      selectable: true,
      styleSheet: MarkdownStyleSheet.fromTheme(theme).copyWith(
        p: body,
        h1: body.copyWith(fontSize: 20, fontWeight: FontWeight.w600),
        h2: body.copyWith(fontSize: 17, fontWeight: FontWeight.w600),
        h3: body.copyWith(fontSize: 15, fontWeight: FontWeight.w600),
        code: mono.copyWith(color: c.text, backgroundColor: c.card),
        codeblockDecoration: BoxDecoration(
          color: c.card,
          border: Border.all(color: c.hairline),
          borderRadius: BorderRadius.circular(10),
        ),
        codeblockPadding: const EdgeInsets.all(12),
        blockquoteDecoration: BoxDecoration(
          border: Border(left: BorderSide(color: c.hairline, width: 3)),
        ),
        a: body.copyWith(color: c.accent),
        listBullet: body,
      ),
    );
  }
}

/// Tool calls between two messages, one line each; click a line for its
/// output.
class WorkCard extends StatefulWidget {
  const WorkCard({super.key, required this.work, this.onInspect});

  final WorkItem work;
  final VoidCallback? onInspect;

  @override
  State<WorkCard> createState() => _WorkCardState();
}

class _WorkCardState extends State<WorkCard> {
  final _open = <String>{};

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final w = widget.work;
    final elapsed = w.elapsed;
    final title = w.running
        ? 'Working…'
        : elapsed != null && elapsed.inSeconds >= 1
        ? 'Worked for ${_duration(elapsed)}'
        : w.steps.length == 1
        ? 'Used 1 tool'
        : 'Used ${w.steps.length} tools';
    return Container(
      decoration: BoxDecoration(
        color: c.card,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(12),
      ),
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  title,
                  style: TextStyle(fontSize: 12.5, color: c.muted),
                ),
              ),
              if (widget.onInspect case final inspect?)
                InkWell(
                  borderRadius: BorderRadius.circular(6),
                  onTap: inspect,
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 4,
                      vertical: 2,
                    ),
                    child: Text(
                      'Inspect run',
                      style: TextStyle(fontSize: 12, color: c.muted),
                    ),
                  ),
                ),
            ],
          ),
          const SizedBox(height: 6),
          for (final s in w.steps) ...[
            InkWell(
              borderRadius: BorderRadius.circular(6),
              onTap: s.output.isEmpty
                  ? null
                  : () => setState(
                      () => _open.contains(s.id)
                          ? _open.remove(s.id)
                          : _open.add(s.id),
                    ),
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: 3),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SizedBox(width: 18, child: _StepIcon(s.status)),
                    const SizedBox(width: 6),
                    Text(
                      s.name,
                      style: const TextStyle(fontWeight: FontWeight.w600),
                    ),
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 8),
                      child: Text('→', style: TextStyle(color: c.muted)),
                    ),
                    Expanded(
                      child: Text(
                        _detail(s),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(color: c.muted),
                      ),
                    ),
                    if (s.output.isNotEmpty)
                      Icon(
                        _open.contains(s.id)
                            ? Icons.expand_less
                            : Icons.expand_more,
                        size: 16,
                        color: c.muted,
                      ),
                  ],
                ),
              ),
            ),
            if (_open.contains(s.id)) _Output(s.output),
          ],
        ],
      ),
    );
  }

  /// The summary without the tool name it usually starts with.
  static String _detail(ToolStep s) {
    final t = s.summary.startsWith(s.name)
        ? s.summary.substring(s.name.length).trim()
        : s.summary;
    return t.isEmpty
        ? (s.status == StepStatus.running ? 'running' : 'done')
        : t;
  }

  static String _duration(Duration d) => d.inMinutes >= 1
      ? '${d.inMinutes}m ${d.inSeconds % 60}s'
      : '${d.inSeconds}s';
}

class _StepIcon extends StatelessWidget {
  const _StepIcon(this.status);

  final StepStatus status;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return switch (status) {
      StepStatus.ok => Icon(Icons.check, size: 16, color: c.ok),
      StepStatus.error => Icon(Icons.close, size: 16, color: c.error),
      StepStatus.running => Padding(
        padding: const EdgeInsets.all(3),
        child: SizedBox(
          width: 11,
          height: 11,
          child: CircularProgressIndicator(strokeWidth: 1.6, color: c.muted),
        ),
      ),
    };
  }
}

class _Output extends StatelessWidget {
  const _Output(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      margin: const EdgeInsets.fromLTRB(24, 2, 0, 6),
      constraints: const BoxConstraints(maxHeight: 260),
      width: double.infinity,
      decoration: BoxDecoration(
        color: c.background,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(8),
      ),
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(10),
        child: SelectableText(text, style: mono.copyWith(color: c.text)),
      ),
    );
  }
}

class _Pill extends StatelessWidget {
  const _Pill(this.pill);

  final PillItem pill;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final icon = switch (pill.kind) {
      PillKind.routine => Icons.schedule,
      PillKind.connector => Icons.hexagon_outlined,
      PillKind.memory => Icons.bookmark_border,
      PillKind.compacted => Icons.compress,
      PillKind.retrying => Icons.refresh,
    };
    return Center(
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 14, color: c.muted),
          const SizedBox(width: 6),
          Text(pill.text, style: TextStyle(fontSize: 12.5, color: c.muted)),
        ],
      ),
    );
  }
}

class ApprovalCard extends StatefulWidget {
  const ApprovalCard({super.key, required this.item, required this.store});

  final ApprovalItem item;
  final TernStore store;

  @override
  State<ApprovalCard> createState() => _ApprovalCardState();
}

class _ApprovalCardState extends State<ApprovalCard> {
  var _showInput = false;
  var _busy = false;

  Future<void> _answer(Decision d) async {
    setState(() => _busy = true);
    await widget.store.answer(widget.item, d);
    if (mounted) setState(() => _busy = false);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final a = widget.item.approval;
    final outcome = widget.item.outcome;
    if (outcome != null) {
      final ok = outcome == 'approved' || outcome == 'always allowed';
      return Row(
        children: [
          Icon(
            ok ? Icons.check_circle_outline : Icons.block,
            size: 15,
            color: ok ? c.ok : c.muted,
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              '${outcome[0].toUpperCase()}${outcome.substring(1)}: ${a.summary}',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
          ),
        ],
      );
    }
    final input = const JsonEncoder.withIndent('  ').convert(a.input);
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.enter): () =>
            _answer(Decision.once),
        const SingleActivator(LogicalKeyboardKey.escape): () =>
            _answer(Decision.deny),
      },
      child: Container(
        decoration: BoxDecoration(
          color: c.card,
          border: Border.all(color: c.warn.withValues(alpha: 0.6)),
          borderRadius: BorderRadius.circular(12),
        ),
        padding: const EdgeInsets.all(14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(Icons.front_hand_outlined, size: 16, color: c.warn),
                const SizedBox(width: 8),
                Text(
                  'tern wants to run ${a.tool}',
                  style: const TextStyle(fontWeight: FontWeight.w600),
                ),
                const Spacer(),
                TextButton(
                  onPressed: () => setState(() => _showInput = !_showInput),
                  child: Text(
                    _showInput ? 'Hide input' : 'Show input',
                    style: TextStyle(fontSize: 12, color: c.muted),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 6),
            Container(
              width: double.infinity,
              padding: const EdgeInsets.all(10),
              decoration: BoxDecoration(
                color: c.background,
                borderRadius: BorderRadius.circular(8),
                border: Border.all(color: c.hairline),
              ),
              child: SelectableText(
                _showInput ? input : a.summary,
                style: mono.copyWith(color: c.text),
              ),
            ),
            const SizedBox(height: 12),
            Row(
              children: [
                FilledButton(
                  autofocus: true,
                  onPressed: _busy ? null : () => _answer(Decision.once),
                  style: FilledButton.styleFrom(
                    backgroundColor: c.accent,
                    foregroundColor: c.onAccent,
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                  ),
                  child: const Text('Allow once'),
                ),
                const SizedBox(width: 8),
                Tooltip(
                  message: 'Adds “${a.pattern}” to the allowlist',
                  child: OutlinedButton(
                    onPressed: _busy ? null : () => _answer(Decision.always),
                    style: OutlinedButton.styleFrom(
                      foregroundColor: c.text,
                      side: BorderSide(color: c.hairline),
                      shape: RoundedRectangleBorder(
                        borderRadius: BorderRadius.circular(8),
                      ),
                    ),
                    child: const Text('Always allow'),
                  ),
                ),
                const SizedBox(width: 4),
                TextButton(
                  onPressed: _busy ? null : () => _answer(Decision.deny),
                  style: TextButton.styleFrom(foregroundColor: c.muted),
                  child: const Text('Deny'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

class _RunEnd extends StatelessWidget {
  const _RunEnd(this.end, {required this.store});

  final RunEndItem end;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final failed = end.status == 'failed';
    return Row(
      children: [
        Icon(
          failed ? Icons.error_outline : Icons.pause_circle_outline,
          size: 15,
          color: failed ? c.error : c.muted,
        ),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            failed ? 'Run failed: ${end.error ?? 'unknown error'}' : 'Stopped',
            style: TextStyle(fontSize: 12.5, color: failed ? c.error : c.muted),
          ),
        ),
        if (store.selected case final session?)
          TextButton(
            onPressed: () =>
                showInspector(context, store, session: session, run: end.runId),
            style: TextButton.styleFrom(foregroundColor: c.muted),
            child: const Text('Details'),
          ),
        TextButton(
          onPressed: store.running ? null : () => store.resume(end.runId),
          child: const Text('Resume'),
        ),
      ],
    );
  }
}
