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
  const Info({
    this.defaultModel,
    required this.tools,
    this.setupNeeded = false,
  });

  factory Info.fromJson(Json j) => Info(
    defaultModel: j['default_model'] as String?,
    setupNeeded: j['setup_needed'] as bool? ?? false,
    tools: [
      for (final t in j['tools'] as List)
        (name: (t as List)[0] as String, description: t[1] as String),
    ],
  );

  final String? defaultModel;
  final List<({String name, String description})> tools;

  /// No usable model yet: offer provider setup.
  final bool setupNeeded;
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
  const ActiveRun({
    required this.runId,
    required this.sessionId,
    this.parentRunId,
    this.label,
  });

  factory ActiveRun.fromJson(Json j) => ActiveRun(
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    parentRunId: j['parent_run_id'] as String?,
    label: j['label'] as String?,
  );

  final String runId;
  final String sessionId;

  /// Set for sub-agents.
  final String? parentRunId;
  final String? label;
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
    this.runId,
    required this.content,
    required this.summary,
  });

  factory SessionMessage.fromJson(Json j) => SessionMessage(
    role: j['role'] as String,
    runId: j['run_id'] as String?,
    content: [
      for (final b in j['content'] as List) ContentBlock.fromJson(b as Json),
    ],
    summary: j['summary'] as bool? ?? false,
  );

  final String role;

  /// The run that wrote it; null for messages from before runs were
  /// recorded, or a user message that started no run.
  final String? runId;
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
    this.parentRunId,
    this.agentLabel,
    this.rootSessionId,
  });

  factory Approval.fromJson(Json j) => Approval(
    id: j['id'] as String,
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    tool: j['tool'] as String,
    input: j['input'],
    summary: j['summary'] as String,
    pattern: j['pattern'] as String?,
    parentRunId: j['parent_run_id'] as String?,
    agentLabel: j['agent_label'] as String?,
    rootSessionId: j['root_session_id'] as String?,
  );

  final String id;
  final String runId;
  final String sessionId;

  /// Set when a sub-agent asks: its parent run, its label, and the
  /// conversation the user started (where the card belongs).
  final String? parentRunId;
  final String? agentLabel;
  final String? rootSessionId;
  final String tool;
  final Object? input;
  final String summary;

  /// What "always allow" adds to the allowlist, or null if this call
  /// can't be always-allowed (hide the option).
  final String? pattern;
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
    if (j.containsKey('providers_changed')) return const ProvidersChanged();
    if (j['models_changed'] case final Json m) {
      return ModelsChanged(m['provider'] as String? ?? '');
    }
    final run = j['run_id'] as String? ?? '';
    final event = _agentEvent(j, run);
    if (event is RunEvent) {
      if (j['session_id'] case final String s) _sessions[event] = s;
      if (j['parent_run_id'] case final String p) _parents[event] = p;
      if (j['root_session_id'] case final String r) _roots[event] = r;
    }
    return event;
  }

  static TernEvent _agentEvent(Json j, String run) {
    return switch (j['type']) {
      'run_started' => RunStarted(
        runId: run,
        sessionId: j['session_id'] as String,
        parentRunId: j['parent_run_id'] as String?,
      ),
      'subagent_spawned' => SubagentSpawned(
        runId: run,
        callId: j['call_id'] as String,
        childRunId: j['child_run_id'] as String,
        childSessionId: j['child_session_id'] as String,
        label: j['label'] as String,
        task: j['task'] as String? ?? '',
        model: j['model'] as String? ?? '',
        status: j['status'] as String? ?? 'running',
      ),
      'subagent_message' => SubagentMessage(
        runId: run,
        toRunId: j['to_run_id'] as String,
        kind: j['kind'] as String,
        text: j['text'] as String? ?? '',
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
      'tool_output' => ToolOutput(
        runId: run,
        id: j['id'] as String,
        chunk: j['chunk'] as String? ?? '',
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

  /// The run's session, which the daemon adds to every event it sends.
  /// Null for events built by hand.
  String? get sessionId => _sessions[this];

  /// For a sub-agent's run: the run that started it, and the conversation
  /// at the root of the tree. The daemon adds both.
  String? get parentRunId => _parents[this];
  String? get rootSessionId => _roots[this];
}

// Kept beside the event rather than in every constructor, so events can
// stay const for tests and hand-built values.
final _sessions = Expando<String>('session_id');
final _parents = Expando<String>('parent_run_id');
final _roots = Expando<String>('root_session_id');

class RunStarted extends RunEvent {
  const RunStarted({
    required String runId,
    required this.sessionId,
    String? parentRunId,
  }) : _parentRunId = parentRunId,
       super(runId);
  final String sessionId;
  final String? _parentRunId;

  @override
  String? get parentRunId => _parentRunId ?? super.parentRunId;
}

/// A run (the parent) started a sub-agent with `agent_spawn`.
class SubagentSpawned extends RunEvent {
  const SubagentSpawned({
    required String runId,
    required this.callId,
    required this.childRunId,
    required this.childSessionId,
    required this.label,
    required this.task,
    required this.model,
    required this.status,
  }) : super(runId);

  /// The `agent_spawn` tool call.
  final String callId;
  final String childRunId;
  final String childSessionId;
  final String label;

  /// The start of the brief.
  final String task;
  final String model;

  /// `running` or `queued`.
  final String status;
}

/// A question, answer or steering message between runs of one tree.
/// [runId] sent it.
class SubagentMessage extends RunEvent {
  const SubagentMessage({
    required String runId,
    required this.toRunId,
    required this.kind,
    required this.text,
  }) : super(runId);
  final String toRunId;

  /// `question`, `answer` or `message`.
  final String kind;
  final String text;
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

/// Live output of a running tool call; never replayed. The full output
/// still arrives in [ToolFinished].
class ToolOutput extends RunEvent {
  const ToolOutput({
    required String runId,
    required this.id,
    required this.chunk,
  }) : super(runId);
  final String id;
  final String chunk;
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

/// Providers or the default model changed; re-read them.
class ProvidersChanged extends TernEvent {
  const ProvidersChanged();
}

/// A provider's model list was refreshed.
class ModelsChanged extends TernEvent {
  const ModelsChanged(this.provider);
  final String provider;
}

class UnknownEvent extends TernEvent {
  const UnknownEvent(this.json);
  final Json json;
}

/// A routine: a prompt the daemon runs on a cron schedule.
class Schedule {
  const Schedule({
    required this.id,
    required this.cron,
    required this.prompt,
    required this.sessionId,
    this.deliverTo,
    required this.enabled,
    this.nextRunAt,
    this.lastRunAt,
    this.lastStatus,
  });

  factory Schedule.fromJson(Json j) => Schedule(
    id: j['id'] as String,
    cron: j['cron'] as String,
    prompt: j['prompt'] as String,
    sessionId: j['session_id'] as String,
    deliverTo: j['deliver_to'] as String?,
    enabled: j['enabled'] as bool,
    nextRunAt: _maybeTime(j['next_run_at']),
    lastRunAt: _maybeTime(j['last_run_at']),
    lastStatus: j['last_status'] as String?,
  );

  final String id;
  final String cron;
  final String prompt;

  /// Each schedule runs in a session of its own (source `cron`).
  final String sessionId;

  /// Null keeps results in the session; else `telegram:<chat_id>`.
  final String? deliverTo;
  final bool enabled;
  final DateTime? nextRunAt;
  final DateTime? lastRunAt;
  final String? lastStatus;
}

class CronPreview {
  const CronPreview({required this.cron, required this.next});

  factory CronPreview.fromJson(Json j) => CronPreview(
    cron: j['cron'] as String,
    next: [for (final t in j['next'] as List) _time(t as int)],
  );

  /// The expression as the daemon normalizes it.
  final String cron;
  final List<DateTime> next;
}

class RunItem {
  const RunItem({
    required this.id,
    required this.status,
    required this.trigger,
    this.model,
    required this.startedAt,
    this.finishedAt,
    this.error,
    this.parentRunId,
    this.label,
    this.depth = 0,
  });

  factory RunItem.fromJson(Json j) => RunItem(
    id: j['id'] as String,
    status: j['status'] as String,
    trigger: j['trigger'] as String,
    model: j['model'] as String?,
    startedAt: _time(j['started_at'] as int),
    finishedAt: _maybeTime(j['finished_at']),
    error: j['error'] as String?,
    parentRunId: j['parent_run_id'] as String?,
    label: j['label'] as String?,
    depth: j['depth'] as int? ?? 0,
  );

  final String id;

  /// Set for sub-agents: the run that started it, its name, and how deep
  /// it is (0 for runs the user started).
  final String? parentRunId;
  final String? label;
  final int depth;

  /// `running`, `completed`, `failed` or `interrupted`.
  final String status;

  /// Who started it: `user`, `cron`, …
  final String trigger;
  final String? model;
  final DateTime startedAt;
  final DateTime? finishedAt;
  final String? error;
}

/// One journal step of a run.
class StepItem {
  const StepItem({
    required this.idx,
    required this.kind,
    required this.status,
    required this.input,
    this.output,
    this.usage,
    this.error,
    required this.startedAt,
    this.finishedAt,
  });

  factory StepItem.fromJson(Json j) => StepItem(
    idx: j['idx'] as int,
    kind: j['kind'] as String,
    status: j['status'] as String,
    input: j['input'],
    output: j['output'],
    usage: j['usage'] is Json ? Usage.fromJson(j['usage'] as Json) : null,
    error: j['error'] as String?,
    startedAt: _time(j['started_at'] as int),
    finishedAt: _maybeTime(j['finished_at']),
  );

  final int idx;

  /// `llm`, `tool`, `approval`, `compaction`, …
  final String kind;

  /// `started`, `completed` or `failed`.
  final String status;
  final Object? input;
  final Object? output;
  final Usage? usage;
  final String? error;
  final DateTime startedAt;
  final DateTime? finishedAt;

  Duration? get duration => finishedAt?.difference(startedAt);
}

/// A secret tern knows about. The API never returns values.
class SecretStatus {
  const SecretStatus({
    required this.name,
    required this.inFile,
    required this.inEnv,
    required this.usedBy,
  });

  factory SecretStatus.fromJson(Json j) => SecretStatus(
    name: j['name'] as String,
    inFile: j['in_file'] as bool,
    inEnv: j['in_env'] as bool,
    usedBy: [for (final u in j['used_by'] as List) u as String],
  );

  final String name;

  /// Stored in `secrets.toml`.
  final bool inFile;

  /// Set in the daemon's environment, which takes precedence over the file.
  final bool inEnv;

  /// What refers to it, e.g. `provider anthropic` or `connector hubspot`.
  final List<String> usedBy;

  bool get isSet => inFile || inEnv;
}

/// Secret names double as environment variable names.
final secretName = RegExp(r'^[A-Za-z_][A-Za-z0-9_]{0,127}$');

DateTime? _maybeTime(Object? t) => t is int ? _time(t) : null;
