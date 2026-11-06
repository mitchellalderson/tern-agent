// Types mirroring the daemon API (`crates/tern/src/daemon/api.rs`) and
// `tern_core::events::AgentEvent`. Unknown fields are ignored and unknown
// event types decode to [UnknownEvent], so a newer daemon doesn't break
// an older app.

import 'channels.dart';
import 'connections.dart';
import 'mcp.dart';
import 'secret_requests.dart';

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

  /// The core has workspaces (`/v1/workspaces`): older cores don't, so hide
  /// the folder picker.
  bool get hasWorkspaces => features.contains('workspaces');

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
    this.gateways = const [],
    this.sandbox,
  });

  factory Info.fromJson(Json j) => Info(
    sandbox: switch (j['sandbox']) {
      final Map<String, dynamic> s => (
        verified: s['verified'] as bool? ?? false,
        detail: s['detail'] as String? ?? '',
      ),
      _ => null,
    },
    defaultModel: j['default_model'] as String?,
    setupNeeded: j['setup_needed'] as bool? ?? false,
    tools: [
      for (final t in j['tools'] as List)
        (name: (t as List)[0] as String, description: t[1] as String),
    ],
    gateways: [
      for (final g in (j['gateways'] as List?) ?? const [])
        (
          name: (g as Json)['name'] as String,
          kind: g['kind'] as String? ?? g['name'] as String,
        ),
    ],
  );

  final String? defaultModel;
  final List<({String name, String description})> tools;

  /// Where routines can deliver (`<name>:<chat>`): Telegram and gateway
  /// connectors such as ntfy or Slack. Empty from older daemons.
  final List<({String name, String kind})> gateways;

  /// No usable model yet: offer provider setup.
  final bool setupNeeded;

  /// Whether the OS sandbox is confirmed active for shell commands (python
  /// script grants apply only then), and a line saying what or why not.
  /// Null from older daemons.
  final ({bool verified, String detail})? sandbox;
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
    this.channelId,
    this.authorId,
  });

  factory ActiveRun.fromJson(Json j) => ActiveRun(
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    parentRunId: j['parent_run_id'] as String?,
    label: j['label'] as String?,
    channelId: j['channel_id'] as String?,
    authorId: j['author_id'] as String?,
  );

  final String runId;
  final String sessionId;

  /// Set for sub-agents.
  final String? parentRunId;
  final String? label;

  /// The channel of its session.
  final String? channelId;

  /// Who is working, as posts name them: `tern`, `tern/<label>`.
  final String? authorId;
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
    this.channelId,
    this.prevSessionId,
    this.segmentReason,
    this.branchedFrom,
    this.movedFrom,
  });

  factory SessionItem.fromJson(Json j) => SessionItem(
    id: j['id'] as String,
    title: j['title'] as String?,
    source: j['source'] as String,
    model: j['model'] as String?,
    messages: j['messages'] as int,
    runs: j['runs'] as int,
    updatedAt: _time(j['updated_at'] as int),
    channelId: j['channel_id'] as String?,
    prevSessionId: j['prev_session_id'] as String?,
    segmentReason: j['segment_reason'] as String?,
    branchedFrom: j['branched_from'] as String?,
    movedFrom: j['moved_from'] as String?,
  );

  final String id;
  final String? title;

  /// Where the session came from: `api`, `cli`, `telegram`, `cron`, …
  final String source;
  final String? model;
  final int messages;
  final int runs;
  final DateTime updatedAt;

  /// The channel it's a thread of.
  final String? channelId;

  /// A segment of a stream: the one before it, and why it was cut
  /// (`idle`, `day`, `new_topic`, `migrated`).
  final String? prevSessionId;
  final String? segmentReason;

  /// A side chat: `message:<id>` or `post:<id>`.
  final String? branchedFrom;

  /// Moved here from this channel.
  final String? movedFrom;
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
    'thinking' => ReasoningBlock(
      text: j['thinking'] as String? ?? '',
      durationMs: j['duration_ms'] as int?,
    ),
    'redacted_thinking' => const ReasoningBlock(text: '', hidden: true),
    _ => const TextBlock(''),
  };
}

