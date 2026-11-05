import 'dart:async';

import 'package:flutter/material.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'inspector.dart';
import 'items.dart';
import 'widgets.dart';

/// The sub-agents one run started: a line each with what it's doing, which
/// opens into its own conversation.
class AgentsCard extends StatefulWidget {
  const AgentsCard({super.key, required this.agents, required this.store});

  final AgentsItem agents;
  final TernStore store;

  @override
  State<AgentsCard> createState() => _AgentsCardState();
}

class _AgentsCardState extends State<AgentsCard> {
  final _open = <String>{};
  Timer? _tick;

  @override
  void initState() {
    super.initState();
    // Elapsed times move while anyone is still working.
    _tick = Timer.periodic(const Duration(seconds: 1), (_) {
      if (widget.agents.children.any((c) => c.active)) setState(() {});
    });
  }

  @override
  void dispose() {
    _tick?.cancel();
    super.dispose();
  }

  void _toggle(AgentChild child) {
    setState(() {
      if (!_open.remove(child.callId)) _open.add(child.callId);
    });
    // History knows only the brief; fetch what it did.
    final session = child.sessionId;
    if (session != null &&
        _open.contains(child.callId) &&
        (widget.store.transcripts[session]?.items.isEmpty ?? true)) {
      unawaited(widget.store.loadChild(session));
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final kids = widget.agents.children;
    final starts = kids.map((k) => k.started).nonNulls;
    final first = starts.isEmpty
        ? null
        : starts.reduce((a, b) => a.isBefore(b) ? a : b);
    final ends = kids.map((k) => k.finished).nonNulls;
    final last = kids.any((k) => k.active) || ends.isEmpty
        ? DateTime.now()
        : ends.reduce((a, b) => a.isAfter(b) ? a : b);
    final n = kids.length;
    return Container(
      decoration: BoxDecoration(
        color: c.card,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(12),
      ),
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            'Delegated to ${n == 1 ? '1 agent' : '$n agents'}'
            '${first == null ? '' : ' · ${formatDuration(last.difference(first))}'}',
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
          const SizedBox(height: 4),
          for (final k in kids) ...[
            _AgentRow(
              child: k,
              store: widget.store,
              open: _open.contains(k.callId),
              onTap: () => _toggle(k),
            ),
            if (_open.contains(k.callId))
              _AgentDetail(child: k, store: widget.store),
          ],
        ],
      ),
    );
  }
}

class _AgentRow extends StatelessWidget {
  const _AgentRow({
    required this.child,
    required this.store,
    required this.open,
    required this.onTap,
  });

  final AgentChild child;
  final TernStore store;
  final bool open;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final started = child.started;
    final elapsed = started == null
        ? null
        : (child.finished ?? DateTime.now()).difference(started);
    return InkWell(
      borderRadius: BorderRadius.circular(6),
      onTap: onTap,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 4),
        child: Row(
          children: [
            SizedBox(width: 18, child: AgentGlyph(child.status)),
            const SizedBox(width: 6),
            // Delegated to another of the user's agents: who it is.
            if (store.agentNamed(child.agent) case final a?) ...[
              AgentAvatar.of(a, size: 16),
              const SizedBox(width: 6),
              Text(
                a.name,
                style: const TextStyle(
                  fontSize: 12.5,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(width: 6),
            ],
            Text(
              child.label,
              style: mono.copyWith(fontSize: 12.5, fontWeight: FontWeight.w600),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                agentActivity(child, store),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  fontSize: 12.5,
                  color: child.status == 'question' ? c.warn : c.muted,
                ),
              ),
            ),
            if (elapsed != null)
              Padding(
                padding: const EdgeInsets.only(left: 8),
                child: Text(
                  formatDuration(elapsed),
                  style: TextStyle(fontSize: 11.5, color: c.muted),
                ),
              ),
            Icon(
              open ? Icons.expand_less : Icons.expand_more,
              size: 16,
              color: c.muted,
            ),
          ],
        ),
      ),
    );
  }
}

/// What a sub-agent is up to, in a few words.
String agentActivity(AgentChild child, TernStore store) {
  final run = child.runId;
  if (run != null &&
      store.transcripts.values.any(
        (t) => t.items.any(
          (i) =>
              i is ApprovalItem && i.outcome == null && i.approval.runId == run,
        ),
      )) {
    return 'Waiting for your approval';
  }
  if (run != null &&
      store.transcripts.values.any(
        (t) => t.items.any(
          (i) => i is SecretRequestItem && i.open && i.request.runId == run,
        ),
      )) {
    return 'Waiting for a secret';
  }
  String firstLine(String s) => s.trim().split('\n').first;
  return switch (child.status) {
    'starting' => 'Starting',
    'queued' => 'Queued',
    'question' => 'Asked: ${firstLine(child.question ?? '')}',
    'completed' => firstLine(child.result ?? 'Done'),
    'running' => store.transcripts[child.sessionId]?.phase?.label ?? 'Working',
    final other =>
      child.error != null
          ? '${other[0].toUpperCase()}${other.substring(1)}: ${firstLine(child.error!)}'
          : '${other[0].toUpperCase()}${other.substring(1)}',
  };
}

