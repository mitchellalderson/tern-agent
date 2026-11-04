import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'mcp.dart' show McpChip;
import 'widgets.dart';

/// Library → Connections: the accounts the agent can use, grouped by the
/// connector or MCP server they belong to. Owner-only.
class ConnectionsView extends StatelessWidget {
  const ConnectionsView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  Widget build(BuildContext context) => Column(
    children: [
      WindowDragArea(
        child: Container(
          height: titleBarInset + 12,
          padding: const EdgeInsets.fromLTRB(12, 0, 10, 0),
          child: Row(
            children: [
              const SizedBox(width: 12),
              const Expanded(
                child: Text(
                  'Connections',
                  style: TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
                ),
              ),
              QuietIconButton(
                icon: panelOpen
                    ? Icons.view_sidebar
                    : Icons.view_sidebar_outlined,
                tooltip: panelOpen ? 'Hide activity' : 'Show activity',
                onPressed: onTogglePanel,
              ),
            ],
          ),
        ),
      ),
      const Hairline(),
      Expanded(child: ConnectionsList(store: store)),
    ],
  );
}

/// What the add dialog asks for.
typedef _AddRequest = ({
  String? name,
  String? label,
  String? method,
  Map<String, String> values,
});

class ConnectionsList extends StatefulWidget {
  const ConnectionsList({super.key, required this.store});

  final TernStore store;

  @override
  State<ConnectionsList> createState() => _ConnectionsListState();
}

class _ConnectionsListState extends State<ConnectionsList> {
  /// What is in flight, e.g. `gmail`; one change at a time.
  String? _busy;
  var _oauth = false;
  String? _error;

  TernStore get _store => widget.store;

  Future<void> _run(
    String what,
    Future<Object?> Function(TernClient c) write, {
    bool oauth = false,
  }) async {
    setState(() {
      _busy = what;
      _oauth = oauth;
      _error = null;
    });
    final err = await _store.connectionsWrite(write);
    if (mounted) {
      setState(() {
        _busy = null;
        _error = err;
      });
    }
  }

  Future<void> _add(ConnectionTarget t) async {
    final r = await showDialog<_AddRequest>(
      context: context,
      builder: (_) => _AddDialog(target: t),
    );
    if (r == null) return;
    await _run(
      t.name,
      (c) => c.addConnection(
        kind: t.kind,
        target: t.name,
        connection: r.name,
        label: r.label,
        method: r.method,
        values: r.values,
      ),
      oauth: r.method != 'key',
    );
  }

  Future<void> _reconnect(Account a, ConnectionTarget? t) async {
    var values = <String, String>{};
    if (a.method == 'key') {
      final r = await showDialog<Map<String, String>>(
        context: context,
        builder: (_) => _KeysDialog(
          title: 'Reconnect ${a.title}',
          names: t?.keyNames ?? const [],
        ),
      );
      if (r == null) return;
      values = r;
    }
    await _run(
      a.target,
      (c) => c.reconnectConnection(a, values: values),
      oauth: a.method == 'oauth',
    );
  }

  Future<void> _rename(Account a) async {
    final label = await showDialog<String>(
      context: context,
      builder: (_) => _RenameDialog(account: a),
    );
    if (label == null) return;
    await _run(a.target, (c) => c.renameConnection(a, label));
  }

  Future<void> _remove(Account a) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Remove ${a.title}?'),
        content: const Text(
          'This signs it out at the service where possible, and deletes '
          'its tokens, keys, cache and grants from tern. Anything using '
          'it will need another account.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    await _run(a.target, (c) => c.removeConnection(a));
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: _store,
    builder: (context, _) {
      final c = context.colors;
      final list = _store.connectionList;
      if (list == null) {
        return const Center(child: CircularProgressIndicator(strokeWidth: 2));
      }
      // Every target, then any account whose target is no longer listed.
      final groups = <(String, String), List<Account>>{
        for (final t in list.targets) (t.kind, t.name): [],
      };
      for (final a in list.accounts) {
        (groups[(a.kind, a.target)] ??= []).add(a);
      }
      return ListView(
        padding: const EdgeInsets.symmetric(vertical: 12),
        children: [
          Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 760),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    if (_busy != null && _oauth)
                      Padding(
                        padding: const EdgeInsets.only(bottom: 10),
                        child: Row(
                          children: [
                            const SizedBox(
                              width: 14,
                              height: 14,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            ),
                            const SizedBox(width: 10),
                            Text(
                              'Finish signing in in your browser',
                              style: TextStyle(fontSize: 13, color: c.muted),
                            ),
                          ],
                        ),
                      ),
                    if (_error case final e?)
                      Padding(
                        padding: const EdgeInsets.only(bottom: 10),
                        child: Text(
                          e,
                          style: TextStyle(fontSize: 12.5, color: c.error),
                        ),
                      ),
                    if (groups.isEmpty)
                      Text(
                        'Nothing to connect to yet.',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                    for (final MapEntry(:key, :value) in groups.entries)
                      _Group(
                        target: list.targets
                            .where((t) => (t.kind, t.name) == key)
                            .firstOrNull,
                        kind: key.$1,
                        name: key.$2,
                        accounts: value,
                        disabled: _busy != null,
                        onAdd: _add,
                        onReconnect: _reconnect,
                        onDefault: (a) =>
                            _run(a.target, (c) => c.makeDefaultConnection(a)),
                        onRename: _rename,
                        onRemove: _remove,
                      ),
                  ],
                ),
              ),
            ),
          ),
        ],
      );
    },
  );
}

