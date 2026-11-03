// Types mirroring the daemon API (`crates/tern/src/daemon/api.rs`) and
// `tern_core::events::AgentEvent`. Unknown fields are ignored and unknown
// event types decode to [UnknownEvent], so a newer daemon doesn't break
// an older app.

typedef Json = Map<String, dynamic>;

/// `/v1/health`: what the core is and what this client may do there
/// (deployment spec §7.3). Derive behaviour from these, not from how the
/// core was reached.
class Health {
  const Health({
    required this.pid,
    required this.version,
    this.api = 1,
    this.deployment = 'local',
    this.authKind = 'owner',
    this.scope = 'owner',
    this.features = const {},
  });

  factory Health.fromJson(Json j) {
    final auth = j['auth'] as Json?;
    return Health(
      pid: j['pid'] as int,
      version: j['version'] as String,
      api: j['api'] as int? ?? 1,
      deployment: j['deployment'] as String? ?? 'local',
      authKind: auth?['kind'] as String? ?? 'owner',
      scope: auth?['scope'] as String? ?? 'owner',
      features: {
        for (final f in j['features'] as List? ?? const []) f as String,
      },
    );
  }

  final int pid;
  final String version;
  final int api;

  /// `local` (this machine: files, Finder, start/stop apply), `server` or
  /// `cloud`.
  final String deployment;

  /// `owner` (the local socket), `device` or `cloud`.
  final String authKind;

  /// `owner`, `chat` or `read`. Hide owner-only screens below `owner`.
  final String scope;

  /// e.g. `devices`, `uploads`.
  final Set<String> features;

  bool get isLocal => deployment == 'local';
  bool get isOwner => scope == 'owner';
  bool get canChat => scope != 'read';
}

/// A paired device, as the owner sees it.
class Device {
  const Device({
    required this.id,
    required this.name,
    required this.scope,
    this.platform,
    required this.createdAt,
    this.lastSeenAt,
    this.lastIp,
    this.revokedAt,
  });

  factory Device.fromJson(Json j) => Device(
    id: j['id'] as String,
    name: j['name'] as String,
    scope: j['scope'] as String,
    platform: j['platform'] as String?,
    createdAt: DateTime.fromMillisecondsSinceEpoch(j['created_at'] as int),
    lastSeenAt: _ms(j['last_seen_at']),
    lastIp: j['last_ip'] as String?,
    revokedAt: _ms(j['revoked_at']),
  );

  final String id;
  final String name;
  final String scope;
  final String? platform;
  final DateTime createdAt;
  final DateTime? lastSeenAt;
  final String? lastIp;
  final DateTime? revokedAt;

  bool get revoked => revokedAt != null;
}

DateTime? _ms(Object? v) =>
    v == null ? null : DateTime.fromMillisecondsSinceEpoch(v as int);

/// A one-time code for a new device; show [link] as a QR code.
class PairingOffer {
  const PairingOffer({
    required this.code,
    required this.scope,
    this.link,
    this.fingerprint,
    required this.expiresIn,
  });

  factory PairingOffer.fromJson(Json j) => PairingOffer(
    code: j['code'] as String,
    scope: j['scope'] as String,
    link: j['link'] as String?,
    fingerprint: j['fingerprint'] as String?,
    expiresIn: Duration(seconds: j['expires_in_secs'] as int),
  );

  final String code;
  final String scope;

  /// Null when the core has no TCP listener yet.
  final String? link;
  final String? fingerprint;
  final Duration expiresIn;
}

class Info {
  const Info({this.defaultModel, required this.tools});

  factory Info.fromJson(Json j) => Info(
    defaultModel: j['default_model'] as String?,
    tools: [
      for (final t in j['tools'] as List)
        (name: (t as List)[0] as String, description: t[1] as String),
    ],
  );

  final String? defaultModel;
  final List<({String name, String description})> tools;
}

class Started {
  const Started({
    required this.sessionId,
    required this.runId,
    required this.model,
  });

  factory Started.fromJson(Json j) => Started(
    sessionId: j['session_id'] as String,
    runId: j['run_id'] as String,
    model: j['model'] as String,
  );

  final String sessionId;
  final String runId;
  final String model;
}

class ActiveRun {
  const ActiveRun({required this.runId, required this.sessionId});

  factory ActiveRun.fromJson(Json j) => ActiveRun(
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
  );

  final String runId;
  final String sessionId;
}