/// The model's reasoning for one turn, as stored with the message. A
/// redacted block (the provider withheld it) has no text.
class ReasoningBlock extends ContentBlock {
  const ReasoningBlock({
    required this.text,
    this.hidden = false,
    this.durationMs,
  });
  final String text;
  final bool hidden;
  final int? durationMs;
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
    int? total,
    this.offset = 0,
  }) : _total = total;

  factory SessionDetail.fromJson(Json j) {
    final messages = [
      for (final m in j['messages'] as List) SessionMessage.fromJson(m as Json),
    ];
    final offset = (j['offset'] as num?)?.toInt() ?? 0;
    return SessionDetail(
      id: j['id'] as String,
      title: j['title'] as String?,
      model: j['model'] as String?,
      messages: messages,
      // An older daemon sends the whole session and no counts.
      total: (j['total'] as num?)?.toInt() ?? messages.length + offset,
      offset: offset,
    );
  }

  final String id;
  final String? title;
  final String? model;
  final List<SessionMessage> messages;

  /// Messages the session has in all, and the index of `messages[0]`
  /// among them (0 unless this is a page of a long session).
  int get total => _total ?? messages.length + offset;
  final int? _total;
  final int offset;

  /// Messages before this page that were not sent.
  bool get hasEarlier => offset > 0;
}

class Usage {
  const Usage({
    this.inputTokens = 0,
    this.outputTokens = 0,
    this.cacheReadTokens = 0,
    this.cacheWriteTokens = 0,
    this.contextTokens,
    this.contextWindow,
    this.compactAt,
  });

  factory Usage.fromJson(Json j) => Usage(
    inputTokens: j['input_tokens'] as int? ?? 0,
    outputTokens: j['output_tokens'] as int? ?? 0,
    cacheReadTokens: j['cache_read_tokens'] as int? ?? 0,
    cacheWriteTokens: j['cache_write_tokens'] as int? ?? 0,
    contextTokens: j['context_tokens'] as int?,
    contextWindow: j['context_window'] as int?,
    compactAt: j['compact_at'] as int?,
  );

  final int inputTokens;
  final int outputTokens;
  final int cacheReadTokens;
  final int cacheWriteTokens;

  /// How full the context was at the latest model call, out of
  /// [contextWindow]; only from `GET /v1/sessions/{id}/usage`, and absent
  /// before the first call or from an older daemon.
  final int? contextTokens;
  final int? contextWindow;

