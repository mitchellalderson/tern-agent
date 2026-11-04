import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'agents.dart';
import 'channel_items.dart';
import 'inspector.dart';
import 'move_dialog.dart';

/// One transcript item.
class ItemView extends StatelessWidget {
  const ItemView({
    super.key,
    required this.item,
    required this.store,
    this.session,
    this.branchable = false,
  });

  final Item item;
  final TernStore store;

  /// The conversation the item is in, when it isn't [TernStore.selected]
  /// (the right pane).
  final String? session;

  /// Messages offer "Branch into side chat" (Home's stream).
  final bool branchable;

  @override
  Widget build(BuildContext context) => switch (item) {
    UserItem(:final text, :final queued, :final messageId) => _branch(
      text,
      messageId,
      _UserMessage(text, queued: queued),
      alignEnd: true,
    ),
    AssistantItem(:final text, :final streaming, :final messageId) => _branch(
      text,
      messageId,
      _AssistantMessage(text, streaming: streaming),
      enabled: !streaming,
    ),
    final WorkItem w => WorkCard(
      work: w,
      onInspect: switch ((session ?? store.selected, w.runId)) {
        (final String session, final String run) => () => showInspector(
          context,
          store,
          session: session,
          run: run,
        ),
        _ => null,
      },
    ),
    final AgentsItem a => AgentsCard(agents: a, store: store),
    final PillItem p => _Pill(p),
    final ApprovalItem a => ApprovalCard(item: a, store: store),
    final SecretRequestItem r => SecretRequestCard(item: r, store: store),
    final RunEndItem r => _RunEnd(r, store: store),
    final CutItem c => CutDivider(
      item: c,
      onMove: () =>
          showMoveDialog(context, store, [c.cut.sessionId], what: 'this topic'),
    ),
    PostItem(:final post) when post.kind == 'moved' => MovedMarker(
      post: post,
      store: store,
    ),
    PostItem(:final post) => PostCard(post: post, store: store),
    SideChatItem(:final chat) => SideChatCard(chat: chat, store: store),
  };

  Widget _branch(
    String text,
    int? messageId,
    Widget child, {
    bool alignEnd = false,
    bool enabled = true,
  }) => branchable && enabled
      ? Branchable(
          text: text,
          messageId: messageId,
          store: store,
          alignEnd: alignEnd,
          child: child,
        )
      : child;
}

class _UserMessage extends StatelessWidget {
  const _UserMessage(this.text, {this.queued = false});

  final String text;

  /// Sent while a run worked: it reads it at its next step.
  final bool queued;

  @override
  Widget build(BuildContext context) {
    final bubble = Container(
      constraints: const BoxConstraints(maxWidth: 560),
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 9),
      decoration: BoxDecoration(
        color: context.colors.userBubble,
        borderRadius: BorderRadius.circular(16),
      ),
      child: SelectableText(text),
    );
    return Align(
      alignment: Alignment.centerRight,
      child: queued
          ? Column(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                bubble,
                Padding(
                  padding: const EdgeInsets.only(top: 3, right: 6),
                  child: Text(
                    'Sent while tern was working · read at its next step',
                    style: TextStyle(
                      fontSize: 11.5,
                      color: context.colors.muted,
                    ),
                  ),
                ),
              ],
            )
          : bubble,
    );
  }
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
        ? 'Worked for ${formatDuration(elapsed)}'
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
            if (_open.contains(s.id))
              _Output(s.output)
            else if (s.status == StepStatus.running && s.output.isNotEmpty)
              _Tail(s.output),
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
        // Pinned to the end, so live output stays in view as it grows.
        reverse: true,
        padding: const EdgeInsets.all(10),
        child: SelectableText(text, style: mono.copyWith(color: c.text)),
      ),
    );
  }
}

