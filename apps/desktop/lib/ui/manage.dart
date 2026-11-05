// Editing and deleting what you create (DECISIONS.md "Editing and
// deleting"): the confirmation for deleting an agent or a channel, which
// first asks the daemon what it touches, and the Archived list to restore
// or delete from.
import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

Future<void> showDeleteAgent(
  BuildContext context,
  TernStore store,
  Agent agent,
) => showDialog<void>(
  context: context,
  builder: (_) => DeleteAgentDialog(store: store, agent: agent),
);

Future<void> showDeleteChannel(
  BuildContext context,
  TernStore store,
  Channel channel,
) => showDialog<void>(
  context: context,
  builder: (_) => DeleteChannelDialog(store: store, channel: channel),
);

Future<void> showArchived(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => ArchivedDialog(store: store),
    );

String _n(int n, String one, [String? many]) =>
    '$n ${n == 1 ? one : many ?? '${one}s'}';

Widget _line(BuildContext context, String text, {bool bad = false}) => Padding(
  padding: const EdgeInsets.only(bottom: 4),
  child: Row(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Text(
        bad ? '!  ' : '•  ',
        style: TextStyle(color: bad ? context.colors.error : null),
      ),
      Expanded(
        child: Text(
          text,
          style: TextStyle(
            fontSize: 13,
            color: bad ? context.colors.error : null,
          ),
        ),
      ),
    ],
  ),
);

/// "Delete Research?": what goes, what stays, where its routines go.
class DeleteAgentDialog extends StatefulWidget {
  const DeleteAgentDialog({
    super.key,
    required this.store,
    required this.agent,
  });

  final TernStore store;
  final Agent agent;

  @override
  State<DeleteAgentDialog> createState() => _DeleteAgentDialogState();
}

class _DeleteAgentDialogState extends State<DeleteAgentDialog> {
  AgentImpact? _impact;
  String? _error;
  var _busy = false;

  /// Hand its routines to `_to` (an agent id), or delete them.
  var _handOver = true;
  late String _to = Agent.defaultId;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final i = await widget.store.client.agentImpact(widget.agent.id);
      if (mounted) setState(() => _impact = i);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  List<Agent> get _others => [
    for (final a in widget.store.agents)
      if (a.id != widget.agent.id && !a.archived) a,
  ];