class AgentGlyph extends StatelessWidget {
  const AgentGlyph(this.status, {super.key});

  final String status;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return switch (status) {
      'starting' || 'running' => Padding(
        padding: const EdgeInsets.all(3),
        child: SizedBox(
          width: 11,
          height: 11,
          child: CircularProgressIndicator(strokeWidth: 1.6, color: c.muted),
        ),
      ),
      'completed' => Icon(Icons.check, size: 16, color: c.ok),
      'question' => Icon(Icons.help_outline, size: 15, color: c.warn),
      'queued' => Icon(Icons.pause_circle_outline, size: 15, color: c.muted),
      'failed' => Icon(Icons.close, size: 16, color: c.error),
      _ => Icon(Icons.stop_circle_outlined, size: 15, color: c.muted),
    };
  }
}

/// A sub-agent opened up: its brief, then its own conversation.
class _AgentDetail extends StatefulWidget {
  const _AgentDetail({required this.child, required this.store});

  final AgentChild child;
  final TernStore store;

  @override
  State<_AgentDetail> createState() => _AgentDetailState();
}

class _AgentDetailState extends State<_AgentDetail> {
  var _fullBrief = false;

  Future<void> _message() async {
    final run = widget.child.runId;
    if (run == null) return;
    await showDialog<void>(
      context: context,
      builder: (_) => _MessageDialog(
        label: widget.child.label,
        send: (text) => widget.store.messageRun(run, text),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final k = widget.child;
    final t = k.sessionId == null
        ? null
        : widget.store.transcripts[k.sessionId];
    // The brief is the first message; don't show it twice.
    final items = groupApprovals([
      ...?t?.items.skipWhile((i) => i is UserItem && i.text == k.task),
    ]);
    final phase = t?.phase;
    return Container(
      margin: const EdgeInsets.fromLTRB(24, 2, 0, 8),
      padding: const EdgeInsets.fromLTRB(12, 10, 12, 6),
      decoration: BoxDecoration(
        color: c.background,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (k.task.isNotEmpty)
            GestureDetector(
              onTap: () => setState(() => _fullBrief = !_fullBrief),
              child: Text(
                k.task,
                maxLines: _fullBrief ? null : 3,
                overflow: _fullBrief ? null : TextOverflow.ellipsis,
                style: TextStyle(fontSize: 12.5, color: c.muted),
              ),
            ),
          if (k.model case final model?)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                model,
                style: mono.copyWith(fontSize: 11, color: c.muted),
              ),
            ),
          if (items.isNotEmpty || phase != null) ...[
            const SizedBox(height: 8),
            ConstrainedBox(
              constraints: const BoxConstraints(maxHeight: 480),
              child: ListView(
                shrinkWrap: true,
                reverse: true,
                children: [
                  for (final i in [if (phase != null) null, ...items.reversed])
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 4),
                      child: i == null
                          ? ActivityLine(phase: phase!, since: t!.runSince)
                          : ItemView(item: i, store: widget.store),
                    ),
                ],
              ),
            ),
          ],
          Row(
            children: [
              if (k.active && k.runId != null) ...[
                _Action('Stop', () => widget.store.cancelRun(k.runId!)),
                _Action('Message…', _message),
              ],
              if (k.runId case final run?)
                if (widget.store.selected case final session?)
                  _Action(
                    'Inspect run',
                    () => showInspector(
                      context,
                      widget.store,
                      session: session,
                      run: run,
                    ),
                  ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Action extends StatelessWidget {
  const _Action(this.label, this.onTap);

  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => TextButton(
    onPressed: onTap,
    style: TextButton.styleFrom(
      foregroundColor: context.colors.muted,
      padding: const EdgeInsets.symmetric(horizontal: 6),
      minimumSize: const Size(0, 30),
    ),
    child: Text(label, style: const TextStyle(fontSize: 12)),
  );
}

class _MessageDialog extends StatefulWidget {
  const _MessageDialog({required this.label, required this.send});

  final String label;
  final Future<String?> Function(String) send;

  @override
  State<_MessageDialog> createState() => _MessageDialogState();
}

class _MessageDialogState extends State<_MessageDialog> {
  final _text = TextEditingController();
  String? _error;
  var _busy = false;

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _send() async {
    final text = _text.text.trim();
    if (text.isEmpty) return;
    setState(() => _busy = true);
    final err = await widget.send(text);
    if (!mounted) return;
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text('Message ${widget.label}'),
    content: SizedBox(
      width: 420,
      child: TextField(
        controller: _text,
        autofocus: true,
        minLines: 2,
        maxLines: 6,
        decoration: InputDecoration(
          hintText: 'It gets this before its next step',
          errorText: _error,
          errorMaxLines: 3,
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Cancel'),
      ),
      FilledButton(onPressed: _busy ? null : _send, child: const Text('Send')),
    ],
  );
}