/// The last few lines of a running tool's output, under its step.
class _Tail extends StatelessWidget {
  const _Tail(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    final lines = text.trimRight().split('\n');
    final tail = lines.skip(lines.length > 4 ? lines.length - 4 : 0);
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 0, 0, 4),
      child: Text(
        tail.join('\n'),
        maxLines: 4,
        overflow: TextOverflow.ellipsis,
        style: mono.copyWith(fontSize: 11.5, color: context.colors.muted),
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
      PillKind.posts => Icons.dynamic_feed,
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
      final ok = const {
        'approved',
        'always allowed',
        'approved_for_run',
        'allowed all sites for this run',
        'site always allowed',
      }.contains(outcome);
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
              outcome == 'deferred'
                  ? 'Nobody answered, so it waits in the outbox: ${a.summary}'
                  : '${outcome[0].toUpperCase()}${outcome.substring(1)}: ${a.summary}',
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
                  '${widget.store.askerName(a)} wants to run ${a.tool}',
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
            if (a.taintNote case final note?) ...[
              const SizedBox(height: 4),
              Row(
                children: [
                  Icon(Icons.shield_outlined, size: 13, color: c.warn),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      'Asking because $note',
                      style: TextStyle(fontSize: 12.5, color: c.warn),
                    ),
                  ),
                ],
              ),
            ],
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
                _showInput ? input : a.headline,
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
                if (a.pattern case final pattern?) ...[
                  const SizedBox(width: 8),
                  Tooltip(
                    message: 'Adds “$pattern” to the allowlist',
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
                ],
                const SizedBox(width: 4),
                TextButton(
                  onPressed: _busy ? null : () => _answer(Decision.deny),
                  style: TextButton.styleFrom(foregroundColor: c.muted),
                  child: const Text('Deny'),
                ),
              ],
            ),
            // The tainted host limit: say yes once for many sites.
            if (a.egressHost case final host?) ...[
              const SizedBox(height: 6),
              Wrap(
                spacing: 8,
                children: [
                  TextButton(
                    onPressed: _busy ? null : () => _answer(Decision.run),
                    child: const Text('Allow all sites for this run'),
                  ),
                  Tooltip(
                    message:
                        'Reads from $host won\'t ask again; remove it with `tern grants`',
                    child: TextButton(
                      onPressed: _busy ? null : () => _answer(Decision.host),
                      child: Text('Always allow $host'),
                    ),
                  ),
                ],
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// The agent asking for secrets. Mirrors [ApprovalCard]. Values exist only
/// in the text controllers until sent; nothing here logs, stores or
/// re-displays them.
class SecretRequestCard extends StatefulWidget {
  const SecretRequestCard({super.key, required this.item, required this.store});

  final SecretRequestItem item;
  final TernStore store;

  @override
  State<SecretRequestCard> createState() => _SecretRequestCardState();
}

class _SecretRequestCardState extends State<SecretRequestCard> {
  final _controllers = <String, TextEditingController>{};
  final _shown = <String>{};
  var _busy = false;
  var _disposedFields = false;

  TextEditingController _field(String name) =>
      _controllers.putIfAbsent(name, TextEditingController.new);

  void _disposeFields() {
    if (_disposedFields) return;
    _disposedFields = true;
    for (final c in _controllers.values) {
      c
        ..clear()
        ..dispose();
    }
    _controllers.clear();
  }

  @override
  void dispose() {
    _disposeFields();
    super.dispose();
  }

  Future<void> _send({required bool skip}) async {
    final values = <String, String>{};
    if (!skip) {
      for (final e in _controllers.entries) {
        if (e.value.text.isNotEmpty) values[e.key] = e.value.text;
      }
    }
    setState(() => _busy = true);
    await widget.store.answerSecret(widget.item, values);
    if (mounted) setState(() => _busy = false);
  }

  Future<void> _paste(String name) async {
    // The clipboard is read only here, on the click.
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    if (data?.text case final text? when mounted) {
      setState(() => _field(name).text = text.trim());
    }
  }

  String _state(SecretAsk s) {
    if (s.inEnv) {
      return 'set in the environment, which wins over a saved value';
    }
    if (s.inFile) return 'saved — this replaces it';
    return 'not set';
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final item = widget.item;
    final r = item.request;
    final names = r.secrets.map((s) => s.name).toList();
    if (!item.open) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _disposeFields());
      final (icon, color, text) = switch (item.outcome) {
        'saved' => (
          Icons.check_circle_outline,
          c.ok,
          '${item.saved.join(', ')} saved ✓',
        ),
        'skipped' => (Icons.block, c.muted, '${names.join(', ')} skipped'),
        _ when item.result.isNotEmpty => (
          Icons.check_circle_outline,
          c.muted,
          item.result.entries.map((e) => '${e.key}: ${e.value}').join(', '),
        ),
        _ => (Icons.block, c.muted, '${names.join(', ')} not entered'),
      };
      return Row(
        children: [
          Icon(icon, size: 15, color: color),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              text,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
          ),
        ],
      );
    }
    final who = r.agentLabel == null
        ? 'The agent'
        : 'Sub-agent ${r.agentLabel}';
    final title =
        '$who is asking for ${names.length == 1 ? 'a secret' : 'secrets'}';
    return Container(
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
              Icon(Icons.key_outlined, size: 16, color: c.warn),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  title,
                  style: const TextStyle(fontWeight: FontWeight.w600),
                ),
              ),
            ],
          ),
          const SizedBox(height: 6),
          // Labelled, so the agent's text can't pass as tern's own.
          Text(
            'The agent says: “${r.reason}”',
            style: TextStyle(fontSize: 13, color: c.muted),
          ),
          for (final s in r.secrets) ...[
            const SizedBox(height: 12),
            Text(s.name, style: mono.copyWith(color: c.text)),
            const SizedBox(height: 2),
            Text(_state(s), style: TextStyle(fontSize: 12, color: c.muted)),
            Text(
              s.usedBy.isEmpty
                  ? 'Nothing uses this secret yet.'
                  : 'Used by ${s.usedBy.join('; ')}',
              style: TextStyle(fontSize: 12, color: c.muted),
            ),
            if (!item.readOnly) ...[
              const SizedBox(height: 6),
              Row(
                children: [
                  Expanded(
                    child: TextField(
                      key: ValueKey('secret-${s.name}'),
                      controller: _field(s.name),
                      obscureText: !_shown.contains(s.name),
                      enableSuggestions: false,
                      autocorrect: false,
                      enableIMEPersonalizedLearning: false,
                      style: mono.copyWith(color: c.text),
                      decoration: InputDecoration(
                        isDense: true,
                        hintText: 'Value',
                        border: OutlineInputBorder(
                          borderRadius: BorderRadius.circular(8),
                        ),
                      ),
                    ),
                  ),
                  const SizedBox(width: 6),
                  TextButton(
                    onPressed: () => _paste(s.name),
                    child: const Text('Paste'),
                  ),
                  TextButton(
                    onPressed: () => setState(
                      () => _shown.contains(s.name)
                          ? _shown.remove(s.name)
                          : _shown.add(s.name),
                    ),
                    child: Text(_shown.contains(s.name) ? 'Hide' : 'Show'),
                  ),
                ],
              ),
            ],
          ],
          const SizedBox(height: 12),
          if (item.readOnly)
            Text(
              'Enter this on an owner device',
              style: TextStyle(fontSize: 12.5, color: c.warn),
            )
          else
            Row(
              children: [
                FilledButton(
                  onPressed: _busy ? null : () => _send(skip: false),
                  style: FilledButton.styleFrom(
                    backgroundColor: c.accent,
                    foregroundColor: c.onAccent,
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(8),
                    ),
                  ),
                  child: const Text('Save'),
                ),
                const SizedBox(width: 4),
                TextButton(
                  onPressed: _busy ? null : () => _send(skip: true),
                  style: TextButton.styleFrom(foregroundColor: c.muted),
                  child: const Text('Skip'),
                ),
              ],
            ),
        ],
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