  /// Where the agent starts summarizing older messages.
  final int? compactAt;

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
    this.effect = 'write',
    required this.pattern,
    this.parentRunId,
    this.agentLabel,
    this.rootSessionId,
    this.taint,
    this.channelId,
    this.agentId,
    this.egressHost,
    this.script,
    this.grant,
    this.notice,
    this.count = 1,
  });

  factory Approval.fromJson(Json j) => Approval(
    id: j['id'] as String,
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    tool: j['tool'] as String,
    input: j['input'],
    summary: j['summary'] as String,
    effect: j['effect'] as String? ?? 'write',
    pattern: j['pattern'] as String?,
    parentRunId: j['parent_run_id'] as String?,
    agentLabel: j['agent_label'] as String?,
    rootSessionId: j['root_session_id'] as String?,
    taint: j['taint'] as String?,
    channelId: j['channel_id'] as String?,
    agentId: j['agent_id'] as String?,
    egressHost: j['egress_host'] as String?,
    script: j['script'] as String?,
    grant: switch (j['grant']) {
      final Map<String, dynamic> g => GrantOffer.fromJson(g),
      _ => null,
    },
    notice: j['notice'] as String?,
    count: (j['count'] as num?)?.toInt() ?? 1,
  );

  /// The channel of the conversation the user started (agent profiles):
  /// an agent's DM shows its card as well as Home.
  final String? channelId;
  final String? agentId;

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

  /// `read`, `write` or `destructive`. Older daemons send none: `write`.
  final String effect;

  /// `write · gmail_send …`: the effect, then the summary.
  String get headline => scriptNote == null
      ? '$effect · $summary'
      : '$effect · $summary ($scriptNote)';

  /// Set when a `python` script's own tool call asks: "from a python script
  /// (call 7 of this script)", a newline, then the script.
  final String? script;

  /// "from a python script (call 7 of this script)", or null.
  String? get scriptNote => script?.split('\n').first;

  /// The script that made the call, for the detail view, or null.
  String? get scriptText {
    final i = script?.indexOf('\n') ?? -1;
    return i < 0 ? null : script!.substring(i + 1);
  }

  /// What "always allow" adds to the allowlist, or null if this call
  /// can't be always-allowed (hide the option).
  final String? pattern;

  /// Set when the run is tainted (it read third-party text): why. Grants
  /// don't cover its writes, so it asks.
  final String? taint;

  /// "this run read untrusted content (…)", for the card.
  String? get taintNote =>
      taint == null ? null : 'this run read untrusted content ($taint)';

  /// Set when it asks only because a tainted run wants a site it hasn't
  /// reached yet: that site. Then [Decision.run] and [Decision.host] can
  /// answer it too.
  final String? egressHost;

  /// Set when scoped grants may be offered: what they would cover.
  /// [Decision.run] (this run), [Decision.session] (this conversation) and
  /// [Decision.always] then apply to exactly this, never to a different
  /// tool, draft or set of hosts. Null in tainted runs and for destructive
  /// calls.
  final GrantOffer? grant;

  /// A plain-words line from tern (never the agent's) for the card: what an
  /// offered grant does not cover ("Imports from other files aren't
  /// covered"), or why none is offered ("Not auto-approved: the sandbox
  /// isn't confirmed active").
  final String? notice;

  /// How many identical simultaneous requests this one card answers. A
  /// card whose count grows arrives again with the same [id].
  final int count;
}

/// What a scoped grant would cover.
class GrantOffer {
  const GrantOffer({
    required this.key,
    required this.label,
    this.runOnly = false,
  });

  factory GrantOffer.fromJson(Json j) => GrantOffer(
    key: j['key'] as String,
    label: j['label'] as String,
    runOnly: j['run_only'] as bool? ?? false,
  );

  /// Only "for this run" may be offered: a tainted run's read scope (the
  /// read-only file tools under a folder, for the run and its sub-agents).
  /// "This conversation" and "always" are never available for it.
  final bool runOnly;

  final String key;

  /// `draft maricopa_assessor.city_stats`.
  final String label;
}

enum Decision {
  once,
  always,

  /// The tainted host limit: no more site prompts in this run. With an
  /// [Approval.grant]: don't ask again about exactly that in this run.
  run,

  /// With an [Approval.grant]: don't ask again about exactly that in this
  /// conversation.
  session,

