import 'dart:async';

import 'package:flutter/material.dart';

import '../state/connection.dart';
import '../theme.dart';
import 'widgets.dart';
import 'where_to_run.dart';

/// Several saved servers (docs/gui-spec.md, "Servers"): "This Mac" is
/// always there; others are added with a pairing link, each with its own
/// name, address and Keychain token. The sidebar footer's switcher changes
/// the one in use; this file holds the switcher menu, the manage dialog and
/// the add/edit dialog.

Color reachColor(TernColors c, Reach r) => switch (r) {
  Reach.online => c.ok,
  Reach.checking => c.muted,
  Reach.offline => c.warn,
  Reach.signedOut => c.error,
  Reach.unusable => c.error,
};

/// "Reachable", "Not reachable", … for a server's status.
String reachWord(ServerStatus s) => switch (s.reach) {
  Reach.online => 'Reachable${s.version == null ? '' : ' · tern ${s.version}'}',
  Reach.checking => 'Checking…',
  Reach.offline => 'Not reachable',
  Reach.signedOut => 'Signed out',
  Reach.unusable => "Can't be used",
};

class ReachDot extends StatelessWidget {
  const ReachDot(this.reach, {super.key});

  final Reach reach;

  @override
  Widget build(BuildContext context) => Container(
    width: 8,
    height: 8,
    decoration: BoxDecoration(
      color: reachColor(context.colors, reach),
      shape: BoxShape.circle,
    ),
  );
}

/// Switch to [id], telling the user why when it can't be done.
Future<bool> switchServer(
  BuildContext context,
  Connections connections,
  String id,
) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  try {
    await connections.switchTo(id);
    return true;
  } on Exception catch (e) {
    messenger?.showSnackBar(SnackBar(content: Text(serverErrorText(e))));
    return false;
  }
}

/// The footer's switcher: the server in use and how it's doing, opening a
/// menu to switch between the saved ones.
class ServerSwitcher extends StatelessWidget {
  const ServerSwitcher({
    super.key,
    required this.connections,
    required this.statusColor,
    required this.statusText,
  });

  final Connections connections;
  final Color statusColor;
  final String statusText;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final name = connections.nameOf(connections.activeId);
    return InkWell(
      key: const Key('server-switcher'),
      borderRadius: BorderRadius.circular(6),
      onTap: () => showServerMenu(context, connections),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 3, horizontal: 2),
        child: Row(
          children: [
            Container(
              width: 8,
              height: 8,
              decoration: BoxDecoration(
                color: statusColor,
                shape: BoxShape.circle,
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    name,
                    key: const Key('server-name'),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: 12,
                      fontWeight: FontWeight.w600,
                      color: c.ink,
                    ),
                  ),
                  Text(
                    statusText,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: small,
                  ),
                ],
              ),
            ),
            Icon(Icons.unfold_more, size: 16, color: c.muted),
          ],
        ),
      ),
    );
  }
}

/// The switcher's menu: every server with how it's doing, then Add and
/// Manage.
Future<void> showServerMenu(
  BuildContext context,
  Connections connections,
) async {
  final box = context.findRenderObject() as RenderBox;
  final overlay =
      Navigator.of(context).overlay!.context.findRenderObject() as RenderBox;
  final origin = box.localToGlobal(Offset.zero, ancestor: overlay);
  final position = RelativeRect.fromRect(
    origin & box.size,
    Offset.zero & overlay.size,
  );
  final profiles = <Profile>[const LocalProfile(), ...connections.servers];
  final picked = await showMenu<String>(
    context: context,
    position: position,
    constraints: const BoxConstraints(minWidth: 250, maxWidth: 320),
    items: [
      for (final p in profiles)
        PopupMenuItem<String>(
          key: Key('server-row-${p.id}'),
          value: 'sw:${p.id}',
          height: 44,
          child: _MenuRow(
            profile: p,
            name: connections.nameOf(p.id),
            active: p.id == connections.activeId,
            connections: connections,
          ),
        ),
      const PopupMenuDivider(),
      const PopupMenuItem<String>(
        key: Key('server-add'),
        value: 'add',
        height: 36,
        child: Text('Add a server…', style: TextStyle(fontSize: 13)),
      ),
      const PopupMenuItem<String>(
        key: Key('server-manage'),
        value: 'manage',
        height: 36,
        child: Text('Manage servers…', style: TextStyle(fontSize: 13)),
      ),
    ],
  );
  if (picked == null || !context.mounted) return;
  switch (picked) {
    case 'add':
      await showDialog<bool>(
        context: context,
        builder: (_) => ServerEditDialog(connections: connections),
      );
    case 'manage':
      await showServersDialog(context, connections);
    case final s when s.startsWith('sw:'):
      final id = s.substring(3);
      if (id != connections.activeId) {
        await switchServer(context, connections, id);
      }
  }
}

