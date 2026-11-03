import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

Future<void> showSecretsDialog(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => SecretsDialog(client: store.client),
    );

/// API keys and tokens in `secrets.toml`. Values are write-only: the app
/// sends them to the daemon and never reads them back.
class SecretsDialog extends StatefulWidget {
  const SecretsDialog({super.key, required this.client});

  final TernClient client;

  @override
  State<SecretsDialog> createState() => _SecretsDialogState();
}

class _SecretsDialogState extends State<SecretsDialog> {
  List<SecretStatus>? _secrets;
  String? _error;

  /// The secret whose value is being entered; `''` while adding a new one.
  String? _editing;
  var _busy = false;
  final _name = TextEditingController();
  final _value = TextEditingController();

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _name.dispose();
    _value.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final list = await widget.client.secrets();
      if (mounted) setState(() => _secrets = list);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  void _edit(String? name) => setState(() {
    _editing = name;
    _error = null;
    _name.clear();
    _value.clear();
  });

  String? get _nameError {
    final n = _name.text.trim();
    if (_editing != '' || n.isEmpty || secretName.hasMatch(n)) return null;
    return 'Letters, digits and underscores, not starting with a digit';
  }

  bool get _canSave =>
      !_busy &&
      _value.text.trim().isNotEmpty &&
      (_editing != '' || (_name.text.trim().isNotEmpty && _nameError == null));

