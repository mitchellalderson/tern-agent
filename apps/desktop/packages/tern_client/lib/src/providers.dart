import 'models.dart';

/// `anthropic` or `openai_compat` (docs/provider-setup-spec.md §3).
enum ProviderKind {
  anthropic('anthropic', 'Anthropic'),
  openaiCompat('openai_compat', 'OpenAI-compatible');

  const ProviderKind(this.wire, this.label);

  final String wire;
  final String label;

  static ProviderKind fromWire(String? s) =>
      s == 'anthropic' ? anthropic : openaiCompat;
}

/// One model a provider lists.
class ModelInfo {
  const ModelInfo({required this.id, this.name, this.contextWindow});

  factory ModelInfo.fromJson(Json j) => ModelInfo(
    id: j['id'] as String,
    name: j['name'] as String?,
    contextWindow: j['context_window'] as int?,
  );

  /// What goes after `provider:`.
  final String id;

  /// Display name, when the provider gives one.
  final String? name;
  final int? contextWindow;
}

/// Why listing a provider's models failed: `unauthorized`, `unreachable`, …
class ListError {
  const ListError(this.kind, [this.detail]);

  static ListError? fromJson(Object? j) => j is Json
      ? ListError(j['kind'] as String, j['detail']?.toString())
      : null;

  final String kind;
  final String? detail;
}

/// A configured provider. Never includes the key, only a masked hint.
class ProviderStatus {
  const ProviderStatus({
    required this.name,
    required this.kind,
    this.baseUrl,
    this.secret,
    required this.keySource,
    this.keyHint,
    this.error,
    this.message,
    required this.isDefault,
  });

  factory ProviderStatus.fromJson(Json j) => ProviderStatus(
    name: j['name'] as String,
    kind: ProviderKind.fromWire(j['kind'] as String?),
    baseUrl: j['base_url'] as String?,
    secret: j['secret'] as String?,
    keySource: j['key_source'] as String? ?? 'none',
    keyHint: j['key_hint'] as String?,
    error: ListError.fromJson(j['error']),
    message: j['message'] as String?,
    isDefault: j['is_default'] as bool? ?? false,
  );

  final String name;
  final ProviderKind kind;
  final String? baseUrl;

  /// The secret holding its key, e.g. `ANTHROPIC_API_KEY`.
  final String? secret;

  /// `env`, `file`, `missing`, or `none` (no key configured).
  final String keySource;

  /// `sk-ant…a1B2`.
  final String? keyHint;

  /// From the last model listing.
  final ListError? error;

  /// [error] in plain words.
  final String? message;
  final bool isDefault;
}

/// One provider's models.
class ProviderModels {
  const ProviderModels({
    required this.provider,
    this.error,
    this.message,
    required this.models,
  });

  factory ProviderModels.fromJson(Json j) => ProviderModels(
    provider: j['provider'] as String,
    error: ListError.fromJson(j['error']),
    message: j['message'] as String?,
    models: [
      for (final m in j['models'] as List? ?? const [])
        ModelInfo.fromJson(m as Json),
    ],
  );

  final String provider;
  final ListError? error;
  final String? message;
  final List<ModelInfo> models;
}

/// What the user filled in to add or change a provider.
class ProviderDraft {
  const ProviderDraft({
    this.kind,
    this.name,
    this.baseUrl,
    this.key,
    this.useEnvKey = false,
  });

  final ProviderKind? kind;
  final String? name;
  final String? baseUrl;

  /// A pasted key; the daemon cleans it up.
  final String? key;

  /// Use the key already set in the daemon's environment.
  final bool useEnvKey;

  Json toJson() => {
    'kind': ?kind?.wire,
    'name': ?_blankToNull(name),
    'base_url': ?_blankToNull(baseUrl),
    'key': ?_blankToNull(key),
    'use_env_key': useEnvKey,
  };

  static String? _blankToNull(String? s) =>
      s == null || s.trim().isEmpty ? null : s.trim();
}

/// The result of checking a draft against the provider. Saves nothing.
class Verified {
  const Verified({
    required this.ok,
    required this.name,
    this.error,
    this.message,
    required this.warnings,
    this.suggestedBaseUrl,
    this.keyHint,
    required this.models,
  });

  factory Verified.fromJson(Json j) => Verified(
    ok: j['ok'] as bool,
    name: j['name'] as String,
    error: ListError.fromJson(j['error']),
    message: j['message'] as String?,
    warnings: [for (final w in j['warnings'] as List? ?? const []) w as String],
    suggestedBaseUrl: j['suggested_base_url'] as String?,
    keyHint: j['key_hint'] as String?,
    models: [
      for (final m in j['models'] as List? ?? const [])
        ModelInfo.fromJson(m as Json),
    ],
  );

  /// The key is accepted (the list may still be empty, e.g. rate limited).
  final bool ok;

  /// The name it will be saved under.
  final String name;
  final ListError? error;

  /// The error in plain words, to show as is.
  final String? message;

  /// Advice that doesn't block saving.
  final List<String> warnings;

  /// Set when `/v1` had to be added to the base URL.
  final String? suggestedBaseUrl;
  final String? keyHint;
  final List<ModelInfo> models;
}

/// A model reference resolved by the daemon (the provider may have been
/// left off), with a warning when the provider doesn't list it.
class Resolved {
  const Resolved({required this.model, this.warning});

  factory Resolved.fromJson(Json j, String fallback) => Resolved(
    model: (j['resolved'] ?? j['model']) as String? ?? fallback,
    warning: j['warning'] as String?,
  );

  final String model;
  final String? warning;
}