  /// The tainted host limit: always allow reads from this site.
  host,
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
    if (j['secret_requested'] case final Json s) {
      return SecretRequested(SecretRequest.fromJson(s));
    }
    if (j['auth_prompt'] case final Json a) {
      return AuthPromptOpened(AuthPrompt.fromJson(a));
    }
    if (j['auth_prompt_closed'] case final String id) {
      return AuthPromptClosed(id);
    }
    if (j.containsKey('connections_changed')) {
      return const ConnectionsChanged();
    }
    if (j.containsKey('providers_changed')) return const ProvidersChanged();
    if (j['models_changed'] case final Json m) {
      return ModelsChanged(m['provider'] as String? ?? '');
    }
    if (j['mcp_changed'] case final Json m) {
      return McpChanged(
        m['name'] as String? ?? '',
        m['state'] is Json ? McpState.fromJson(m['state'] as Json) : null,
      );
    }
    if (j['post_updated'] case final Json p) {
      return PostUpdated(Post.fromJson(p['post'] as Json));
    }
    if (j['post_created'] case final Json p) {
      return PostCreated(Post.fromJson(p['post'] as Json));
    }
    if (j.containsKey('channels_changed')) return const ChannelsChanged();
    if (j.containsKey('agents_changed')) return const AgentsChanged();
    if (j['events_dropped'] case final Json d) {
      return EventsDropped(d['missed'] as int? ?? 0);
    }
    if (j['outbox_changed'] case final Json o) {
      return OutboxChanged(o['id'] as int, o['state'] as String? ?? '');
    }
    if (j['schedules_changed'] case final Json s) {
      return SchedulesChanged(s['id'] as String? ?? '');
    }
    final run = j['run_id'] as String? ?? '';
    final event = _agentEvent(j, run);
    if (event is RunEvent) {
      if (j['session_id'] case final String s) _sessions[event] = s;
      if (j['channel_id'] case final String c) _channels[event] = c;
      if (j['agent_id'] case final String a) _agents[event] = a;
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
        agent: j['agent'] as String?,
      ),
      'subagent_message' => SubagentMessage(
        runId: run,
        toRunId: j['to_run_id'] as String,
        kind: j['kind'] as String,
        text: j['text'] as String? ?? '',
      ),
      'text_delta' => TextDelta(runId: run, text: j['text'] as String),
      'reasoning_delta' => ReasoningDelta(
        runId: run,
        text: j['text'] as String? ?? '',
      ),
      'reasoning_hidden' => ReasoningHidden(runId: run),
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
      'checkpoint' => CheckpointSaved(
        runId: run,
        commit: j['commit'] as String?,
        skipped: j['skipped'] as String?,
      ),
      'usage' => UsageEvent(runId: run, usage: Usage.fromJson(j['usage'])),
      'run_finished' => RunFinished(
        runId: run,
        status: j['status'] as String,
        error: j['error'] as String?,
        quiet: j['quiet'] as bool? ?? false,
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

  /// The channel of the run's session, which the daemon adds.
  String? get channelId => _channels[this];

  /// The agent profile of the run's session (`tern` by default), which the
  /// daemon adds.
  String? get agentId => _agents[this];
}

// Kept beside the event rather than in every constructor, so events can
// stay const for tests and hand-built values.
final _sessions = Expando<String>('session_id');
final _parents = Expando<String>('parent_run_id');
final _roots = Expando<String>('root_session_id');
final _channels = Expando<String>('channel_id');
final _agents = Expando<String>('agent_id');

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
    this.agent,
  }) : super(runId);

  /// The agent profile it runs as, when the parent delegated to one.
  final String? agent;

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

/// Streaming reasoning from a live model call, apart from the answer.
class ReasoningDelta extends RunEvent {
  const ReasoningDelta({required String runId, required this.text})
    : super(runId);
  final String text;
}

/// The provider withheld this turn's reasoning.
class ReasoningHidden extends RunEvent {
  const ReasoningHidden({required String runId}) : super(runId);
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

/// Code auto mode took (or couldn't take) its snapshot before the run's
/// first unasked change: [commit] when it did, else [skipped] says why not.
class CheckpointSaved extends RunEvent {
  const CheckpointSaved({required String runId, this.commit, this.skipped})
    : super(runId);
  final String? commit;
  final String? skipped;
}

/// A file a code auto mode run changed since its snapshot.
class RunChange {
  const RunChange({required this.path, required this.kind});
  factory RunChange.fromJson(Json j) =>
      RunChange(path: j['path'] as String, kind: j['kind'] as String);
  final String path;

