import 'dart:io';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../theme.dart';

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

/// "N saved changes apply when tern restarts", with a restart button when
/// the app can restart tern.
class RestartBanner extends StatelessWidget {
  const RestartBanner({
    super.key,
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
      decoration: settingsBox(
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

/// The rounded field style Settings uses for search and values.
InputDecoration settingsBox(
  BuildContext context, {
  String? hint,
  Widget? icon,
}) {
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