  Future<void> _delete() async {
    final i = _impact;
    if (i == null) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    final hasRoutines = i.routines.isNotEmpty;
    final err = await widget.store.deleteAgent(
      widget.agent,
      reassignTo: hasRoutines && _handOver ? _to : null,
      deleteRoutines: hasRoutines && !_handOver,
    );
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
  Widget build(BuildContext context) {
    final c = context.colors;
    final a = widget.agent;
    final i = _impact;
    return AlertDialog(
      title: Text('Delete ${a.name}?'),
      content: SizedBox(
        width: 440,
        child: i == null
            ? Padding(
                padding: const EdgeInsets.symmetric(vertical: 12),
                child: _error == null
                    ? const Center(child: CircularProgressIndicator())
                    : Text(_error!, style: TextStyle(color: c.error)),
              )
            : SingleChildScrollView(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'This can\'t be undone.',
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                    const SizedBox(height: 10),
                    _line(
                      context,
                      'Its ${_n(i.conversations, 'conversation')} stay in '
                      'History under its name.',
                    ),
                    if (i.memories > 0)
                      _line(
                        context,
                        'Its ${_n(i.memories, 'private memory', 'private memories')} '
                        'will be deleted.',
                      ),
                    if (i.memberChannels.isNotEmpty)
                      _line(
                        context,
                        'It leaves ${i.memberChannels.map((x) => '#${x.label}').join(', ')}.',
                      ),
                    if (i.ledChannels.isNotEmpty)
                      _line(
                        context,
                        'It leads ${i.ledChannels.map((x) => '#${x.label}').join(', ')}: '
                        'the lead moves to ${_handOver && i.routines.isNotEmpty ? _nameOf(_to) : 'tern'}.',
                      ),
                    if (i.telegramBot case final secret?)
                      _line(
                        context,
                        'Its own Telegram bot stops. The token secret '
                        '$secret stays in your secrets.',
                      ),
                    if (i.bundles.isNotEmpty)
                      _line(
                        context,
                        'Bundle ${i.bundles.join(', ')} no longer tracks it; '
                        'installing again makes a new one.',
                      ),
                    if (i.routines.isNotEmpty) ...[
                      const SizedBox(height: 8),
                      Text(
                        'ITS ${_n(i.routines.length, 'ROUTINE')}',
                        style: Theme.of(context).textTheme.labelSmall,
                      ),
                      for (final r in i.routines)
                        _line(
                          context,
                          r.label.isEmpty ? '(no prompt)' : r.label,
                        ),
                      const SizedBox(height: 4),
                      Wrap(
                        spacing: 8,
                        children: [
                          ChoiceChip(
                            key: const Key('routines-hand-over'),
                            label: const Text('Hand them to another agent'),
                            selected: _handOver,
                            onSelected: (_) => setState(() => _handOver = true),
                          ),
                          ChoiceChip(
                            key: const Key('routines-delete'),
                            label: const Text('Delete them'),
                            selected: !_handOver,
                            onSelected: (_) =>
                                setState(() => _handOver = false),
                          ),
                        ],
                      ),
                      if (_handOver) ...[
                        const SizedBox(height: 8),
                        DropdownButtonFormField<String>(
                          key: const Key('routines-to'),
                          initialValue: _to,
                          isDense: true,
                          decoration: InputDecoration(
                            isDense: true,
                            border: OutlineInputBorder(
                              borderRadius: BorderRadius.circular(8),
                            ),
                          ),
                          items: [
                            for (final o in _others)
                              DropdownMenuItem(
                                value: o.id,
                                child: Text(o.name),
                              ),
                          ],
                          onChanged: (v) => setState(() => _to = v ?? _to),
                        ),
                      ],
                    ],
                    if (i.blockers.isNotEmpty) ...[
                      const SizedBox(height: 10),
                      for (final b in i.blockers) _line(context, b, bad: true),
                    ],
                    if (_error case final e?) ...[
                      const SizedBox(height: 8),
                      Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
                    ],
                  ],
                ),
              ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('confirm-delete'),
          style: FilledButton.styleFrom(backgroundColor: c.error),
          onPressed: _busy || i == null || i.blockers.isNotEmpty
              ? null
              : _delete,
          child: Text('Delete ${a.name}'),
        ),
      ],
    );
  }

  String _nameOf(String id) => widget.store.agentById(id)?.name ?? id;
}

/// "Delete #launch?": its posts go, its conversations stay.
class DeleteChannelDialog extends StatefulWidget {
  const DeleteChannelDialog({
    super.key,
    required this.store,
    required this.channel,
  });

  final TernStore store;
  final Channel channel;

  @override
  State<DeleteChannelDialog> createState() => _DeleteChannelDialogState();
}

