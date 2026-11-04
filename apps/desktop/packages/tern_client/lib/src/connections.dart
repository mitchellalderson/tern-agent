import 'models.dart';

/// One signed-in account on a connector or MCP server (`GET
/// /v1/connections`). Named for what it is rather than `Connection`, which
/// the app uses for reaching daemons.
class Account {
  const Account({
    required this.id,
    required this.kind,
    required this.target,
    required this.slug,
    this.label,
    this.account,
    required this.method,
    this.isDefault = false,
    this.status = 'ok',
    this.lastError,
    this.createdAt,
    this.lastUsedAt,
    required this.title,
  });

  factory Account.fromJson(Json j) => Account(
    id: j['id'] as String,
    kind: j['kind'] as String,
    target: j['target'] as String,
    slug: j['slug'] as String,
    label: j['label'] as String?,
    account: j['account'] as String?,
    method: (j['auth'] as Json?)?['method'] as String? ?? 'none',
    isDefault: j['is_default'] as bool? ?? false,
    status: j['status'] as String? ?? 'ok',
    lastError: j['last_error'] as String?,
    createdAt: _time(j['created_at']),
    lastUsedAt: _time(j['last_used_at']),
    title: j['title'] as String? ?? '${j['target']} · ${j['slug']}',
  );

  static DateTime? _time(Object? ms) =>
      ms is int ? DateTime.fromMillisecondsSinceEpoch(ms) : null;

  final String id;

  /// `connector` or `mcp`.
  final String kind;
  final String target;

  /// Its name among the target's accounts; goes in the URL paths.
  final String slug;
  final String? label;

  /// Who it is at the service, e.g. an email address.
  final String? account;

  /// `oauth`, `key` or `none`.
  final String method;
  final bool isDefault;

  /// `ok`, `needs_reauth` or `error`.
  final String status;
  final String? lastError;
  final DateTime? createdAt;
  final DateTime? lastUsedAt;

  /// `gmail · work (me@x.com)`.
  final String title;

  bool get needsReauth => status == 'needs_reauth';
  bool get failing => status != 'ok';
}

/// Somewhere an account can be added: every connector and MCP server, even
/// with none yet.
class ConnectionTarget {
  const ConnectionTarget({
    required this.kind,
    required this.name,
    this.methods = const [],
    this.keyNames = const [],
  });

  factory ConnectionTarget.fromJson(Json j) => ConnectionTarget(
    kind: j['kind'] as String,
    name: j['name'] as String,
    methods: [for (final m in j['methods'] as List? ?? const []) m as String],
    keyNames: [
      for (final k in j['key_names'] as List? ?? const []) k as String,
    ],
  );

  final String kind;
  final String name;

  /// `oauth` and/or `key`.
  final List<String> methods;

  /// The secrets a key account asks for.
  final List<String> keyNames;

  bool get oauth => methods.contains('oauth');
  bool get key => methods.contains('key');
}

/// `GET /v1/connections`.
class ConnectionList {
  const ConnectionList({this.accounts = const [], this.targets = const []});

  factory ConnectionList.fromJson(Json j) => ConnectionList(
    accounts: [
      for (final c in j['connections'] as List? ?? const [])
        Account.fromJson(c as Json),
    ],
    targets: [
      for (final t in j['targets'] as List? ?? const [])
        ConnectionTarget.fromJson(t as Json),
    ],
  );

  final List<Account> accounts;
  final List<ConnectionTarget> targets;
}

/// What adding or reconnecting an account returned.
class ConnectResult {
  const ConnectResult(this.status, this.account);

  factory ConnectResult.fromJson(Json j) => ConnectResult(
    j['status'] as String? ?? 'connected',
    Account.fromJson(j['connection'] as Json),
  );

  /// `connected` or `reconnected`.
  final String status;
  final Account account;
}

/// One button on a `choose` card.
class AuthOption {
  const AuthOption({
    required this.id,
    required this.label,
    this.detail = '',
    this.recommended = false,
  });

  factory AuthOption.fromJson(Json j) => AuthOption(
    id: j['id'] as String,
    label: j['label'] as String? ?? j['id'] as String,
    detail: j['detail'] as String? ?? '',
    recommended: j['recommended'] as bool? ?? false,
  );

  /// What to answer with.
  final String id;
  final String label;
  final String detail;
  final bool recommended;
}

/// The daemon asking the user to pick how to connect, or to sign in.
class AuthCard {
  const AuthCard({
    required this.kind,
    required this.title,
    this.options = const [],
    this.url,
    this.userCode,
    this.verificationUri,
    this.deviceAvailable = false,
    this.notes = const [],
  });

  factory AuthCard.fromJson(Json j) => AuthCard(
    kind: j['kind'] as String? ?? 'sign_in',
    title: j['title'] as String? ?? '',
    options: [
      for (final o in j['options'] as List? ?? const [])
        AuthOption.fromJson(o as Json),
    ],
    url: j['url'] as String?,
    userCode: j['user_code'] as String?,
    verificationUri: j['verification_uri'] as String?,
    deviceAvailable: j['device_available'] as bool? ?? false,
    notes: [for (final n in j['notes'] as List? ?? const []) n as String],
  );

  /// `choose` or `sign_in`.
  final String kind;
  final String title;

  /// `choose`: the ways to connect.
  final List<AuthOption> options;

  /// `sign_in`: the browser page, or [userCode] at [verificationUri].
  final String? url;
  final String? userCode;
  final String? verificationUri;

  /// "Use a code instead" is on offer.
  final bool deviceAvailable;
  final List<String> notes;

  bool get choose => kind == 'choose';
  bool get device => userCode != null;
}

/// An [AuthCard] and where it came from. Arrives on `/v1/events` as
/// `{"auth_prompt": {...}}` and from `GET /v1/auth-prompts`. Cards the app
/// started itself have empty `runId` and `sessionId`.
class AuthPrompt {
  const AuthPrompt({
    required this.id,
    this.runId = '',
    this.sessionId = '',
    this.callId,
    this.parentRunId,
    this.agentLabel,
    this.rootSessionId,
    this.channelId,
    this.agentId,
    required this.card,
  });

  factory AuthPrompt.fromJson(Json j) => AuthPrompt(
    id: j['id'] as String,
    runId: j['run_id'] as String? ?? '',
    sessionId: j['session_id'] as String? ?? '',
    callId: j['call_id'] as String?,
    parentRunId: j['parent_run_id'] as String?,
    agentLabel: j['agent_label'] as String?,
    rootSessionId: j['root_session_id'] as String?,
    channelId: j['channel_id'] as String?,
    agentId: j['agent_id'] as String?,
    card: AuthCard.fromJson(j['card'] as Json),
  );

  final String id;
  final String runId;
  final String sessionId;

  /// The tool call that asked; its `tool_finished` closes the card, and a
  /// new card with the same id replaces it.
  final String? callId;
  final String? parentRunId;
  final String? agentLabel;
  final String? rootSessionId;
  final String? channelId;
  final String? agentId;
  final AuthCard card;
}
