import 'package:flutter/material.dart';

import '../state/store.dart';
import '../theme.dart';
import 'channel_view.dart' show slugFor;

/// "Move to channel…": take conversations (a topic from Home, side chats,
/// threads, History entries) into a topic channel, existing or new
/// (DECISIONS.md "T2 moves").
Future<void> showMoveDialog(
  BuildContext context,
  TernStore store,
  List<String> sessions, {
  String? what,
}) => showDialog<void>(
  context: context,
  builder: (_) => MoveDialog(store: store, sessions: sessions, what: what),
);

class MoveDialog extends StatefulWidget {
  const MoveDialog({
    super.key,
    required this.store,
    required this.sessions,
    this.what,
  });

  final TernStore store;
  final List<String> sessions;

  /// What is being moved, in words ("this topic").
  final String? what;

  @override
  State<MoveDialog> createState() => _MoveDialogState();
}

class _MoveDialogState extends State<MoveDialog> {
  /// A topic channel's id, or [_new].
  late String _target = widget.store.topics.firstOrNull?.id ?? _new;
  static const _new = '';
  final _title = TextEditingController();
  String? _error;
  var _busy = false;

  @override
  void initState() {
    super.initState();
    _title.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _title.dispose();
    super.dispose();
  }

  bool get _ok =>
      !_busy && (_target != _new || slugFor(_title.text).isNotEmpty);

  Future<void> _move() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.store.moveToChannel(
      widget.sessions,
      channelId: _target == _new ? null : _target,
      newTitle: _target == _new ? _title.text.trim() : null,
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
    final n = widget.sessions.length;
    final what =
        widget.what ?? (n == 1 ? 'this conversation' : '$n conversations');
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 440, maxHeight: 560),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text(
                'Move to channel',
                style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 4),
              Text(
                'Moves $what, history and all, into a channel as '
                '${n == 1 ? 'a thread' : 'threads'}. Where '
                '${n == 1 ? 'it was' : 'they were'}, a note says where '
                '${n == 1 ? 'it' : 'they'} went.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 12),
              Flexible(
                child: RadioGroup<String>(
                  groupValue: _target,
                  onChanged: (v) => setState(() => _target = v ?? _new),
                  child: ListView(
                    shrinkWrap: true,
                    children: [
                      for (final ch in widget.store.topics)
                        RadioListTile<String>(
                          value: ch.id,
                          dense: true,
                          title: Text('#${ch.slug}'),
                          subtitle: ch.title.toLowerCase() == ch.slug
                              ? null
                              : Text(ch.title),
                        ),
                      const RadioListTile<String>(
                        value: _new,
                        dense: true,
                        title: Text('New channel'),
                      ),
                    ],
                  ),
                ),
              ),
              if (_target == _new)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 4, 0, 0),
                  child: TextField(
                    controller: _title,
                    autofocus: true,
                    decoration: InputDecoration(
                      hintText: 'Name, e.g. Launch',
                      isDense: true,
                      border: OutlineInputBorder(
                        borderRadius: BorderRadius.circular(8),
                      ),
                      helperText: slugFor(_title.text).isEmpty
                          ? null
                          : '#${slugFor(_title.text)}',
                    ),
                  ),
                ),
              if (_error case final e?) ...[
                const SizedBox(height: 10),
                Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
              ],
              const SizedBox(height: 16),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  TextButton(
                    onPressed: () => Navigator.of(context).pop(),
                    child: const Text('Cancel'),
                  ),
                  const SizedBox(width: 8),
                  FilledButton(
                    onPressed: _ok ? _move : null,
                    child: const Text('Move'),
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