class _DeleteChannelDialogState extends State<DeleteChannelDialog> {
  ChannelImpact? _impact;
  String? _error;
  var _busy = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final i = await widget.store.client.channelImpact(widget.channel.id);
      if (mounted) setState(() => _impact = i);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _delete() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.store.deleteChannel(widget.channel.id);
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
  Widget build(BuildContext context) {
    final c = context.colors;
    final ch = widget.channel;
    final i = _impact;
    return AlertDialog(
      title: Text('Delete #${ch.slug}?'),
      content: SizedBox(
        width: 420,
        child: i == null
            ? Padding(
                padding: const EdgeInsets.symmetric(vertical: 12),
                child: _error == null
                    ? const Center(child: CircularProgressIndicator())
                    : Text(_error!, style: TextStyle(color: c.error)),
              )
            : Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    'This can\'t be undone.',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  const SizedBox(height: 10),
                  _line(context, '${_n(i.posts, 'post')} will be deleted.'),
                  _line(
                    context,
                    'Its ${_n(i.conversations, 'conversation')} stay in '
                    'History.',
                  ),
                  if (i.routines.isNotEmpty)
                    _line(
                      context,
                      '${_n(i.routines.length, 'routine')} post here '
                      '(${i.routines.map((r) => r.label).join('; ')}): they '
                      'will post to their agent\'s chat instead.',
                    ),
                  for (final b in i.blockers) _line(context, b, bad: true),
                  if (_error case final e?) ...[
                    const SizedBox(height: 8),
                    Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
                  ],
                ],
              ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('confirm-delete'),
          style: FilledButton.styleFrom(backgroundColor: c.error),
          onPressed: _busy || i == null || i.blockers.isNotEmpty
              ? null
              : _delete,
          child: Text('Delete #${ch.slug}'),
        ),
      ],
    );
  }
}

/// Archived agents and channels: bring one back, or delete it.
class ArchivedDialog extends StatefulWidget {
  const ArchivedDialog({super.key, required this.store});

  final TernStore store;

  @override
  State<ArchivedDialog> createState() => _ArchivedDialogState();
}

class _ArchivedDialogState extends State<ArchivedDialog> {
  List<Agent>? _agents;
  List<Channel>? _channels;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final r = await widget.store.archivedItems();
      if (!mounted) return;
      setState(() {
        _agents = r.agents;
        _channels = r.channels;
      });
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _do(Future<String?> Function() act) async {
    final err = await act();
    if (!mounted) return;
    setState(() => _error = err);
    await _load();
  }

  Widget _row(
    String label, {
    required VoidCallback onRestore,
    required VoidCallback onDelete,
    required Key key,
  }) => Row(
    key: key,
    children: [
      Expanded(child: Text(label, overflow: TextOverflow.ellipsis)),
      TextButton(onPressed: onRestore, child: const Text('Restore')),
      TextButton(
        onPressed: onDelete,
        child: Text('Delete…', style: TextStyle(color: context.colors.error)),
      ),
    ],
  );

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final agents = _agents;
    final channels = _channels;
    return AlertDialog(
      title: const Text('Archived'),
      content: SizedBox(
        width: 440,
        child: agents == null || channels == null
            ? Padding(
                padding: const EdgeInsets.symmetric(vertical: 12),
                child: _error == null
                    ? const Center(child: CircularProgressIndicator())
                    : Text(_error!, style: TextStyle(color: c.error)),
              )
            : SingleChildScrollView(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (agents.isEmpty && channels.isEmpty)
                      Text(
                        'Nothing is archived. Archiving hides an agent or '
                        'channel and keeps everything.',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    if (agents.isNotEmpty)
                      Text(
                        'AGENTS',
                        style: Theme.of(context).textTheme.labelSmall,
                      ),
                    for (final a in agents)
                      _row(
                        a.name,
                        key: Key('archived-agent-${a.slug}'),
                        onRestore: () =>
                            _do(() => widget.store.unarchiveAgent(a)),
                        onDelete: () async {
                          await showDeleteAgent(context, widget.store, a);
                          if (mounted) await _load();
                        },
                      ),
                    if (channels.isNotEmpty) ...[
                      const SizedBox(height: 8),
                      Text(
                        'CHANNELS',
                        style: Theme.of(context).textTheme.labelSmall,
                      ),
                    ],
                    for (final ch in channels)
                      _row(
                        '#${ch.slug}',
                        key: Key('archived-channel-${ch.slug}'),
                        onRestore: () =>
                            _do(() => widget.store.unarchiveChannel(ch.id)),
                        onDelete: () async {
                          await showDeleteChannel(context, widget.store, ch);
                          if (mounted) await _load();
                        },
                      ),
                    if (_error case final e?) ...[
                      const SizedBox(height: 8),
                      Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
                    ],
                  ],
                ),
              ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Close'),
        ),
      ],
    );
  }
}
