import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

/// The role an agent wrote for itself (agent profiles §1.5): shown in the
/// agent sheet, with Edit (the user's words, fixed from then on) and
/// Re-infer (the agent writes a new one from its name and its work in chats
/// and channels). Re-infer always answers: progress, then the new role, a
/// note when nothing changed, or the error.
class AgentRoleSection extends StatefulWidget {
  const AgentRoleSection({super.key, required this.store, required this.agent});

  final TernStore store;
  final Agent agent;

  @override
  State<AgentRoleSection> createState() => _AgentRoleSectionState();
}

class _AgentRoleSectionState extends State<AgentRoleSection> {
  AgentRole? _role;
  final _text = TextEditingController();
  var _editing = false;
  var _busy = false;
  String? _error;
  String? _info;
  var _inferring = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final r = await widget.store.client.role(widget.agent.id);
      if (mounted) setState(() => _role = r);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  Future<void> _run(Future<AgentRole> Function() call) async {
    setState(() {
      _busy = true;
      _error = null;
      _info = null;
    });
    try {
      final r = await call();
      if (!mounted) return;
      setState(() {
        _role = r;
        _editing = false;
        _busy = false;
      });
    } on TernException catch (e) {
      if (mounted) {
        setState(() {
          _error = e.message;
          _busy = false;
        });
      }
    }
  }

  Future<void> _reinfer() async {
    setState(() {
      _busy = true;
      _inferring = true;
      _error = null;
      _info = null;
    });
    try {
      final r = await widget.store.client.inferRole(widget.agent.id);
      if (!mounted) return;
      setState(() {
        _role = r;
        _info = r.note ?? 'New role written.';
      });
    } on TernException catch (e) {
      if (mounted) {
        setState(() => _error = 'Could not write a role: ${e.message}');
      }
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _inferring = false;
        });
      }
    }
  }

  String _status(AgentRole r) => switch (r.state) {
    'inferring' =>
      r.role.isEmpty
          ? 'It writes this from its name, then refines it from its work.'
          : 'It refines this over its first few messages.',
    'user' => 'Written by you.',
    _ => 'Fixed. It can suggest changes, you decide.',
  };

  @override
  Widget build(BuildContext context) {
    final r = _role;
    // Agents with a hand-written persona have no role of their own.
    if (r == null || !r.managed) return const SizedBox.shrink();
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final id = widget.agent.id;
    return Column(
      key: const Key('agent-role'),
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.only(top: 14, bottom: 4),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  'Role',
                  style: Theme.of(context).textTheme.labelSmall,
                ),
              ),
              if (!_editing) ...[
                TextButton(
                  key: const Key('role-edit'),
                  onPressed: _busy
                      ? null
                      : () => setState(() {
                          _text.text = r.role;
                          _editing = true;
                        }),
                  child: const Text('Edit'),
                ),
                TextButton(
                  key: const Key('role-reinfer'),
                  onPressed: _busy ? null : _reinfer,
                  child: const Text('Re-infer'),
                ),
              ],
            ],
          ),
        ),
        if (_editing) ...[
          TextField(
            key: const Key('role-field'),
            controller: _text,
            minLines: 3,
            maxLines: 8,
            style: const TextStyle(fontSize: 13),
            decoration: InputDecoration(
              isDense: true,
              border: OutlineInputBorder(
                borderRadius: BorderRadius.circular(8),
              ),
              hintText: 'What it is for, how it works, what it won\'t do.',
            ),
          ),
          Align(
            alignment: Alignment.centerRight,
            child: Wrap(
              spacing: 8,
              children: [
                TextButton(
                  onPressed: _busy
                      ? null
                      : () => setState(() => _editing = false),
                  child: const Text('Cancel'),
                ),
                FilledButton(
                  key: const Key('role-save'),
                  onPressed: _busy || _text.text.trim().isEmpty
                      ? null
                      : () => _run(
                          () => widget.store.client.setRole(id, _text.text),
                        ),
                  child: const Text('Save role'),
                ),
              ],
            ),
          ),
        ] else
          Container(
            width: double.infinity,
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              color: c.sidebar,
              borderRadius: BorderRadius.circular(8),
            ),
            child: SelectableText(
              r.role.isEmpty ? 'Not written yet.' : r.role,
              key: const Key('role-text'),
              style: TextStyle(
                fontSize: 13,
                color: r.role.isEmpty ? c.muted : null,
              ),
            ),
          ),
        Padding(
          padding: const EdgeInsets.only(top: 4, left: 2),
          child: _inferring
              ? Row(
                  key: const Key('role-progress'),
                  children: [
                    const SizedBox(
                      width: 12,
                      height: 12,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    ),
                    const SizedBox(width: 8),
                    Text('Writing a role from its work…', style: small),
                  ],
                )
              : Text(
                  _error ?? _info ?? _status(r),
                  key: const Key('role-status'),
                  style: small,
                ),
        ),
      ],
    );
  }
}
