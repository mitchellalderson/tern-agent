// Types mirroring `tern_core::config::settings`: `config.toml` as the
// config editor shows it (`/v1/config/settings`). Holds no secret values.

import 'models.dart';

/// How a setting is edited.
enum SettingKind {
  boolean('bool'),
  integer('int'),
  float('float'),
  text('text'),
  choice('choice'),

  /// A list of strings.
  list('list'),

  /// `provider:model`, or a fallback chain of them.
  model('model'),

  /// Shown, but changed only in `config.toml`.
  other('other');

  const SettingKind(this.wire);

  final String wire;

  static SettingKind fromWire(String? s) =>
      values.firstWhere((k) => k.wire == s, orElse: () => other);
}

class ConfigSetting {
  const ConfigSetting({
    required this.key,
    required this.kind,
    this.choices = const [],
    this.min,
    this.max,
    this.optional = false,
    this.value,
    this.defaultValue,
    this.set = false,
    this.description = '',
    this.live = false,
    this.restartPending = false,
    this.security,
    this.risky = false,
    this.editable = true,
    this.group = 'advanced',
    this.advanced = false,
    this.restartRequired = false,
  });

  factory ConfigSetting.fromJson(Json j) => ConfigSetting(
    key: j['key'] as String,
    kind: SettingKind.fromWire(j['kind'] as String?),
    choices: [for (final c in j['choices'] as List? ?? const []) c as String],
    min: (j['min'] as num?)?.toDouble(),
    max: (j['max'] as num?)?.toDouble(),
    optional: j['optional'] as bool? ?? false,
    value: j['value'],
    defaultValue: j['default'],
    set: j['set'] as bool? ?? false,
    description: j['description'] as String? ?? '',
    live: j['applies'] == 'live',
    restartPending: j['restart_pending'] as bool? ?? false,
    security: j['security'] as String?,
    risky: j['risky'] as bool? ?? false,
    editable: j['editable'] as bool? ?? true,
    group: j['group'] as String? ?? 'advanced',
    advanced: j['advanced'] as bool? ?? false,
    restartRequired: j['restart_required'] as bool? ?? j['applies'] != 'live',
  );

  /// Dotted path, e.g. `sandbox.network`.
  final String key;
  final SettingKind kind;
  final List<String> choices;
  final double? min;
  final double? max;

  /// May be left unset (the default is `null`).
  final bool optional;

  /// JSON as the daemon sent it: what the file says, or the default.
  final Object? value;
  final Object? defaultValue;

  /// The file sets it.
  final bool set;
  final String description;

  /// Applies from the next run; otherwise when tern restarts.
  final bool live;

  /// Saved, but the running tern still uses the old value.
  final bool restartPending;

  /// Why this setting matters for safety, when it does.
  final String? security;

  /// The current value is the less safe choice.
  final bool risky;
  final bool editable;

  /// Where the platform Settings window shows it: `general`, `models`,
  /// `permissions`, `limits`, `servers` or `advanced`.
  final String group;

  /// A tuning knob, shown under Advanced (or behind "show advanced").
  final bool advanced;

  /// A saved change needs tern to restart (as opposed to the next run).
  final bool restartRequired;

  /// Differs from the default (a file entry equal to it doesn't count).
  bool get changed => set && !_same(value, defaultValue);

  /// The last part of [key]: `network` for `sandbox.network`.
  String get name => key.split('.').last;
}

bool _same(Object? a, Object? b) {
  if (a is List && b is List) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (!_same(a[i], b[i])) return false;
    }
    return true;
  }
  if (a is num && b is num) return a == b;
  return a == b;
}

class ConfigSection {
  const ConfigSection({
    required this.id,
    required this.title,
    this.description = '',
    this.settings = const [],
  });

  factory ConfigSection.fromJson(Json j) => ConfigSection(
    id: j['id'] as String,
    title: j['title'] as String,
    description: j['description'] as String? ?? '',
    settings: [
      for (final s in j['settings'] as List) ConfigSetting.fromJson(s as Json),
    ],
  );

  final String id;
  final String title;
  final String description;
  final List<ConfigSetting> settings;
}

/// `GET /v1/config/settings`.
class ConfigView {
  const ConfigView({required this.path, this.error, this.sections = const []});

  factory ConfigView.fromJson(Json j) => ConfigView(
    path: j['path'] as String,
    error: j['error'] as String?,
    sections: [
      for (final s in j['sections'] as List? ?? const [])
        ConfigSection.fromJson(s as Json),
    ],
  );

  /// `config.toml` on the machine tern runs on.
  final String path;

  /// The file doesn't load; [sections] is empty until it's fixed.
  final String? error;
  final List<ConfigSection> sections;

  Iterable<ConfigSetting> get settings => sections.expand((s) => s.settings);

  /// Some saved setting still waits on a restart.
  bool get restartRequired => settings.any((s) => s.restartPending);

  /// The settings of one Settings-window group, in file order.
  List<ConfigSetting> inGroup(String group, {bool advanced = false}) => [
    for (final s in settings)
      if (s.group == group && s.advanced == advanced) s,
  ];
}