class SessionItem {
  const SessionItem({
    required this.id,
    this.title,
    required this.source,
    this.model,
    required this.messages,
    required this.runs,
    required this.updatedAt,
  });

  factory SessionItem.fromJson(Json j) => SessionItem(
    id: j['id'] as String,
    title: j['title'] as String?,
    source: j['source'] as String,
    model: j['model'] as String?,
    messages: j['messages'] as int,
    runs: j['runs'] as int,
    updatedAt: _time(j['updated_at'] as int),
  );

  final String id;
  final String? title;

  /// Where the session came from: `api`, `cli`, `telegram`, `cron`, …
  final String source;
  final String? model;
  final int messages;
  final int runs;
  final DateTime updatedAt;
}

/// Store timestamps are Unix seconds; accept milliseconds too.
DateTime _time(int t) =>
    DateTime.fromMillisecondsSinceEpoch(t > 100000000000 ? t : t * 1000);

sealed class ContentBlock {
  const ContentBlock();

  factory ContentBlock.fromJson(Json j) => switch (j['type']) {
    'text' => TextBlock(j['text'] as String),
    'tool_use' => ToolUseBlock(
      id: j['id'] as String,
      name: j['name'] as String,
      input: j['input'],
    ),
    'tool_result' => ToolResultBlock(
      toolUseId: j['tool_use_id'] as String,
      content: j['content'] as String,
      isError: j['is_error'] as bool? ?? false,
    ),
    _ => const TextBlock(''),
  };
}

class TextBlock extends ContentBlock {
  const TextBlock(this.text);
  final String text;
}

class ToolUseBlock extends ContentBlock {
  const ToolUseBlock({
    required this.id,
    required this.name,
    required this.input,
  });
  final String id;
  final String name;
  final Object? input;
}

class ToolResultBlock extends ContentBlock {
  const ToolResultBlock({
    required this.toolUseId,
    required this.content,
    required this.isError,
  });
  final String toolUseId;
  final String content;
  final bool isError;
}

class SessionMessage {
  const SessionMessage({
    required this.role,
    required this.content,
    required this.summary,
  });

  factory SessionMessage.fromJson(Json j) => SessionMessage(
    role: j['role'] as String,
    content: [
      for (final b in j['content'] as List) ContentBlock.fromJson(b as Json),
    ],
    summary: j['summary'] as bool? ?? false,
  );

  final String role;
  final List<ContentBlock> content;

  /// A compaction summary rather than something anyone said.
  final bool summary;
}

class SessionDetail {
  const SessionDetail({
    required this.id,
    this.title,
    this.model,
    required this.messages,
  });

  factory SessionDetail.fromJson(Json j) => SessionDetail(
    id: j['id'] as String,
    title: j['title'] as String?,
    model: j['model'] as String?,
    messages: [
      for (final m in j['messages'] as List) SessionMessage.fromJson(m as Json),
    ],
  );

  final String id;
  final String? title;
  final String? model;
  final List<SessionMessage> messages;
}

class Usage {
  const Usage({
    this.inputTokens = 0,
    this.outputTokens = 0,
    this.cacheReadTokens = 0,
    this.cacheWriteTokens = 0,
  });

  factory Usage.fromJson(Json j) => Usage(
    inputTokens: j['input_tokens'] as int? ?? 0,
    outputTokens: j['output_tokens'] as int? ?? 0,
    cacheReadTokens: j['cache_read_tokens'] as int? ?? 0,
    cacheWriteTokens: j['cache_write_tokens'] as int? ?? 0,
  );

  final int inputTokens;
  final int outputTokens;
  final int cacheReadTokens;
  final int cacheWriteTokens;

  int get total =>
      inputTokens + outputTokens + cacheReadTokens + cacheWriteTokens;
}

/// A tool call waiting for the user. Arrives on `/v1/events` as
/// `{"approval_requested": {...}}` and from `GET /v1/approvals`.
class Approval {
  const Approval({
    required this.id,
    required this.runId,
    required this.sessionId,
    required this.tool,
    required this.input,
    required this.summary,
    required this.pattern,
  });

  factory Approval.fromJson(Json j) => Approval(
    id: j['id'] as String,
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    tool: j['tool'] as String,
    input: j['input'],
    summary: j['summary'] as String,
    pattern: j['pattern'] as String,
  );

