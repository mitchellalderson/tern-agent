import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'underline_tabs.dart';

/// MEMORY in the side panel (agent profiles §1.3, gui-spec §4.3): what the
/// open agent remembers. Scope chips pick its own memories or the shared
/// ones; in Home, the shared ones or a private agent's. Tap a memory to
/// rewrite or delete it; Clear empties the scope. A note saved after the
/// agent read untrusted content is marked "unverified": the agent recalls it
/// as a lead to check, and rewriting it makes it yours.
class MemorySection extends StatefulWidget {
  const MemorySection({super.key, required this.store});

  final TernStore store;

  @override
  State<MemorySection> createState() => _MemorySectionState();
}

class _MemorySectionState extends State<MemorySection> {
  /// `shared` or an agent id; null = the open agent's default chip.
  String? _scope;
  String? _loadedFor;
  List<Memory>? _memories;
  String? _error;
  bool _showAll = false;

  /// The chips: the agent's own first when its memory is private, then
  /// Shared, and in Home one per private agent.
  List<({String id, String label})> _chips(Agent? agent) {
    final store = widget.store;
    if (agent != null && agent.privateMemory && !agent.isDefault) {
      return [
        (id: agent.id, label: agent.name),
        (id: 'shared', label: 'Shared'),
      ];
    }
    return [
      (id: 'shared', label: 'Shared'),
      if (agent?.isDefault ?? true)
        for (final a in store.otherAgents)
          if (a.privateMemory) (id: a.id, label: a.name),
    ];
  }

  Future<void> _load(String scope) async {
    _loadedFor = scope;
    try {
      final m = await widget.store.client.memories(agent: scope);
      if (!mounted || _loadedFor != scope) return;
      setState(() {
        _memories = m;
        _error = null;
      });
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  /// Run a change, then show what is stored now.
  Future<void> _change(String scope, Future<void> Function() go) async {
    try {
      await go();
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
      return;
    }
    if (mounted) await _load(scope);
  }

  Future<void> _edit(String scope, Memory m) async {
    final controller = TextEditingController(text: m.content);
    final action = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(m.unverified ? 'Unverified memory' : 'Memory'),
        content: SizedBox(
          width: 420,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (m.unverified)
                const Padding(
                  padding: EdgeInsets.only(bottom: 8),
                  child: Text(
                    'Saved after the agent read untrusted content. It recalls '
                    'this as a lead to check, not a fact. Saving your own '
                    'wording makes it yours.',
                    style: TextStyle(fontSize: 12.5),
                  ),
                ),
              TextField(
                key: const Key('memory-text'),
                controller: controller,
                minLines: 2,
                maxLines: 8,
                autofocus: true,
              ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, 'delete'),
            child: const Text('Delete'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, 'save'),
            child: const Text('Save'),
          ),
        ],
      ),
    );
    final text = controller.text.trim();
    if (!mounted) return;
    switch (action) {
      case 'save' when text.isNotEmpty && text != m.content:
        await _change(scope, () => widget.store.client.editMemory(m.id, text));
      case 'delete':
        await _change(scope, () => widget.store.client.deleteMemory(m.id));
    }
  }

  Future<void> _clear(String scope, String label) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Clear $label memory?'),
        content: const Text(
          'Every memory in this list is deleted. The ledger keeps a record '
          'that you did it.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Clear'),
          ),
        ],
      ),
    );
    if (ok != true || !mounted) return;
    await _change(scope, () => widget.store.client.clearMemories(scope));
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final agent = widget.store.streamAgent ?? widget.store.defaultAgent;
    final chips = _chips(agent);
    final scope = chips.any((x) => x.id == _scope) ? _scope! : chips.first.id;
    if (scope != _loadedFor) {
      _memories = null;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted && _loadedFor != scope) _load(scope);
      });
      _loadedFor = null;
    }
    final memories = _memories;
    final label = chips.firstWhere((x) => x.id == scope).label;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                'MEMORY',
                style: Theme.of(context).textTheme.labelSmall,
              ),
            ),
            if (memories != null && memories.isNotEmpty)
              TextButton(
                key: const Key('memory-clear'),
                style: TextButton.styleFrom(
                  visualDensity: VisualDensity.compact,
                  padding: const EdgeInsets.symmetric(horizontal: 6),
                  minimumSize: const Size(0, 24),
                ),
                onPressed: () => _clear(scope, label),
                child: const Text('Clear', style: TextStyle(fontSize: 11.5)),
              ),
          ],
        ),
        const SizedBox(height: 6),
        if (chips.length > 1)
          UnderlineTabs(
            spacing: 12,
            tabs: [
              for (final chip in chips)
                UnderlineTab(
                  label: chip.label,
                  fontSize: 12.5,
                  selected: chip.id == scope,
                  onTap: () => setState(() => _scope = chip.id),
                ),
            ],
          ),
        const SizedBox(height: 6),
        if (_error case final e?)
          Text(e, style: TextStyle(fontSize: 12, color: c.error))
        else if (memories == null)
          Text('Loading…', style: Theme.of(context).textTheme.bodySmall)
        else if (memories.isEmpty)
          Text(
            scope == 'shared'
                ? 'Nothing saved yet.'
                : 'Nothing of its own yet.',
            style: Theme.of(context).textTheme.bodySmall,
          )
        else
          for (final m in memories.take(_showAll ? memories.length : 8))
            InkWell(
              key: Key('memory-${m.id}'),
              onTap: () => _edit(scope, m),
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: 3),
                child: Text.rich(
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  TextSpan(
                    children: [
                      if (m.unverified)
                        TextSpan(
                          text: 'unverified  ',
                          style: TextStyle(
                            fontSize: 11,
                            fontWeight: FontWeight.w600,
                            color: c.warn,
                          ),
                        ),
                      TextSpan(
                        text: m.content,
                        style: const TextStyle(fontSize: 12.5),
                      ),
                    ],
                  ),
                ),
              ),
            ),
        if (memories != null && memories.length > 8)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton(
              style: TextButton.styleFrom(
                visualDensity: VisualDensity.compact,
                padding: const EdgeInsets.symmetric(horizontal: 6),
                minimumSize: const Size(0, 24),
              ),
              onPressed: () => setState(() => _showAll = !_showAll),
              child: Text(
                _showAll ? 'Show fewer' : 'Show all ${memories.length}',
                style: const TextStyle(fontSize: 11.5),
              ),
            ),
          ),
      ],
    );
  }
}
