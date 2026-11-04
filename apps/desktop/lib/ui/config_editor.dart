import 'dart:io';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

Future<void> showConfigDialog(
  BuildContext context,
  TernStore store, {
  Future<void> Function()? onRestart,
}) => showDialog<void>(
  context: context,
  builder: (_) => ConfigDialog(
    client: store.client,
    canReveal: store.client.endpoint is! RemoteEndpoint,
    onRestart: onRestart,
  ),
);

/// Every setting in `config.toml`, by section, with its default, a reset,
/// and what needs a restart. The daemon describes the settings, so new
/// ones show up here without changes. Providers, MCP servers and secrets
/// have their own editors.
class ConfigDialog extends StatefulWidget {
  const ConfigDialog({
    super.key,
    required this.client,
    this.canReveal = true,
    this.onRestart,
    this.reveal = revealFile,
  });

  final TernClient client;

  /// `config.toml` is on this computer, so it can be shown in Finder.
  final bool canReveal;

  /// Restarts tern; without it the restart hint only explains.
  final Future<void> Function()? onRestart;
  final Future<void> Function(String path) reveal;

  @override
  State<ConfigDialog> createState() => _ConfigDialogState();
}

class _ConfigDialogState extends State<ConfigDialog> {
  ConfigView? _view;
  String? _error;
  String? _section;
  final _search = TextEditingController();

  /// Per setting: being saved, or why the last save failed.
  final _busy = <String>{};
  final _errors = <String, String>{};
  var _restarting = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final v = await widget.client.configSettings();
      if (!mounted) return;
      setState(() {
        _view = v;
        _error = null;
        _section ??= v.sections.firstOrNull?.id;
      });
    } on TernException catch (e) {
      if (!mounted) return;
      setState(
        () => _error = e.status == 403
            ? 'Only a device paired as owner can see the configuration.'
            : e.message,
      );
    }
  }

  void _replace(ConfigSetting s) {
    final v = _view;
    if (v == null) return;
    _view = ConfigView(
      path: v.path,
      error: v.error,
      sections: [
        for (final sec in v.sections)
          ConfigSection(
            id: sec.id,
            title: sec.title,
            description: sec.description,
            settings: [for (final x in sec.settings) x.key == s.key ? s : x],
          ),
      ],
    );
  }

  Future<void> _write(
    String key,
    Future<ConfigSetting> Function(TernClient c) op,
  ) async {
    setState(() {
      _busy.add(key);
      _errors.remove(key);
    });
    try {
      final s = await op(widget.client);
      if (mounted) setState(() => _replace(s));
    } on TernException catch (e) {
      if (mounted) setState(() => _errors[key] = e.message);
    } finally {
      if (mounted) setState(() => _busy.remove(key));
    }
  }

  Future<void> _restart() async {
    final r = widget.onRestart;
    if (r == null) return;
    setState(() => _restarting = true);
    try {
      await r();
    } finally {
      if (mounted) Navigator.pop(context);
    }
  }

  bool _matches(ConfigSetting s, String q) =>
      s.key.toLowerCase().contains(q) ||
      settingLabel(s).toLowerCase().contains(q) ||
      s.description.toLowerCase().contains(q) ||
      (s.security?.toLowerCase().contains(q) ?? false);

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final view = _view;
    final pending = view?.settings.where((s) => s.restartPending).length ?? 0;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 780, maxHeight: 680),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  const Expanded(
                    child: Text(
                      'Configuration',
                      style: TextStyle(
                        fontSize: 17,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  if (view != null && widget.canReveal)
                    TextButton.icon(
                      onPressed: () => widget.reveal(view.path),
                      icon: Icon(Icons.description_outlined, size: 16),
                      label: const Text('Open config.toml'),
                    ),
                ],
              ),
              const SizedBox(height: 4),
              Text(
                'What ${view?.path ?? 'config.toml'} sets. Each change is '
                'checked and saved right away, keeping the file\'s comments. '
                'Models, MCP servers and secrets have their own windows.',
                style: small,
              ),
              const SizedBox(height: 12),
              if (pending > 0) ...[
                _RestartBanner(
                  count: pending,
                  restarting: _restarting,
                  onRestart: widget.onRestart == null ? null : _restart,
                ),
                const SizedBox(height: 10),
              ],
              Flexible(
                child: view == null || view.error != null
                    ? Padding(
                        padding: const EdgeInsets.all(24),
                        child: Center(
                          child: switch ((_error, view?.error)) {
                            (final e?, _) => Text(
                              e,
                              style: TextStyle(color: c.error),
                            ),
                            (_, final e?) => Text(
                              "config.toml doesn't load, so nothing can be "
                              'changed here until it is fixed:\n\n$e',
                              style: TextStyle(color: c.error),
                            ),
                            _ => const CircularProgressIndicator(
                              strokeWidth: 2,
                            ),
                          },
                        ),
                      )
                    : _body(context, view),
              ),
              const SizedBox(height: 12),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: () => Navigator.pop(context),
                  child: const Text('Done'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _body(BuildContext context, ConfigView view) {
    final c = context.colors;
    final q = _search.text.trim().toLowerCase();
    final sections = [
      for (final s in view.sections)
        if (q.isEmpty ? s.id == _section : true)
          (
            s,
            [
              for (final x in s.settings)
                if (_matches(x, q)) x,
            ],
          ),
    ].where((e) => e.$2.isNotEmpty).toList();
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          width: 170,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                controller: _search,
                onChanged: (_) => setState(() {}),
                style: const TextStyle(fontSize: 13),
                decoration: _box(
                  context,
                  hint: 'Search',
                  icon: Icon(Icons.search, size: 16, color: c.muted),
                ),
              ),
              const SizedBox(height: 8),
              Flexible(
                child: ListView(
                  shrinkWrap: true,
                  children: [
                    for (final s in view.sections)
                      _SectionTile(
                        section: s,
                        selected: q.isEmpty && s.id == _section,
                        onTap: () => setState(() {
                          _section = s.id;
                          _search.clear();
                        }),
                      ),
                  ],
                ),
              ),
            ],
          ),
        ),
        const SizedBox(width: 16),
        Expanded(
          child: sections.isEmpty
              ? Padding(
                  padding: const EdgeInsets.all(24),
                  child: Text(
                    'No setting matches “${_search.text.trim()}”.',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                )
              : ListView(
                  shrinkWrap: true,
                  children: [
                    for (final (sec, settings) in sections) ...[
                      if (q.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.only(top: 4, bottom: 2),
                          child: Text(
                            sec.title,
                            style: TextStyle(
                              fontSize: 12,
                              fontWeight: FontWeight.w600,
                              color: c.muted,
                            ),
                          ),
                        )
                      else if (sec.description.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.only(bottom: 6),
                          child: Text(
                            sec.description,
                            style: Theme.of(context).textTheme.bodySmall,
                          ),
                        ),
                      for (final s in settings)
                        SettingRow(
                          key: ValueKey(s.key),
                          setting: s,
                          busy: _busy.contains(s.key),
                          error: _errors[s.key],
                          onChanged: (v) =>
                              _write(s.key, (c) => c.setSetting(s.key, v)),
                          onReset: () =>
                              _write(s.key, (c) => c.resetSetting(s.key)),
                        ),
                    ],
                  ],
                ),
        ),
      ],
    );
  }
}

