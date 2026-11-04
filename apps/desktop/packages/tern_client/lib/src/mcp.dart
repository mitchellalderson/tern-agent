// Types mirroring `tern_core::mcp` and `McpServerConfig`
// (docs/mcp-management-spec.md §4.2, §7). Secret values never appear here
// except in [ImportedServer.secrets], which the daemon read from a paste.

import 'models.dart';

enum McpTransport {
  stdio('stdio'),
  http('http');

  const McpTransport(this.wire);

  final String wire;

  static McpTransport fromWire(String? s) => s == 'http' ? http : stdio;
}

/// One `[mcp.servers.<name>]` table. Holds secret names, never values.
class McpServerConfig {
  const McpServerConfig({
    this.transport = McpTransport.stdio,
    this.enabled = true,
    this.command,
    this.args = const [],
    this.env = const {},
    this.cwd,
    this.url,
    this.envSecrets = const [],
    this.bearerTokenSecret,
    this.headerSecrets = const {},
    this.disabledTools = const [],
    this.idempotent = false,
    this.requiresApproval = false,
    this.effects = const {},
    this.untrusted = false,
  });

  factory McpServerConfig.fromJson(Json j) => McpServerConfig(
    transport: McpTransport.fromWire(j['transport'] as String?),
    enabled: j['enabled'] as bool? ?? true,
    command: j['command'] as String?,
    args: [for (final a in j['args'] as List? ?? const []) a as String],
    env: _strings(j['env']),
    cwd: j['cwd'] as String?,
    url: j['url'] as String?,
    envSecrets: [
      for (final s in j['env_secrets'] as List? ?? const []) s as String,
    ],
    bearerTokenSecret: j['bearer_token_secret'] as String?,
    headerSecrets: _strings(j['header_secrets']),
    disabledTools: [
      for (final t in j['disabled_tools'] as List? ?? const []) t as String,
    ],
    idempotent: j['idempotent'] as bool? ?? false,
    requiresApproval: j['requires_approval'] as bool? ?? false,
    effects: _strings(j['effects']),
    untrusted: j['untrusted'] as bool? ?? false,
  );

  final McpTransport transport;

  /// A disabled server isn't started and adds no tools.
  final bool enabled;

  /// stdio: the executable, its arguments, plain environment and working
  /// directory.
  final String? command;
  final List<String> args;
  final Map<String, String> env;
  final String? cwd;

  /// http: the streamable HTTP endpoint.
  final String? url;

  /// Secret names exported to the server's environment.
  final List<String> envSecrets;

  /// http: a secret sent as `Authorization: Bearer <value>`.
  final String? bearerTokenSecret;

  /// http: header name → secret name.
  final Map<String, String> headerSecrets;

  /// The server's own tool names, hidden from the model.
  final List<String> disabledTools;
  final bool idempotent;
  final bool requiresApproval;

  /// Tool name (or `*`) → `read`, `write` or `destructive`.
  final Map<String, String> effects;

  /// Its tools return third-party text (a browser, mail): their results
  /// taint the run, so its writes ask.
  final bool untrusted;

  /// Every secret the server needs, in the daemon's order.
  List<String> get secretNames => [
    ...envSecrets,
    ?bearerTokenSecret,
    ...headerSecrets.values,
  ];

  /// What the daemon accepts: defaults and the other transport's fields
  /// are left out (it refuses unknown fields and mixed transports).
  Json toJson() {
    final stdio = transport == McpTransport.stdio;
    return {
      'transport': transport.wire,
      if (!enabled) 'enabled': false,
      if (stdio) ...{
        'command': ?command,
        if (args.isNotEmpty) 'args': args,
        if (env.isNotEmpty) 'env': env,
        'cwd': ?cwd,
      } else ...{
        'url': ?url,
        'bearer_token_secret': ?bearerTokenSecret,
        if (headerSecrets.isNotEmpty) 'header_secrets': headerSecrets,
      },
      if (envSecrets.isNotEmpty) 'env_secrets': envSecrets,
      if (disabledTools.isNotEmpty) 'disabled_tools': disabledTools,
      if (idempotent) 'idempotent': true,
      if (requiresApproval) 'requires_approval': true,
      if (effects.isNotEmpty) 'effects': effects,
      if (untrusted) 'untrusted': true,
    };
  }

  McpServerConfig copyWith({
    McpTransport? transport,
    bool? enabled,
    List<String>? disabledTools,
  }) => McpServerConfig(
    transport: transport ?? this.transport,
    enabled: enabled ?? this.enabled,
    command: command,
    args: args,
    env: env,
    cwd: cwd,
    url: url,
    envSecrets: envSecrets,
    bearerTokenSecret: bearerTokenSecret,
    headerSecrets: headerSecrets,
    disabledTools: disabledTools ?? this.disabledTools,
    idempotent: idempotent,
    requiresApproval: requiresApproval,
    effects: effects,
    untrusted: untrusted,
  );

  static Map<String, String> _strings(Object? j) => {
    for (final MapEntry(:key, :value) in (j as Json? ?? const {}).entries)
      key: value as String,
  };
}

/// Where a server is. Arrives tagged by `kind`.
sealed class McpState {
  const McpState();

  factory McpState.fromJson(Json j) => switch (j['kind']) {
    'disabled' => const McpDisabled(),
    'connecting' => const McpConnecting(),
    'running' => McpRunning(
      tools: j['tools'] as int? ?? 0,
      since: _seconds(j['since']),
    ),
    'unavailable' => McpUnavailable(
      error: j['error'] as String? ?? '',
      retryAt: j['retry_at'] is int ? _seconds(j['retry_at']) : null,
      failures: j['failures'] as int? ?? 0,
    ),
    _ => const McpNotStarted(),
  };