class _MenuRow extends StatefulWidget {
  const _MenuRow({
    required this.profile,
    required this.name,
    required this.active,
    required this.connections,
  });

  final Profile profile;
  final String name;
  final bool active;
  final Connections connections;

  @override
  State<_MenuRow> createState() => _MenuRowState();
}

class _MenuRowState extends State<_MenuRow> {
  var _status = ServerStatus.checking;

  @override
  void initState() {
    super.initState();
    unawaited(_ask());
  }

  Future<void> _ask() async {
    final s = await widget.connections.status(widget.profile);
    if (mounted) setState(() => _status = s);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Row(
      children: [
        ReachDot(_status.reach),
        const SizedBox(width: 10),
        Expanded(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                widget.name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(fontSize: 13),
              ),
              Text(
                reachWord(_status),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 11, color: c.muted),
              ),
            ],
          ),
        ),
        if (widget.active) Icon(Icons.check, size: 16, color: c.accent),
      ],
    );
  }
}

Future<void> showServersDialog(BuildContext context, Connections connections) =>
    showDialog<void>(
      context: context,
      builder: (_) => ServersDialog(connections: connections),
    );

/// Settings › Servers: the saved servers, each with its status, and ways to
/// switch, add, rename, edit (address, pairing) and remove. This computer is
/// built in and stays.
class ServersDialog extends StatefulWidget {
  const ServersDialog({super.key, required this.connections});

  final Connections connections;

  @override
  State<ServersDialog> createState() => _ServersDialogState();
}

class _ServersDialogState extends State<ServersDialog> {
  final _status = <String, ServerStatus>{};
  String? _error;
  var _busy = false;

  Connections get _c => widget.connections;

  @override
  void initState() {
    super.initState();
    unawaited(_refresh());
  }

  Future<void> _refresh() async {
    final profiles = <Profile>[const LocalProfile(), ..._c.servers];
    setState(() {
      for (final p in profiles) {
        _status[p.id] = ServerStatus.checking;
      }
    });
    await Future.wait([
      for (final p in profiles)
        _c.status(p).then((s) {
          if (mounted) setState(() => _status[p.id] = s);
        }),
    ]);
  }