/// Show `path` in Finder (or open its folder elsewhere).
Future<void> revealFile(String path) async {
  try {
    if (Platform.isMacOS) {
      await Process.run('open', ['-R', path]);
    } else {
      await Process.run('xdg-open', [File(path).parent.path]);
    }
  } on ProcessException {
    // No file manager; the path is in the dialog's text.
  }
}

/// `sandbox.shell_timeout_secs` → "Shell timeout (seconds)".
String settingLabel(ConfigSetting s) {
  var words = s.name.split('_');
  String? unit;
  if (words.length > 1) {
    unit = switch (words.last) {
      'secs' => 'seconds',
      'hours' => 'hours',
      _ => null,
    };
    if (unit != null) words = words.sublist(0, words.length - 1);
  }
  final text = words
      .map(
        (w) => switch (w) {
          'md' => '.md',
          'usd' => 'USD',
          'mcp' => 'MCP',
          'api' => 'API',
          'tls' => 'TLS',
          'url' => 'URL',
          'cpus' => 'CPUs',
          'pids' => 'processes',
          _ => w,
        },
      )
      .join(' ')
      .replaceAll(' .md', '.md');
  final label = text.isEmpty
      ? s.key
      : text[0].toUpperCase() + text.substring(1);
  return unit == null ? label : '$label ($unit)';
}

class _SectionTile extends StatelessWidget {
  const _SectionTile({
    required this.section,
    required this.selected,
    required this.onTap,
  });