class _Group extends StatelessWidget {
  const _Group({
    required this.target,
    required this.kind,
    required this.name,
    required this.accounts,
    required this.disabled,
    required this.onAdd,
    required this.onReconnect,
    required this.onDefault,
    required this.onRename,
    required this.onRemove,
  });

  final ConnectionTarget? target;
  final String kind;
  final String name;
  final List<Account> accounts;
  final bool disabled;
  final void Function(ConnectionTarget) onAdd;
  final void Function(Account, ConnectionTarget?) onReconnect;
  final void Function(Account) onDefault;
  final void Function(Account) onRename;
  final void Function(Account) onRemove;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final t = target;
    return Padding(
      padding: const EdgeInsets.only(bottom: 18),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Text(
                name,
                style: const TextStyle(
                  fontSize: 14,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(width: 8),
              McpChip(kind == 'mcp' ? 'mcp' : 'connector', c.muted),
              const Spacer(),
              if (t != null)
                TextButton.icon(
                  onPressed: disabled ? null : () => onAdd(t),
                  icon: const Icon(Icons.add, size: 15),
                  label: Text(
                    accounts.isEmpty ? 'Add account' : 'Add another account',
                  ),
                ),
            ],
          ),
          const Hairline(),
          if (accounts.isEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Text(
                'Uses its default account',
                style: TextStyle(fontSize: 12.5, color: c.muted),
              ),
            ),
          for (final a in accounts)
            _AccountRow(
              account: a,
              disabled: disabled,
              onReconnect: () => onReconnect(a, t),
              onDefault: () => onDefault(a),
              onRename: () => onRename(a),
              onRemove: () => onRemove(a),
            ),
        ],
      ),
    );
  }
}

class _AccountRow extends StatelessWidget {
  const _AccountRow({
    required this.account,
    required this.disabled,
    required this.onReconnect,
    required this.onDefault,
    required this.onRename,
    required this.onRemove,
  });