  Future<void> _switch(String id) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await _c.switchTo(id);
      // The app now shows that server: this dialog belonged to the last.
      if (mounted) Navigator.pop(context);
    } on Exception catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = serverErrorText(e);
        });
      }
    }
  }

  Future<void> _edit(RemoteProfile? p) async {
    final before = _c.activeId;
    final saved = await showDialog<bool>(
      context: context,
      builder: (_) => ServerEditDialog(connections: _c, editing: p),
    );
    if (!mounted) return;
    if (saved == true && _c.activeId != before) {
      Navigator.pop(context);
    } else if (saved == true) {
      unawaited(_refresh());
    }
  }

  Future<void> _remove(RemoteProfile p) async {
    final wasActive = _c.activeId == p.id;
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Remove ${p.name}?'),
        content: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 400),
          child: Text(
            'This app forgets the server and its key'
            '${wasActive ? ' and goes back to ${localName.toLowerCase()}' : ''}. '
            'Nothing on the server changes: to stop it accepting this device '
            'too, revoke the device there with `tern devices`.',
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            key: const Key('confirm-remove-server'),
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (ok != true || !mounted) return;
    try {
      await _c.remove(p.id);
      if (!mounted) return;
      if (wasActive) {
        Navigator.pop(context);
      } else {
        setState(() => _status.remove(p.id));
      }
    } on Exception catch (e) {
      if (mounted) setState(() => _error = serverErrorText(e));
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final profiles = <Profile>[const LocalProfile(), ..._c.servers];
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 560, maxHeight: 640),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 14),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  const Expanded(
                    child: Text(
                      'Servers',
                      style: TextStyle(
                        fontSize: 17,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  QuietIconButton(
                    icon: Icons.refresh,
                    tooltip: 'Check them again',
                    onPressed: _busy ? null : _refresh,
                  ),
                ],
              ),
              const SizedBox(height: 4),
              Text(
                'Where tern runs. The app shows one at a time; switching '
                'changes everything you see to that server, and nothing is '
                'shared between them. Each keeps its own key in the Keychain.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 12),
              Flexible(
                child: SingleChildScrollView(
                  child: Column(
                    children: [
                      for (final p in profiles) ...[
                        const Hairline(),
                        _ServerRow(
                          key: Key('server-${p.id}'),
                          name: _c.nameOf(p.id),
                          detail: switch (p) {
                            LocalProfile() =>
                              'Built in · tern running on this computer',
                            RemoteProfile r => r.url.toString(),
                          },
                          status: _status[p.id] ?? ServerStatus.checking,
                          active: p.id == _c.activeId,
                          onSwitch: _busy ? null : () => _switch(p.id),
                          onEdit: p is RemoteProfile && !_busy
                              ? () => _edit(p)
                              : null,
                          onRemove: p is RemoteProfile && !_busy
                              ? () => _remove(p)
                              : null,
                        ),
                      ],
                    ],
                  ),
                ),
              ),
              if (_error case final e?) ...[
                const SizedBox(height: 8),
                Text(e, style: TextStyle(fontSize: 12.5, color: c.error)),
              ],
              const SizedBox(height: 10),
              Row(
                children: [
                  TextButton.icon(
                    key: const Key('add-server'),
                    onPressed: _busy ? null : () => _edit(null),
                    icon: Icon(Icons.add, size: 16, color: c.muted),
                    label: Text(
                      'Add a server',
                      style: TextStyle(color: c.muted),
                    ),
                  ),
                  const Spacer(),
                  FilledButton(
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
}

class _ServerRow extends StatelessWidget {
  const _ServerRow({
    super.key,
    required this.name,
    required this.detail,
    required this.status,
    required this.active,
    required this.onSwitch,
    required this.onEdit,
    required this.onRemove,
  });

  final String name;
  final String detail;
  final ServerStatus status;
  final bool active;
  final VoidCallback? onSwitch;
  final VoidCallback? onEdit;
  final VoidCallback? onRemove;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final quiet =
        status.reach == Reach.online || status.reach == Reach.checking;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 9),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 5),
            child: ReachDot(status.reach),
          ),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Flexible(
                      child: Text(
                        name,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          fontSize: 13.5,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ),
                    if (active) ...[
                      const SizedBox(width: 8),
                      Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 7,
                          vertical: 2,
                        ),
                        decoration: BoxDecoration(
                          color: c.selection,
                          borderRadius: BorderRadius.circular(8),
                        ),
                        child: Text(
                          'In use',
                          style: TextStyle(
                            fontSize: 10.5,
                            fontWeight: FontWeight.w600,
                            color: c.ink,
                          ),
                        ),
                      ),
                    ],
                  ],
                ),
                Text(
                  detail,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: mono.copyWith(fontSize: 11.5, color: c.muted),
                ),
                Text(
                  status.message ?? reachWord(status),
                  style: small?.copyWith(
                    color: quiet ? c.muted : reachColor(c, status.reach),
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(width: 8),
          if (!active)
            OutlinedButton(
              key: const Key('switch-server'),
              onPressed: onSwitch,
              child: const Text('Switch'),
            ),
          if (onEdit != null)
            QuietIconButton(
              icon: Icons.edit_outlined,
              size: 16,
              tooltip: 'Rename, change address or pair again',
              onPressed: onEdit,
            ),
          if (onRemove != null)
            QuietIconButton(
              icon: Icons.delete_outline,
              size: 16,
              tooltip: 'Remove from this app',
              onPressed: onRemove,
            ),
        ],
      ),
    );
  }
}

/// Add a server (a pairing link, or its address plus a pairing code), or
/// edit one: rename it, or pair it again, at the same address or a new one.
/// "Test connection" asks the address whether tern answers, sending no
/// token, before anything is saved. Pops `true` when something was saved.
class ServerEditDialog extends StatefulWidget {
  const ServerEditDialog({super.key, required this.connections, this.editing});

  final Connections connections;

  /// Null when adding.
  final RemoteProfile? editing;

  @override
  State<ServerEditDialog> createState() => _ServerEditDialogState();
}

class _ServerEditDialogState extends State<ServerEditDialog> {
  late final _name = TextEditingController(text: widget.editing?.name);
  final _fields = ServerFields();
  var _pairAgain = false;
  var _switchNow = true;
  var _busy = false;
  var _testing = false;
  String? _error;
  ({bool ok, String text})? _test;

  Connections get _c => widget.connections;
  RemoteProfile? get _editing => widget.editing;

  /// Whether the address form is showing: always when adding.
  bool get _form => _editing == null || _pairAgain;

  @override
  void initState() {
    super.initState();
    _name.addListener(() => setState(() {}));
    _fields.input.addListener(() => setState(() => _test = null));
  }

  @override
  void dispose() {
    _name.dispose();
    _fields.dispose();
    super.dispose();
  }