  static DateTime _seconds(Object? t) =>
      DateTime.fromMillisecondsSinceEpoch((t as int? ?? 0) * 1000);
}

class McpDisabled extends McpState {
  const McpDisabled();
}

/// Enabled, but nothing has needed it yet (startup is lazy).
class McpNotStarted extends McpState {
  const McpNotStarted();
}

class McpConnecting extends McpState {
  const McpConnecting();
}

class McpRunning extends McpState {
  const McpRunning({required this.tools, required this.since});

  /// Tools offered to the model (`disabled_tools` excluded).
  final int tools;
  final DateTime since;
}

class McpUnavailable extends McpState {
  const McpUnavailable({
    required this.error,
    this.retryAt,
    required this.failures,
  });

  final String error;

  /// When it's tried again; null once it has given up.
  final DateTime? retryAt;
  final int failures;
}

class McpToolInfo {
  const McpToolInfo({
    required this.exposed,
    required this.remote,
    required this.description,
    this.disabled = false,
  });

  factory McpToolInfo.fromJson(Json j) => McpToolInfo(
    exposed: j['exposed'] as String,
    remote: j['remote'] as String,
    description: j['description'] as String? ?? '',
    disabled: j['disabled'] as bool? ?? false,
  );

  /// `mcp__<server>__<tool>`, the name the model sees.
  final String exposed;

  /// The server's own name, which `disabled_tools` lists.
  final String remote;
  final String description;
  final bool disabled;
}

/// One server as `GET /v1/mcp` lists it.
class McpStatus {
  const McpStatus({
    required this.name,
    required this.config,
    required this.state,
    this.tools = const [],
    this.missingSecrets = const [],
    this.serverInfo,
    this.stderrTail = const [],
  });

  factory McpStatus.fromJson(Json j) => McpStatus(
    name: j['name'] as String,
    config: McpServerConfig.fromJson(j['config'] as Json? ?? const {}),
    state: McpState.fromJson(j['state'] as Json? ?? const {}),
    tools: [
      for (final t in j['tools'] as List? ?? const [])
        McpToolInfo.fromJson(t as Json),
    ],
    missingSecrets: [
      for (final s in j['missing_secrets'] as List? ?? const []) s as String,
    ],
    serverInfo: j['server_info'] as String?,
    stderrTail: [
      for (final l in j['stderr_tail'] as List? ?? const []) l as String,
    ],
  );

  final String name;
  final McpServerConfig config;
  final McpState state;
  final List<McpToolInfo> tools;
  final List<String> missingSecrets;

  /// Name and version from the server's `initialize` reply.
  final String? serverInfo;

  /// The last lines a stdio server wrote to stderr, redacted.
  final List<String> stderrTail;
}

/// A successful test connection.
class McpVerified {
  const McpVerified({required this.tools, this.serverInfo});

  factory McpVerified.fromJson(Json j) => McpVerified(
    tools: [
      for (final t in j['tools'] as List? ?? const [])
        McpToolInfo.fromJson(t as Json),
    ],
    serverInfo: j['server_info'] as String?,
  );

  final List<McpToolInfo> tools;
  final String? serverInfo;
}

/// Why a test connection failed.
class McpVerifyError {
  const McpVerifyError({
    required this.kind,
    required this.message,
    this.stderrTail = const [],
  });

  factory McpVerifyError.fromJson(Json j) => McpVerifyError(
    kind: j['kind'] as String? ?? 'protocol',
    message: j['message'] as String? ?? '',
    stderrTail: [
      for (final l in j['stderr_tail'] as List? ?? const []) l as String,
    ],
  );

  /// `command_not_found`, `spawn_failed`, `exited_early`, `timeout`,
  /// `unauthorized`, `unreachable`, `protocol`, `missing_secret` or
  /// `invalid` (the config itself is wrong).
  final String kind;
  final String message;
  final List<String> stderrTail;

  /// [kind] in words, for a heading.
  String get label => switch (kind) {
    'command_not_found' => 'Command not found',
    'spawn_failed' => "Couldn't start",
    'exited_early' => 'Exited during startup',
    'timeout' => 'Timed out',
    'unauthorized' => 'Not authorized',
    'unreachable' => "Couldn't reach it",
    'missing_secret' => 'Missing secret',
    'invalid' => 'Invalid config',
    _ => 'Protocol error',
  };
}

/// `POST /v1/mcp/verify`: exactly one of [verified] and [error] is set.
class McpVerifyResult {
  const McpVerifyResult({this.verified, this.error});

  factory McpVerifyResult.fromJson(Json j) => j['ok'] == true
      ? McpVerifyResult(
          verified: McpVerified.fromJson(j['verified'] as Json? ?? const {}),
        )
      : McpVerifyResult(
          error: McpVerifyError.fromJson(j['error'] as Json? ?? const {}),
        );

  final McpVerified? verified;
  final McpVerifyError? error;

  bool get ok => verified != null;
}

/// A server read from a pasted config, for review. Nothing is saved yet.
class ImportedServer {
  const ImportedServer({
    required this.name,
    required this.config,
    this.secrets = const {},
    this.warnings = const [],
  });

  factory ImportedServer.fromJson(Json j) => ImportedServer(
    name: j['name'] as String,
    config: McpServerConfig.fromJson(j['config'] as Json? ?? const {}),
    secrets: {
      for (final MapEntry(:key, :value)
          in (j['secrets'] as Json? ?? const {}).entries)
        key: value as String?,
    },
    warnings: [for (final w in j['warnings'] as List? ?? const []) w as String],
  );

  final String name;
  final McpServerConfig config;

  /// Secret name → the value found in the paste, or null when it was
  /// missing or a placeholder and the user must enter it.
  final Map<String, String?> secrets;
  final List<String> warnings;
}