  final String id;
  final String runId;
  final String sessionId;
  final String tool;
  final Object? input;
  final String summary;

  /// What "always allow" adds to the allowlist.
  final String pattern;
}

enum Decision {
  once,
  always,
  deny;

  String get wire => name;
}

/// Everything `/v1/events` can send.
sealed class TernEvent {
  const TernEvent();

  factory TernEvent.fromJson(Json j) {
    if (j['approval_requested'] case final Json a) {
      return ApprovalRequested(Approval.fromJson(a));
    }
    final run = j['run_id'] as String? ?? '';
    return switch (j['type']) {
      'run_started' => RunStarted(
        runId: run,
        sessionId: j['session_id'] as String,
      ),
      'text_delta' => TextDelta(runId: run, text: j['text'] as String),
      'assistant_message' => AssistantMessage(
        runId: run,
        text: j['text'] as String,
        replayed: j['replayed'] as bool? ?? false,
      ),
      'tool_started' => ToolStarted(
        runId: run,
        id: j['id'] as String,
        name: j['name'] as String,
        summary: j['summary'] as String,
      ),
      'tool_finished' => ToolFinished(
        runId: run,
        id: j['id'] as String,
        name: j['name'] as String,
        isError: j['is_error'] as bool? ?? false,
        preview: j['preview'] as String? ?? '',
        output: j['output'] as String? ?? '',
        replayed: j['replayed'] as bool? ?? false,
      ),
      'approval' => ApprovalAnswered(
        runId: run,
        id: j['id'] as String,
        name: j['name'] as String,
        outcome: j['outcome'] as String,
      ),
      'retrying' => Retrying(
        runId: run,
        attempt: j['attempt'] as int,
        delayMs: j['delay_ms'] as int,
        error: j['error'] as String,
      ),
      'compacted' => Compacted(runId: run),
      'usage' => UsageEvent(runId: run, usage: Usage.fromJson(j['usage'])),
      'run_finished' => RunFinished(
        runId: run,
        status: j['status'] as String,
        error: j['error'] as String?,
      ),
      _ => UnknownEvent(j),
    };
  }
}

/// An event about one run.
sealed class RunEvent extends TernEvent {
  const RunEvent(this.runId);
  final String runId;
}

class RunStarted extends RunEvent {
  const RunStarted({required String runId, required this.sessionId})
    : super(runId);
  final String sessionId;
}

class TextDelta extends RunEvent {
  const TextDelta({required String runId, required this.text}) : super(runId);
  final String text;
}

class AssistantMessage extends RunEvent {
  const AssistantMessage({
    required String runId,
    required this.text,
    required this.replayed,
  }) : super(runId);
  final String text;
  final bool replayed;
}

class ToolStarted extends RunEvent {
  const ToolStarted({
    required String runId,
    required this.id,
    required this.name,
    required this.summary,
  }) : super(runId);
  final String id;
  final String name;
  final String summary;
}

class ToolFinished extends RunEvent {
  const ToolFinished({
    required String runId,
    required this.id,
    required this.name,
    required this.isError,
    required this.preview,
    required this.output,
    required this.replayed,
  }) : super(runId);
  final String id;
  final String name;
  final bool isError;
  final String preview;
  final String output;
  final bool replayed;
}

/// How an approval step ended (`approved`, `denied`, `timeout`).
class ApprovalAnswered extends RunEvent {
  const ApprovalAnswered({
    required String runId,
    required this.id,
    required this.name,
    required this.outcome,
  }) : super(runId);
  final String id;
  final String name;
  final String outcome;
}

class Retrying extends RunEvent {
  const Retrying({
    required String runId,
    required this.attempt,
    required this.delayMs,
    required this.error,
  }) : super(runId);
  final int attempt;
  final int delayMs;
  final String error;
}

class Compacted extends RunEvent {
  const Compacted({required String runId}) : super(runId);
}

class UsageEvent extends RunEvent {
  const UsageEvent({required String runId, required this.usage}) : super(runId);
  final Usage usage;
}

class RunFinished extends RunEvent {
  const RunFinished({required String runId, required this.status, this.error})
    : super(runId);

  /// `completed`, `failed` or `interrupted`.
  final String status;
  final String? error;
}

class ApprovalRequested extends TernEvent {
  const ApprovalRequested(this.approval);
  final Approval approval;
}

class UnknownEvent extends TernEvent {
  const UnknownEvent(this.json);
  final Json json;
}