  final ConfigSection section;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final changed = section.settings.where((s) => s.changed).length;
    final risky = section.settings.any((s) => s.risky);
    return Material(
      color: selected ? c.selection : Colors.transparent,
      borderRadius: BorderRadius.circular(7),
      child: InkWell(
        borderRadius: BorderRadius.circular(7),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  section.title,
                  style: TextStyle(
                    fontSize: 13,
                    fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                  ),
                ),
              ),
              if (risky)
                Padding(
                  padding: const EdgeInsets.only(left: 4),
                  child: Icon(Icons.shield_outlined, size: 13, color: c.warn),
                ),
              if (changed > 0)
                Padding(
                  padding: const EdgeInsets.only(left: 4),
                  child: Text(
                    '$changed',
                    style: TextStyle(fontSize: 11, color: c.link),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _RestartBanner extends StatelessWidget {
  const _RestartBanner({
    required this.count,
    required this.restarting,
    required this.onRestart,
  });

  final int count;
  final bool restarting;
  final VoidCallback? onRestart;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      padding: const EdgeInsets.fromLTRB(12, 8, 8, 8),
      decoration: BoxDecoration(
        color: c.warn.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Icon(Icons.restart_alt, size: 16, color: c.warn),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              count == 1
                  ? '1 saved change applies when tern restarts.'
                  : '$count saved changes apply when tern restarts.',
              style: const TextStyle(fontSize: 12.5),
            ),
          ),
          if (onRestart != null)
            TextButton(
              onPressed: restarting ? null : onRestart,
              child: Text(restarting ? 'Restarting…' : 'Restart tern'),
            ),
        ],
      ),
    );
  }
}

/// One setting: its label, description, safety note and a control for
/// its kind, with changed/restart marks and a reset.
class SettingRow extends StatefulWidget {
  const SettingRow({
    super.key,
    required this.setting,
    required this.onChanged,
    required this.onReset,
    this.busy = false,
    this.error,
  });

  final ConfigSetting setting;

  /// A new JSON value; `null` unsets an optional setting.
  final ValueChanged<Object?> onChanged;
  final VoidCallback onReset;
  final bool busy;
  final String? error;

  @override
  State<SettingRow> createState() => _SettingRowState();
}

class _SettingRowState extends State<SettingRow> {
  late final _text = TextEditingController(text: _show(widget.setting.value));
  final _focus = FocusNode();
  String? _local;

  /// The text last sent, so a submit and then a blur send it once.
  String? _sent;

  @override
  void initState() {
    super.initState();
    _focus.addListener(() {
      if (!_focus.hasFocus) _commit();
    });
  }

  @override
  void didUpdateWidget(SettingRow old) {
    super.didUpdateWidget(old);
    if (old.setting.value != widget.setting.value) {
      _sent = null;
      if (!_focus.hasFocus) _text.text = _show(widget.setting.value);
    }
  }

  @override
  void dispose() {
    _text.dispose();
    _focus.dispose();
    super.dispose();
  }

  static String _show(Object? v) => switch (v) {
    null => '',
    List l => l.join(', '),
    _ => '$v',
  };

  /// The text field's value as JSON, or an error to show instead.
  (Object?, String?) _parse(String raw) {
    final s = widget.setting;
    final t = raw.trim();
    if (t.isEmpty) {
      return switch (s.kind) {
        SettingKind.list => (const <String>[], null),
        SettingKind.text when !s.optional => ('', null),
        _ when s.optional => (null, null),
        _ => (null, 'Needs a value'),
      };
    }
    List<String> items() => [
      for (final p in t.split(','))
        if (p.trim().isNotEmpty) p.trim(),
    ];
    return switch (s.kind) {
      SettingKind.integer => switch (int.tryParse(t)) {
        final n? => (n, null),
        null => (null, 'A whole number'),
      },
      SettingKind.float => switch (double.tryParse(t)) {
        final n? => (n, null),
        null => (null, 'A number'),
      },
      SettingKind.list => (items(), null),
      SettingKind.model => switch (items()) {
        [final one] => (one, null),
        final many => (many, null),
      },
      _ => (t, null),
    };
  }

