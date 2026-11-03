import 'models.dart';

/// One name the agent wants a value for.
class SecretAsk {
  const SecretAsk({
    required this.name,
    required this.inFile,
    required this.inEnv,
    required this.usedBy,
  });

  factory SecretAsk.fromJson(Json j) => SecretAsk(
    name: j['name'] as String,
    inFile: j['in_file'] as bool? ?? false,
    inEnv: j['in_env'] as bool? ?? false,
    usedBy: [for (final u in j['used_by'] as List? ?? const []) u as String],
  );

  final String name;

  /// A value is already saved in `secrets.toml` (entering one replaces it).
  final bool inFile;

  /// Set in the daemon's environment, which wins over a saved value.
  final bool inEnv;

  /// What will read it, e.g. `connector weather (draft) · reaches api…`.
  final List<String> usedBy;
}

/// The agent asking the user to enter secrets. Arrives on `/v1/events` as
/// `{"secret_requested": {...}}` and from `GET /v1/secret-requests`.
/// The agent never sees the values, and neither does this model.
class SecretRequest {
  const SecretRequest({
    required this.id,
    required this.runId,
    required this.sessionId,
    this.callId,
    required this.secrets,
    required this.reason,
    this.replace = false,
    this.parentRunId,
    this.agentLabel,
    this.rootSessionId,
  });

  factory SecretRequest.fromJson(Json j) => SecretRequest(
    id: j['id'] as String,
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    callId: j['call_id'] as String?,
    secrets: [
      for (final s in j['secrets'] as List) SecretAsk.fromJson(s as Json),
    ],
    reason: j['reason'] as String? ?? '',
    replace: j['replace'] as bool? ?? false,
    parentRunId: j['parent_run_id'] as String?,
    agentLabel: j['agent_label'] as String?,
    rootSessionId: j['root_session_id'] as String?,
  );

  final String id;
  final String runId;
  final String sessionId;

  /// The `secret_request` tool call; its `tool_finished` ends the request.
  final String? callId;
  final List<SecretAsk> secrets;

  /// The agent's own words. Show them as such.
  final String reason;
  final bool replace;

  /// Set when a sub-agent asks (see [Approval]).
  final String? parentRunId;
  final String? agentLabel;
  final String? rootSessionId;
}