  Future<void> _testConnection() async {
    setState(() {
      _testing = true;
      _test = null;
      _error = null;
    });
    ({bool ok, String text}) result;
    try {
      if (_form) {
        final a = _fields.address();
        await _c.test(a.url, pin: a.pin);
        result = (
          ok: true,
          text:
              'Found a tern server at ${a.url.host}'
              '${a.pin == null ? '' : ', and its certificate matches the link'}.',
        );
      } else {
        final s = await _c.status(_editing!);
        result = s.reach == Reach.online
            ? (
                ok: true,
                text:
                    'Connected to ${_editing!.name}'
                    '${s.version == null ? '' : ' (tern ${s.version})'}.',
              )
            : (ok: false, text: s.message ?? reachWord(s));
      }
    } on Exception catch (e) {
      result = (ok: false, text: serverErrorText(e));
    }
    if (mounted) {
      setState(() {
        _testing = false;
        _test = result;
      });
    }
  }

  Future<void> _save() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final e = _editing;
      if (e == null) {
        await _c.connect(
          _fields.link(),
          name: _name.text,
          activate: _switchNow,
        );
      } else if (_pairAgain) {
        await _c.repair(e.id, _fields.link(), name: _name.text);
      } else if (_name.text.trim() != e.name) {
        await _c.rename(e.id, _name.text);
      }
      if (mounted) Navigator.pop(context, true);
    } on Exception catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = serverErrorText(e);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final e = _editing;
    final changed =
        e == null || _pairAgain || _name.text.trim() != e.name.trim();
    final canTest =
        !_busy && !_testing && (!_form || _fields.input.text.trim().isNotEmpty);
    return AlertDialog(
      title: Text(e == null ? 'Add a server' : 'Edit ${e.name}'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460, minWidth: 420),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                key: const Key('server-name-field'),
                controller: _name,
                enabled: !_busy,
                autofocus: e != null,
                decoration: InputDecoration(
                  labelText: 'Name',
                  hintText: e == null ? 'Home server (optional)' : null,
                ),
              ),
              const SizedBox(height: 12),
              if (e != null && !_pairAgain) ...[
                Text('Address', style: small),
                const SizedBox(height: 2),
                SelectableText(
                  e.url.toString(),
                  style: mono.copyWith(fontSize: 12.5),
                ),
                const SizedBox(height: 6),
                Align(
                  alignment: Alignment.centerLeft,
                  child: TextButton(
                    key: const Key('pair-again'),
                    onPressed: _busy
                        ? null
                        : () => setState(() {
                            _pairAgain = true;
                            _test = null;
                            _fields.input.text = e.url.toString();
                          }),
                    child: const Text('Pair again or change address…'),
                  ),
                ),
              ] else ...[
                ServerForm(
                  fields: _fields,
                  enabled: !_busy,
                  autofocus: e == null,
                ),
                if (e != null) ...[
                  const SizedBox(height: 6),
                  Text(
                    'Pairing again signs this device in with a new key. The '
                    'old key is forgotten only after the new one works, and '
                    'is never sent to a different address.',
                    style: small,
                  ),
                ],
              ],
              const SizedBox(height: 10),
              Row(
                children: [
                  OutlinedButton(
                    key: const Key('test-connection'),
                    onPressed: canTest ? _testConnection : null,
                    child: Text(_testing ? 'Testing…' : 'Test connection'),
                  ),
                  const SizedBox(width: 10),
                  if (_test case final t?)
                    Expanded(
                      child: Text(
                        t.text,
                        key: const Key('test-result'),
                        style: TextStyle(
                          fontSize: 12.5,
                          color: t.ok ? c.ok : c.error,
                        ),
                      ),
                    ),
                ],
              ),
              if (e == null)
                CheckboxListTile(
                  key: const Key('switch-now'),
                  contentPadding: EdgeInsets.zero,
                  dense: true,
                  controlAffinity: ListTileControlAffinity.leading,
                  value: _switchNow,
                  onChanged: _busy
                      ? null
                      : (v) => setState(() => _switchNow = v ?? false),
                  title: const Text('Switch to it now'),
                ),
              if (_error case final err?) ...[
                const SizedBox(height: 8),
                Text(
                  err,
                  key: const Key('server-error'),
                  style: TextStyle(fontSize: 12.5, color: c.error),
                ),
              ],
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.pop(context, false),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('save-server'),
          onPressed: _busy || !changed ? null : _save,
          child: Text(
            _busy
                ? 'Connecting…'
                : e == null
                ? 'Add server'
                : _pairAgain
                ? 'Pair again'
                : 'Save',
          ),
        ),
      ],
    );
  }
}
