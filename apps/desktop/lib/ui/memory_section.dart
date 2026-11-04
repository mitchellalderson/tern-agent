import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

/// MEMORY in the side panel (agent profiles §1.3, gui-spec §4.3): what the
/// open agent remembers. Scope chips pick its own memories or the shared
/// ones; in Home, the shared ones or a private agent's.
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
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text('MEMORY', style: Theme.of(context).textTheme.labelSmall),
        const SizedBox(height: 6),
        if (chips.length > 1)
          Wrap(
            spacing: 4,
            runSpacing: 4,
            children: [
              for (final chip in chips)
                ChoiceChip(
                  label: Text(
                    chip.label,
                    style: const TextStyle(fontSize: 11.5),
                  ),
                  selected: chip.id == scope,
                  visualDensity: VisualDensity.compact,
                  onSelected: (_) => setState(() => _scope = chip.id),
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
          for (final m in memories.take(8))
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 3),
              child: Text(
                m.content,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(fontSize: 12.5),
              ),
            ),
      ],
    );
  }
}