  /// `added`, `modified` or `deleted`.
  final String kind;
}

/// What a run changed since its snapshot (`GET /v1/runs/{id}/changes`), or
/// what an undo reverted. [skipped] says why there is no snapshot.
class RunChanges {
  const RunChanges({this.changes = const [], this.skipped});
  factory RunChanges.fromJson(Json j) => RunChanges(
    changes: [
      for (final c in (j['changes'] as List<dynamic>? ?? const []))
        RunChange.fromJson(c as Json),
    ],
    skipped: j['skipped'] as String?,
  );
  final List<RunChange> changes;
  final String? skipped;
}

class UsageEvent extends RunEvent {
  const UsageEvent({required String runId, required this.usage}) : super(runId);
  final Usage usage;
}

class RunFinished extends RunEvent {
  const RunFinished({
    required String runId,
    required this.status,
    this.error,
    this.quiet = false,
  }) : super(runId);

  /// `completed`, `failed` or `interrupted`.
  final String status;
  final String? error;

  /// A scheduled run that answered `NO_REPLY`: nothing to notify about.
  final bool quiet;
}

class ApprovalRequested extends TernEvent {
  const ApprovalRequested(this.approval);
  final Approval approval;
}

class SecretRequested extends TernEvent {
  const SecretRequested(this.request);
  final SecretRequest request;
}

/// A sign-in or connect card wants the user.
class AuthPromptOpened extends TernEvent {
  const AuthPromptOpened(this.prompt);
  final AuthPrompt prompt;
}

/// Close that card: answered elsewhere, or the sign-in finished.
class AuthPromptClosed extends TernEvent {
  const AuthPromptClosed(this.id);
  final String id;
}

/// An account was added, changed or removed; re-read `/v1/connections`.
class ConnectionsChanged extends TernEvent {
  const ConnectionsChanged();
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

/// An MCP server changed state or config; [state] is null once it's
/// removed. Re-read `/v1/mcp` (and the tool list).
class McpChanged extends TernEvent {
  const McpChanged(this.name, this.state);
  final String name;
  final McpState? state;
}

/// A post was written to a channel.
class PostCreated extends TernEvent {
  const PostCreated(this.post);
  final Post post;
}

/// A post the daemon keeps current was rewritten (a delegation's pointer).
class PostUpdated extends TernEvent {
  const PostUpdated(this.post);
  final Post post;
}

/// A channel was created, changed, archived, or got a new main thread
/// (Home: a new segment). Re-read `/v1/channels`.
class ChannelsChanged extends TernEvent {
  const ChannelsChanged();
}

/// An agent profile was created, edited or archived.
class AgentsChanged extends TernEvent {
  const AgentsChanged();
}

/// The daemon skipped [missed] events this client was too slow to take
/// (a `run_finished` may be among them): re-read the active runs.
class EventsDropped extends TernEvent {
  const EventsDropped(this.missed);
  final int missed;
}

/// An outbox item was queued or changed state; re-read the outbox.
class OutboxChanged extends TernEvent {
  const OutboxChanged(this.id, this.state);
  final int id;
  final String state;
}

/// A routine was created, changed or deleted (by an agent's tool or the
/// API); re-read the routines.
class SchedulesChanged extends TernEvent {
  const SchedulesChanged(this.id);
  final String id;
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
    this.lastQuiet = false,
    this.limits = const ScheduleLimits(),
    this.paused,
    this.pausedUntil,
    this.pausedReason,
    this.spentUsd,
    this.usdCap,
    this.channelId = 'home',
    this.agentId = 'tern',
    this.trigger = 'cron',
    this.at,
    this.watch,
    this.everySecs,
    this.lastCheck,
    this.initiative,
    this.gate,
    this.gateConnector,
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
    lastQuiet: j['last_quiet'] as bool? ?? false,
    limits: ScheduleLimits.fromJson(j),
    paused: j['paused'] as String?,
    pausedUntil: _maybeTime(j['paused_until']),
    pausedReason: j['paused_reason'] as String?,
    spentUsd: (j['spent_usd'] as num?)?.toDouble(),
    usdCap: (j['usd_cap'] as num?)?.toDouble(),
    channelId: j['channel_id'] as String? ?? 'home',
    agentId: j['agent_id'] as String? ?? 'tern',
    trigger: j['trigger'] as String? ?? 'cron',
    at: _maybeTime(j['at']),
    watch: j['watch'] as String?,
    everySecs: j['every_secs'] as int?,
    lastCheck: j['last_check'] as String?,
    initiative: j['initiative'] as String?,
    gate: j['gate'] as String?,
    gateConnector: j['gate_connector'] as String?,
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