  void _commit() {
    final text = _text.text;
    if (text == _show(widget.setting.value) || text == _sent) {
      setState(() => _local = null);
      return;
    }
    final (v, err) = _parse(text);
    setState(() => _local = err);
    if (err != null) return;
    _sent = text;
    widget.onChanged(v);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final s = widget.setting;
    final error = _local ?? widget.error;
    final desc = s.description.isEmpty
        ? ''
        : s.description[0].toUpperCase() + s.description.substring(1);
    return Container(
      padding: const EdgeInsets.symmetric(vertical: 10),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: c.hairline)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Wrap(
                      spacing: 6,
                      runSpacing: 4,
                      crossAxisAlignment: WrapCrossAlignment.center,
                      children: [
                        Text(
                          settingLabel(s),
                          style: const TextStyle(
                            fontSize: 13.5,
                            fontWeight: FontWeight.w500,
                          ),
                        ),
                        if (s.changed) _Mark('Changed', c.accent),
                        if (s.restartPending) _Mark('Restart to apply', c.warn),
                      ],
                    ),
                    const SizedBox(height: 2),
                    Text(
                      s.key,
                      style: mono.copyWith(fontSize: 11, color: c.muted),
                    ),
                    if (desc.isNotEmpty) ...[
                      const SizedBox(height: 3),
                      Text(
                        desc,
                        style: TextStyle(fontSize: 12, color: c.muted),
                      ),
                    ],
                  ],
                ),
              ),
              const SizedBox(width: 12),
              _control(context),
              SizedBox(
                width: 36,
                child: s.set && s.editable
                    ? IconButton(
                        tooltip: 'Reset to default (${_default(s)})',
                        icon: Icon(Icons.undo, size: 16, color: c.muted),
                        splashRadius: 16,
                        visualDensity: VisualDensity.compact,
                        onPressed: widget.busy ? null : widget.onReset,
                      )
                    : null,
              ),
            ],
          ),
          if (s.security case final note?)
            Container(
              margin: const EdgeInsets.only(top: 8),
              padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 5),
              decoration: BoxDecoration(
                color: (s.risky ? c.warn : c.muted).withValues(alpha: 0.10),
                borderRadius: BorderRadius.circular(6),
              ),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(
                    s.risky ? Icons.shield_outlined : Icons.shield,
                    size: 14,
                    color: s.risky ? c.warn : c.muted,
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      note,
                      style: TextStyle(
                        fontSize: 12,
                        color: s.risky ? c.text : c.muted,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          if (error != null)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Text(
                error,
                style: TextStyle(fontSize: 12, color: c.error),
              ),
            ),
        ],
      ),
    );
  }

  static String _default(ConfigSetting s) => switch (s.defaultValue) {
    null => 'unset',
    List l when l.isEmpty => 'none',
    final v => _show(v),
  };

  Widget _control(BuildContext context) {
    final c = context.colors;
    final s = widget.setting;
    final enabled = !widget.busy && s.editable;
    switch (s.kind) {
      case SettingKind.boolean:
        return Switch(
          value: s.value == true,
          activeTrackColor: s.risky ? c.warn : c.accent,
          onChanged: enabled ? widget.onChanged : null,
        );
      case SettingKind.choice:
        return SegmentedButton<String>(
          showSelectedIcon: false,
          style: const ButtonStyle(visualDensity: VisualDensity.compact),
          segments: [
            for (final ch in s.choices)
              ButtonSegment(
                value: ch,
                label: Text(ch, style: const TextStyle(fontSize: 12.5)),
              ),
          ],
          selected: {if (s.value case final String v) v},
          emptySelectionAllowed: true,
          onSelectionChanged: enabled
              ? (v) {
                  if (v.isNotEmpty) widget.onChanged(v.first);
                }
              : null,
        );
      case SettingKind.other:
        return SizedBox(
          width: 220,
          child: Text(
            'Edit in config.toml',
            textAlign: TextAlign.right,
            style: TextStyle(fontSize: 12, color: c.muted),
          ),
        );
      case SettingKind.integer || SettingKind.float:
        return SizedBox(width: 120, child: _field(context, enabled));
      case SettingKind.text || SettingKind.list || SettingKind.model:
        return SizedBox(width: 240, child: _field(context, enabled));
    }
  }

  Widget _field(BuildContext context, bool enabled) {
    final s = widget.setting;
    final numeric =
        s.kind == SettingKind.integer || s.kind == SettingKind.float;
    return TextField(
      controller: _text,
      focusNode: _focus,
      enabled: enabled,
      textAlign: numeric ? TextAlign.right : TextAlign.start,
      style: mono.copyWith(fontSize: 12.5),
      onSubmitted: (_) => _commit(),
      decoration: _box(
        context,
        hint: s.optional
            ? 'Not set'
            : s.kind == SettingKind.list
            ? 'None'
            : null,
      ),
    );
  }
}

class _Mark extends StatelessWidget {
  const _Mark(this.label, this.color);

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
    padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
    decoration: BoxDecoration(
      color: color.withValues(alpha: 0.12),
      borderRadius: BorderRadius.circular(5),
    ),
    child: Text(label, style: TextStyle(fontSize: 10.5, color: color)),
  );
}

InputDecoration _box(BuildContext context, {String? hint, Widget? icon}) {
  final c = context.colors;
  OutlineInputBorder border(Color color) => OutlineInputBorder(
    borderRadius: BorderRadius.circular(8),
    borderSide: BorderSide(color: color),
  );
  return InputDecoration(
    hintText: hint,
    hintStyle: TextStyle(color: c.muted),
    prefixIcon: icon,
    prefixIconConstraints: const BoxConstraints(minWidth: 30, minHeight: 0),
    isDense: true,
    contentPadding: const EdgeInsets.all(9),
    border: border(c.hairline),
    enabledBorder: border(c.hairline),
    focusedBorder: border(c.ink),
  );
}