  final Account account;
  final bool disabled;
  final VoidCallback onReconnect;
  final VoidCallback onDefault;
  final VoidCallback onRename;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final a = account;
    final style = TextButton.styleFrom(
      foregroundColor: c.muted,
      visualDensity: VisualDensity.compact,
      textStyle: const TextStyle(fontSize: 12.5),
    );
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Wrap(
            spacing: 8,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              Text(
                a.slug,
                style: const TextStyle(
                  fontSize: 13.5,
                  fontWeight: FontWeight.w600,
                ),
              ),
              if (a.label case final l?)
                Text(l, style: const TextStyle(fontSize: 13)),
              if (a.account case final who?)
                Text(who, style: TextStyle(fontSize: 12.5, color: c.muted)),
              if (a.isDefault) McpChip('default', c.accent),
              McpChip(switch (a.method) {
                'oauth' => 'OAuth',
                'key' => 'key',
                _ => 'none',
              }, c.muted),
              if (a.needsReauth)
                McpChip('needs reauth', c.warn)
              else if (a.failing)
                McpChip('error', c.error),
            ],
          ),
          if (a.failing && a.lastError != null)
            Padding(
              padding: const EdgeInsets.only(top: 3),
              child: Text(
                a.lastError!,
                style: TextStyle(
                  fontSize: 12,
                  color: a.needsReauth ? c.warn : c.error,
                ),
              ),
            ),
          Wrap(
            spacing: 2,
            children: [
              TextButton(
                style: style,
                onPressed: disabled ? null : onReconnect,
                child: const Text('Reconnect'),
              ),
              if (!a.isDefault)
                TextButton(
                  style: style,
                  onPressed: disabled ? null : onDefault,
                  child: const Text('Make default'),
                ),
              TextButton(
                style: style,
                onPressed: disabled ? null : onRename,
                child: const Text('Rename'),
              ),
              TextButton(
                style: style,
                onPressed: disabled ? null : onRemove,
                child: const Text('Remove'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Masked fields, one per key name. Values only ever leave in the pop.
class _KeyFields extends StatelessWidget {
  const _KeyFields({required this.names, required this.controllers});

  final List<String> names;
  final Map<String, TextEditingController> controllers;

  @override
  Widget build(BuildContext context) => Column(
    children: [
      for (final n in names)
        Padding(
          padding: const EdgeInsets.only(top: 8),
          child: TextField(
            controller: controllers[n],
            obscureText: true,
            autocorrect: false,
            enableSuggestions: false,
            decoration: InputDecoration(labelText: n, isDense: true),
          ),
        ),
    ],
  );
}

Map<String, String> _entered(Map<String, TextEditingController> cs) => {
  for (final MapEntry(:key, :value) in cs.entries)
    if (value.text.isNotEmpty) key: value.text,
};

class _KeysDialog extends StatefulWidget {
  const _KeysDialog({required this.title, required this.names});

  final String title;
  final List<String> names;

  @override
  State<_KeysDialog> createState() => _KeysDialogState();
}

class _KeysDialogState extends State<_KeysDialog> {
  late final _controllers = {
    for (final n in widget.names) n: TextEditingController(),
  };

  @override
  void dispose() {
    for (final c in _controllers.values) {
      c.dispose();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(widget.title),
    content: SizedBox(
      width: 380,
      child: _KeyFields(names: widget.names, controllers: _controllers),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: () => Navigator.pop(context, _entered(_controllers)),
        child: const Text('Reconnect'),
      ),
    ],
  );
}

class _RenameDialog extends StatefulWidget {
  const _RenameDialog({required this.account});

  final Account account;

  @override
  State<_RenameDialog> createState() => _RenameDialogState();
}

class _RenameDialogState extends State<_RenameDialog> {
  late final _label = TextEditingController(text: widget.account.label);

  @override
  void dispose() {
    _label.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text('Rename ${widget.account.slug}'),
    content: SizedBox(
      width: 380,
      child: TextField(
        controller: _label,
        autofocus: true,
        decoration: const InputDecoration(labelText: 'Label', isDense: true),
        onSubmitted: (v) => Navigator.pop(context, v.trim()),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: () => Navigator.pop(context, _label.text.trim()),
        child: const Text('Save'),
      ),
    ],
  );
}

final _slug = RegExp(r'^[a-z0-9_-]{0,16}$');

class _AddDialog extends StatefulWidget {
  const _AddDialog({required this.target});

  final ConnectionTarget target;

  @override
  State<_AddDialog> createState() => _AddDialogState();
}

class _AddDialogState extends State<_AddDialog> {
  final _name = TextEditingController();
  final _label = TextEditingController();
  late final _keys = {
    for (final n in widget.target.keyNames) n: TextEditingController(),
  };
  late String? _method = widget.target.oauth
      ? 'oauth'
      : widget.target.key
      ? 'key'
      : null;

  @override
  void dispose() {
    _name.dispose();
    _label.dispose();
    for (final c in _keys.values) {
      c.dispose();
    }
    super.dispose();
  }

  bool get _valid => _slug.hasMatch(_name.text.trim());

  void _submit() => Navigator.pop<_AddRequest>(context, (
    name: _name.text.trim().isEmpty ? null : _name.text.trim(),
    label: _label.text.trim().isEmpty ? null : _label.text.trim(),
    method: _method,
    values: _method == 'key' ? _entered(_keys) : const {},
  ));

  @override
  Widget build(BuildContext context) {
    final t = widget.target;
    return AlertDialog(
      title: Text('Add an account to ${t.name}'),
      content: SizedBox(
        width: 380,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                controller: _name,
                onChanged: (_) => setState(() {}),
                decoration: InputDecoration(
                  labelText: 'Connection name (optional)',
                  hintText: 'work',
                  isDense: true,
                  errorText: _valid ? null : 'Up to 16 of a-z, 0-9, _ and -',
                ),
              ),
              const SizedBox(height: 8),
              TextField(
                controller: _label,
                decoration: const InputDecoration(
                  labelText: 'Label (optional)',
                  isDense: true,
                ),
              ),
              if (t.oauth && t.key)
                RadioGroup<String>(
                  groupValue: _method,
                  onChanged: (v) => setState(() => _method = v),
                  child: const Column(
                    children: [
                      RadioListTile<String>(
                        contentPadding: EdgeInsets.zero,
                        value: 'oauth',
                        title: Text('Sign in (OAuth)'),
                      ),
                      RadioListTile<String>(
                        contentPadding: EdgeInsets.zero,
                        value: 'key',
                        title: Text('API key'),
                      ),
                    ],
                  ),
                ),
              if (_method == 'key')
                _KeyFields(names: t.keyNames, controllers: _keys),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _valid ? _submit : null,
          child: Text(_method == 'key' ? 'Add' : 'Continue'),
        ),
      ],
    );
  }
}