  /// The latest run answered `NO_REPLY` and delivered nothing.
  final bool lastQuiet;

  /// Quiet hours, urgency and caps.
  final ScheduleLimits limits;

  /// `budget` while a daily cap has paused it (it stays [enabled]).
  final String? paused;

  /// When a budget pause ends on its own.
  final DateTime? pausedUntil;

  /// Which cap paused it: `runs_per_day` or `usd_per_day`.
  final String? pausedReason;

  /// USD spent today and the daily cap it counts against; both null when
  /// there is no cap or no known price.
  final double? spentUsd;
  final double? usdCap;

  /// The channel its results are posted to.
  final String channelId;

  /// The agent whose job it is.
  final String agentId;

  bool get pausedByBudget => paused == 'budget';

  /// What starts it (T1b): `cron`, `once` (at [at]) or `watch` ([watch],
  /// polled every [everySecs]).
  final String trigger;
  final DateTime? at;

  /// `<connector>.<watch>`.
  final String? watch;
  final int? everySecs;

  /// What the last watch poll or gate found, e.g. "no new events".
  final String? lastCheck;

  /// `silent`, `digest`, `notify` or `interrupt`; null = the default.
  final String? initiative;

  /// Luau run before each cron or once slot; false skips the run.
  final String? gate;
  final String? gateConnector;
}

/// A routine's trigger beyond a cron, its initiative and its gate (T1b).
class RoutineOptions {
  const RoutineOptions({
    this.at,
    this.watch,
    this.every,
    this.initiative,
    this.gate,
    this.gateConnector,
  });

  /// Once, at a local `YYYY-MM-DD HH:MM`.
  final String? at;

  /// `<connector>.<watch>`, polled [every] (`5m`; null = the watch's own).
  final String? watch;
  final String? every;

  /// Null = the daemon's default.
  final String? initiative;
  final String? gate;
  final String? gateConnector;

  /// Not a cron: the request leaves `cron` out.
  bool get replacesCron => at != null || watch != null;

  /// Every field: nulls put the default back (and remove a gate).
  Json toJson() => {
    'at': ?at,
    'watch': ?watch,
    'every': ?every,
    'initiative': initiative ?? 'default',
    'gate': gate,
    'gate_connector': gateConnector,
  };
}

/// A routine's quiet hours, urgency and caps. Null means the daemon's
/// `[schedules]` default.
class ScheduleLimits {
  const ScheduleLimits({
    this.quietHours,
    this.urgent = false,
    this.runsPerDay,
    this.usdPerDay,
    this.tokensPerRun,
  });

  factory ScheduleLimits.fromJson(Json j) => ScheduleLimits(
    quietHours: j['quiet_hours'] as String?,
    urgent: j['urgent'] as bool? ?? false,
    runsPerDay: j['runs_per_day'] as int?,
    usdPerDay: (j['usd_per_day'] as num?)?.toDouble(),
    tokensPerRun: j['tokens_per_run'] as int?,
  );

  /// `HH:MM-HH:MM` (local) or `none`.
  final String? quietHours;

  /// Delivers even during quiet hours.
  final bool urgent;
  final int? runsPerDay;
  final double? usdPerDay;
  final int? tokensPerRun;

  /// Every field, nulls included: a null puts the default back.
  Json toJson() => {
    'quiet_hours': quietHours,
    'urgent': urgent,
    'runs_per_day': runsPerDay,
    'usd_per_day': usdPerDay,
    'tokens_per_run': tokensPerRun,
  };
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
    this.taint,
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
    taint: j['taint'] as String?,
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

  /// Why the run is tainted (it read third-party text, so its writes ask);
  /// null while clean.
  final String? taint;
  bool get tainted => taint != null;
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