/// The line under the newest item while a run is going: what it's doing
/// and for how long, so a quiet stretch doesn't look like a hang.
class ActivityLine extends StatefulWidget {
  const ActivityLine({super.key, required this.phase, this.since});

  final RunPhase phase;
  final DateTime? since;

  @override
  State<ActivityLine> createState() => _ActivityLineState();
}

class _ActivityLineState extends State<ActivityLine>
    with SingleTickerProviderStateMixin {
  late final _pulse = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1100),
  )..repeat(reverse: true);
  late final Timer _tick;

  @override
  void initState() {
    super.initState();
    _tick = Timer.periodic(const Duration(seconds: 1), (_) => setState(() {}));
  }

  @override
  void dispose() {
    _tick.cancel();
    _pulse.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final since = widget.since;
    final elapsed = since == null ? null : DateTime.now().difference(since);
    return Padding(
      padding: const EdgeInsets.only(left: 2),
      child: Row(
        children: [
          FadeTransition(
            opacity: Tween(begin: 0.25, end: 1.0).animate(_pulse),
            child: Container(
              width: 8,
              height: 8,
              decoration: BoxDecoration(
                color: c.accent,
                shape: BoxShape.circle,
              ),
            ),
          ),
          const SizedBox(width: 10),
          Text(
            '${widget.phase.label}…',
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
          if (elapsed != null && elapsed.inSeconds >= 1)
            Text(
              ' · ${formatDuration(elapsed)}',
              style: TextStyle(fontSize: 12.5, color: c.muted),
            ),
        ],
      ),
    );
  }
}

String formatDuration(Duration d) => d.inMinutes >= 1
    ? '${d.inMinutes}m ${d.inSeconds % 60}s'
    : '${d.inSeconds}s';
