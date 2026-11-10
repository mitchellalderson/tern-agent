import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

/// A searchable checklist of agents. Agents in [locked] show checked and
/// disabled, with [lockedNote] beside them. Space toggles the focused row;
/// Down from the search field moves into the list.
class AgentPicker extends StatefulWidget {
  const AgentPicker({
    super.key,
    required this.agents,
    required this.selected,
    required this.onChanged,
    this.locked = const {},
    this.lockedNote = 'In channel',
    this.autofocus = false,
    this.height = 240,
  });

  final List<Agent> agents;
  final Set<String> selected;
  final ValueChanged<Set<String>> onChanged;

  /// Ids already in: checked, disabled, never counted as selected.
  final Set<String> locked;
  final String lockedNote;
  final bool autofocus;
  final double height;

  @override
  State<AgentPicker> createState() => _AgentPickerState();
}

class _AgentPickerState extends State<AgentPicker> {
  final _query = TextEditingController();
  final _list = FocusScopeNode();

  @override
  void initState() {
    super.initState();
    _query.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _query.dispose();
    _list.dispose();
    super.dispose();
  }

  List<Agent> get _visible {
    final q = _query.text.trim().toLowerCase().replaceFirst('@', '');
    return [
      for (final a in widget.agents)
        if (q.isEmpty ||
            a.name.toLowerCase().contains(q) ||
            a.slug.toLowerCase().contains(q))
          a,
    ];
  }

  void _toggle(String id, bool on) {
    final next = {...widget.selected};
    on ? next.add(id) : next.remove(id);
    widget.onChanged(next);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final visible = _visible;
    final choosable = [
      for (final a in visible)
        if (!widget.locked.contains(a.id)) a.id,
    ];
    final allOn =
        choosable.isNotEmpty && choosable.every(widget.selected.contains);
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Focus(
          onKeyEvent: (_, e) {
            if (e is KeyDownEvent &&
                e.logicalKey == LogicalKeyboardKey.arrowDown) {
              // Into the first row, past the buttons.
              FocusScope.of(context).setFirstFocus(_list);
              _list.nextFocus();
              return KeyEventResult.handled;
            }
            return KeyEventResult.ignored;
          },
          child: TextField(
            key: const Key('agent-search'),
            controller: _query,
            autofocus: widget.autofocus,
            decoration: InputDecoration(
              hintText: 'Search agents',
              isDense: true,
              prefixIcon: Icon(Icons.search, size: 18, color: c.muted),
              prefixIconConstraints: const BoxConstraints(minWidth: 28),
              border: UnderlineInputBorder(
                borderSide: BorderSide(color: c.hairline),
              ),
            ),
          ),
        ),
        const SizedBox(height: 4),
        Row(
          children: [
            Text(
              '${widget.selected.length} selected',
              style: TextStyle(fontSize: 12, color: c.muted),
            ),
            const Spacer(),
            TextButton(
              key: const Key('agents-select-all'),
              onPressed: choosable.isEmpty || allOn
                  ? null
                  : () => widget.onChanged({...widget.selected, ...choosable}),
              child: const Text('Select all'),
            ),
            TextButton(
              key: const Key('agents-clear'),
              onPressed: widget.selected.isEmpty
                  ? null
                  : () => widget.onChanged({}),
              child: const Text('Clear'),
            ),
          ],
        ),
        SizedBox(
          height: widget.height,
          child: visible.isEmpty
              ? Center(
                  child: Text(
                    'No agent matches',
                    style: TextStyle(fontSize: 12.5, color: c.muted),
                  ),
                )
              : FocusScope(
                  node: _list,
                  child: ListView(
                    children: [
                      for (final a in visible)
                        _row(a, widget.locked.contains(a.id)),
                    ],
                  ),
                ),
        ),
      ],
    );
  }

  Widget _row(Agent a, bool locked) {
    final c = context.colors;
    return CheckboxListTile(
      key: Key('agent-${a.id}'),
      dense: true,
      controlAffinity: ListTileControlAffinity.leading,
      contentPadding: EdgeInsets.zero,
      value: locked || widget.selected.contains(a.id),
      onChanged: locked ? null : (v) => _toggle(a.id, v ?? false),
      title: Row(
        children: [
          AgentAvatar.of(a, size: 20),
          const SizedBox(width: 8),
          Flexible(child: Text(a.name, overflow: TextOverflow.ellipsis)),
          const SizedBox(width: 6),
          Text('@${a.slug}', style: TextStyle(fontSize: 12, color: c.muted)),
          if (locked) ...[
            const Spacer(),
            Text(
              widget.lockedNote,
              style: TextStyle(fontSize: 11.5, color: c.muted),
            ),
          ],
        ],
      ),
    );
  }
}

/// "+ Agent" in a channel's header: pick several agents to add at once.
Future<void> showAddAgents(
  BuildContext context,
  TernStore store,
  Channel channel,
) => showDialog<void>(
  context: context,
  builder: (_) => AddAgentsDialog(store: store, channel: channel),
);

class AddAgentsDialog extends StatefulWidget {
  const AddAgentsDialog({
    super.key,
    required this.store,
    required this.channel,
  });

  final TernStore store;
  final Channel channel;

  @override
  State<AddAgentsDialog> createState() => _AddAgentsDialogState();
}

class _AddAgentsDialogState extends State<AddAgentsDialog> {
  var _selected = <String>{};
  var _busy = false;
  String? _error;

  Future<void> _add() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await widget.store.addAgentsToChannel(
      widget.channel.id,
      _selected.toList(),
    );
    if (!mounted) return;
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      // Those that joined are members now and show as such.
      setState(() {
        _busy = false;
        _error = err;
        _selected = {
          for (final id in _selected)
            if (!(widget.store.channels
                        .where((x) => x.id == widget.channel.id)
                        .firstOrNull
                        ?.memberIds ??
                    const <String>[])
                .contains(id))
              id,
        };
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final channel =
        widget.store.channels
            .where((x) => x.id == widget.channel.id)
            .firstOrNull ??
        widget.channel;
    final n = _selected.length;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 440),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                'Add agents to #${channel.slug}',
                style: const TextStyle(
                  fontSize: 17,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 14),
              AgentPicker(
                autofocus: true,
                agents: [
                  for (final a in widget.store.agents)
                    if (!a.archived) a,
                ],
                locked: channel.memberIds.toSet(),
                selected: _selected,
                onChanged: (s) => setState(() => _selected = s),
              ),
              if (_error case final e?) ...[
                const SizedBox(height: 8),
                Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
              ],
              const SizedBox(height: 12),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  TextButton(
                    onPressed: () => Navigator.of(context).pop(),
                    child: const Text('Cancel'),
                  ),
                  const SizedBox(width: 8),
                  FilledButton(
                    key: const Key('add-agents'),
                    onPressed: n == 0 || _busy ? null : _add,
                    child: Text(
                      n == 0 ? 'Add' : 'Add $n ${n == 1 ? 'agent' : 'agents'}',
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