  Future<void> _save() async {
    final name = _editing == '' ? _name.text.trim() : _editing!;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.client.setSecret(name, _value.text);
      _value.clear();
      await _load();
      if (mounted) setState(() => _editing = null);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _remove(SecretStatus s) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Remove ${s.name}?'),
        content: Text(
          s.usedBy.isEmpty
              ? 'It will be deleted from secrets.toml.'
              : 'It will be deleted from secrets.toml. '
                    '${s.usedBy.join(', ')} will stop working until you '
                    'set it again.',
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
    if (!(ok ?? false)) return;
    try {
      await widget.client.removeSecret(s.name);
      await _load();
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final secrets = _secrets;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 560, maxHeight: 620),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text(
                'Secrets',
                style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 4),
              Text(
                'API keys and tokens for providers, connectors and MCP '
                'servers. Saved to ~/.tern/secrets.toml, readable only by '
                'you, and used from the next request on. Saved values '
                "can't be shown again.",
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 14),
              Flexible(
                child: secrets == null
                    ? Padding(
                        padding: const EdgeInsets.all(24),
                        child: Center(
                          child: _error == null
                              ? const CircularProgressIndicator(strokeWidth: 2)
                              : Text(_error!, style: TextStyle(color: c.error)),
                        ),
                      )
                    : ListView(
                        shrinkWrap: true,
                        children: [
                          if (secrets.isEmpty && _editing != '')
                            Padding(
                              padding: const EdgeInsets.symmetric(vertical: 12),
                              child: Text(
                                'No secrets yet.',
                                style: Theme.of(context).textTheme.bodySmall,
                              ),
                            ),
                          for (final s in secrets) ...[
                            _SecretRow(
                              secret: s,
                              editing: _editing == s.name,
                              onSet: () => _edit(s.name),
                              onRemove: s.inFile ? () => _remove(s) : null,
                            ),
                            if (_editing == s.name) _editor(context),
                          ],
                          if (_editing == '') _editor(context),
                        ],
                      ),
              ),
              if (_error case final e? when secrets != null) ...[
                const SizedBox(height: 10),
                Text(e, style: TextStyle(fontSize: 12.5, color: c.error)),
              ],
              const SizedBox(height: 14),
              Row(
                children: [
                  if (_editing != '')
                    TextButton.icon(
                      onPressed: secrets == null ? null : () => _edit(''),
                      icon: Icon(Icons.add, size: 16, color: c.muted),
                      label: Text(
                        'Add secret',
                        style: TextStyle(color: c.muted),
                      ),
                    ),
                  const Spacer(),
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Done'),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// Name (when adding) and value fields, under the row being set.
  Widget _editor(BuildContext context) {
    final c = context.colors;
    final adding = _editing == '';
    return Container(
      margin: const EdgeInsets.only(bottom: 8),
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: c.background,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(10),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (adding) ...[
            TextField(
              controller: _name,
              autofocus: true,
              style: mono.copyWith(fontSize: 13),
              onChanged: (_) => setState(() {}),
              decoration: _box(
                context,
                hint: 'NAME, e.g. GITHUB_TOKEN',
                error: _nameError,
              ),
            ),
            const SizedBox(height: 8),
          ],
          TextField(
            controller: _value,
            autofocus: !adding,
            obscureText: true,
            autocorrect: false,
            enableSuggestions: false,
            style: mono.copyWith(fontSize: 13),
            onChanged: (_) => setState(() {}),
            onSubmitted: (_) => _canSave ? _save() : null,
            decoration: _box(context, hint: 'Paste the value'),
          ),
          const SizedBox(height: 10),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              TextButton(
                onPressed: _busy ? null : () => _edit(null),
                child: const Text('Cancel'),
              ),
              const SizedBox(width: 8),
              FilledButton(
                onPressed: _canSave ? _save : null,
                style: FilledButton.styleFrom(
                  backgroundColor: c.accent,
                  foregroundColor: c.onAccent,
                ),
                child: const Text('Save'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _SecretRow extends StatelessWidget {
  const _SecretRow({
    required this.secret,
    required this.editing,
    required this.onSet,
    required this.onRemove,
  });

  final SecretStatus secret;
  final bool editing;
  final VoidCallback onSet;
  final VoidCallback? onRemove;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = secret;
    final (label, color) = switch ((s.inEnv, s.inFile)) {
      (true, true) => ('Environment overrides file', c.warn),
      (true, false) => ('From environment', c.ok),
      (false, true) => ('Set', c.ok),
      (false, false) => ('Missing', c.error),
    };
    final usedBy = s.usedBy.isEmpty ? 'Not used yet' : s.usedBy.join(' · ');
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        children: [
          Icon(Icons.key, size: 16, color: s.isSet ? c.accent : c.muted),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(s.name, style: mono.copyWith(fontSize: 13)),
                const SizedBox(height: 2),
                Text(
                  usedBy,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 11.5, color: c.muted),
                ),
              ],
            ),
          ),
          const SizedBox(width: 8),
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
            decoration: BoxDecoration(
              color: color.withValues(alpha: 0.12),
              borderRadius: BorderRadius.circular(6),
            ),
            child: Text(label, style: TextStyle(fontSize: 11, color: color)),
          ),
          const SizedBox(width: 6),
          TextButton(
            onPressed: editing ? null : onSet,
            child: Text(s.inFile ? 'Replace' : 'Set'),
          ),
          if (onRemove != null)
            IconButton(
              tooltip: 'Remove from secrets.toml',
              icon: Icon(Icons.delete_outline, size: 17, color: c.muted),
              splashRadius: 16,
              visualDensity: VisualDensity.compact,
              onPressed: editing ? null : onRemove,
            )
          else
            const SizedBox(width: 40),
        ],
      ),
    );
  }
}

InputDecoration _box(BuildContext context, {String? hint, String? error}) {
  final c = context.colors;
  OutlineInputBorder border(Color color) => OutlineInputBorder(
    borderRadius: BorderRadius.circular(8),
    borderSide: BorderSide(color: color),
  );
  return InputDecoration(
    hintText: hint,
    hintStyle: TextStyle(color: c.muted),
    errorText: error,
    isDense: true,
    contentPadding: const EdgeInsets.all(10),
    border: border(c.hairline),
    enabledBorder: border(c.hairline),
    focusedBorder: border(c.accent),
  );
}
