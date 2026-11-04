import 'models.dart';

/// An agent profile (docs/agent-profiles-design.md): a configurable agent
/// with its own persona, model, tools, memory, routines and DM.
class Agent {
  const Agent({
    required this.id,
    required this.slug,
    required this.name,
    this.avatar,
    this.model = const [],
    this.subagentModel = const [],
    this.tools,
    this.memory = 'private',
    this.egress,
    this.mainChannelId,
    this.lastActivity,
    this.needsYou = 0,
    this.activeRuns = 0,
    this.archived = false,
    this.deleted = false,
    this.isDefault = false,
    this.delegates,
  });

  factory Agent.fromJson(Json j) => Agent(
    id: j['id'] as String,
    slug: j['slug'] as String,
    name: j['name'] as String,
    avatar: j['avatar'] is Json
        ? AvatarSpec.fromJson(j['avatar'] as Json)
        : null,
    model: chainFromJson(j['model']),
    subagentModel: chainFromJson(j['subagent_model']),
    tools: j['tools'] is Json ? ToolScope.fromJson(j['tools'] as Json) : null,
    memory: j['memory'] as String? ?? 'private',
    egress: j['egress'] as String?,
    mainChannelId: j['main_channel_id'] as String?,
    lastActivity: j['last_activity'] is int
        ? DateTime.fromMillisecondsSinceEpoch(j['last_activity'] as int)
        : null,
    needsYou: j['needs_you'] as int? ?? 0,
    activeRuns: j['active_runs'] as int? ?? 0,
    archived: j['archived'] as bool? ?? false,
    deleted: j['deleted'] as bool? ?? false,
    isDefault: j['is_default'] as bool? ?? j['id'] == defaultId,
    delegates: switch (j['delegates']) {
      final List<dynamic> l => [for (final d in l) d as String],
      _ => null,
    },
  );

  /// The default agent: today's setup, whose DM is Home.
  static const defaultId = 'tern';

  /// `tern`, or `tern:<slug at creation>`; never changes.
  final String id;

  /// Its @name; can be renamed.
  final String slug;
  final String name;

  /// Null: generated from the id, as [id] picks a colour.
  final AvatarSpec? avatar;

  /// A model, or a fallback chain; empty = the default model.
  final List<String> model;
  final List<String> subagentModel;

  /// Null = every tool (the default agent).
  final ToolScope? tools;

  /// `shared` or `private`.
  final String memory;

  /// After untrusted content, whether reading a new site asks (`ask`) or
  /// not (`allow`); null = the configured default.
  final String? egress;

  /// Its DM: `home` for the default agent.
  final String? mainChannelId;
  final DateTime? lastActivity;

  /// Approvals and secret requests from its runs.
  final int needsYou;
  final int activeRuns;
  final bool archived;

  /// Deleted: only listed by `agents(deleted: true)`, so history can still
  /// name what it wrote.
  final bool deleted;
  final bool isDefault;

  /// Agent ids it may hand work to (`agent_spawn {agent}`). Null: every
  /// agent for the default agent, none for others.
  final List<String>? delegates;

  bool get privateMemory => memory == 'private';

  /// Whether it may hand work to `other`.
  bool mayDelegateTo(Agent other) =>
      other.id != id &&
      !other.archived &&
      (delegates?.contains(other.id) ?? isDefault);
}

/// `"anthropic:x"`, `["a:x", "b:y"]` or null, as a list.
List<String> chainFromJson(Object? v) => switch (v) {
  final String s => [s],
  final List<dynamic> l => [for (final m in l) m as String],
  _ => const [],
};

/// A glyph and one of the eight avatar colours (gui-spec §8).
class AvatarSpec {
  const AvatarSpec({required this.glyph, required this.color});

  factory AvatarSpec.fromJson(Json j) =>
      AvatarSpec(glyph: j['glyph'] as String, color: j['color'] as String);

  final String glyph;

  /// `amber`, `teal`, `violet`, `pink`, `blue`, `orange`, `purple`, `green`.
  final String color;

  Json toJson() => {'glyph': glyph, 'color': color};
}

/// One key of a tool scope: everything, read-only built-ins, or names.
class Pick {
  const Pick.all() : all = true, readOnly = false, names = const [];
  const Pick.readOnly() : all = false, readOnly = true, names = const [];
  const Pick.names(this.names) : all = false, readOnly = false;

  factory Pick.fromJson(Object? v) => switch (v) {
    'read_only' => const Pick.readOnly(),
    final List<dynamic> l => Pick.names([for (final n in l) n as String]),
    _ => const Pick.all(),
  };

  final bool all;
  final bool readOnly;
  final List<String> names;

  bool allows(String name) => all || names.contains(name);

  Object toJson() => all
      ? 'all'
      : readOnly
      ? 'read_only'
      : names;
}

/// What an agent may use (§1.2). A missing key is `all`.
class ToolScope {
  const ToolScope({
    this.builtins = const Pick.all(),
    this.connectors = const Pick.all(),
    this.mcp = const Pick.all(),
    this.skills = const Pick.all(),
  });

  factory ToolScope.fromJson(Json j) => ToolScope(
    builtins: Pick.fromJson(j['builtins']),
    connectors: Pick.fromJson(j['connectors']),
    mcp: Pick.fromJson(j['mcp']),
    skills: Pick.fromJson(j['skills']),
  );

  final Pick builtins;
  final Pick connectors;
  final Pick mcp;
  final Pick skills;

  Json toJson() => {
    'builtins': builtins.toJson(),
    'connectors': connectors.toJson(),
    'mcp': mcp.toJson(),
    'skills': skills.toJson(),
  };
}

/// A starting point for a new agent: it only fills the sheet.
class AgentTemplate {
  const AgentTemplate({
    required this.id,
    required this.name,
    required this.description,
    required this.persona,
    this.category = '',
    this.tools,
    this.memory = 'private',
    this.routine,
    this.avatar,
  });

  factory AgentTemplate.fromJson(Json j) => AgentTemplate(
    id: j['id'] as String,
    name: j['name'] as String? ?? '',
    description: j['description'] as String? ?? '',
    persona: j['persona'] as String? ?? '',
    category: j['category'] as String? ?? '',
    tools: j['tools'] is Json ? ToolScope.fromJson(j['tools'] as Json) : null,
    memory: j['memory'] as String? ?? 'private',
    routine: switch (j['routine']) {
      [final String cron, final String prompt] => (cron: cron, prompt: prompt),
      _ => null,
    },
    avatar: switch (j['avatar']) {
      [final String glyph, final String color] => AvatarSpec(
        glyph: glyph,
        color: color,
      ),
      _ => null,
    },
  );

  final String id;
  final String name;
  final String description;

  /// What the picker groups starters by ("Coding", "Everyday", ...).
  final String category;

  /// May contain `{name}`, replaced with the agent's name.
  final String persona;

  /// Null: the narrow default the core picks.
  final ToolScope? tools;
  final String memory;
  final ({String cron, String prompt})? routine;
  final AvatarSpec? avatar;

  /// What a search over the starters matches: name, description, category.
  String get searchText => '$name $description $category $id'.toLowerCase();
}

/// An agent's persona and, from GET, what tern adds under every persona.
class Persona {
  const Persona({required this.persona, this.systemSections});

  factory Persona.fromJson(Json j) => Persona(
    persona: j['persona'] as String? ?? '',
    systemSections: j['system_sections'] as String?,
  );

  final String persona;
  final String? systemSections;
}

/// A long-term memory: shared, or an agent's own.
class Memory {
  const Memory({
    required this.id,
    required this.content,
    this.tags = const [],
    required this.createdAt,
    this.agentId,
  });

  factory Memory.fromJson(Json j) => Memory(
    id: j['id'] as int,
    content: j['content'] as String,
    tags: [for (final t in j['tags'] as List? ?? const []) t as String],
    createdAt: DateTime.fromMillisecondsSinceEpoch(j['created_at'] as int),
    agentId: j['agent_id'] as String?,
  );

  final int id;
  final String content;
  final List<String> tags;
  final DateTime createdAt;

  /// Null: shared.
  final String? agentId;

  bool get shared => agentId == null;
}

/// A line of an agent's recent work: a batch delegated to it, or a routine
/// run (`GET /v1/agents/{id}/work`).
class AgentWork {
  const AgentWork({
    required this.kind,
    required this.title,
    this.caller,
    required this.status,
    required this.sessionId,
    required this.runId,
    this.tasks = 1,
    required this.startedAt,
    this.finishedAt,
    this.postId,
  });

  factory AgentWork.fromJson(Json j) => AgentWork(
    kind: j['kind'] as String,
    title: j['title'] as String? ?? '',
    caller: j['caller'] as String?,
    status: j['status'] as String,
    sessionId: j['session_id'] as String,
    runId: j['run_id'] as String,
    tasks: j['tasks'] as int? ?? 1,
    startedAt: DateTime.fromMillisecondsSinceEpoch(j['started_at'] as int),
    finishedAt: j['finished_at'] is int
        ? DateTime.fromMillisecondsSinceEpoch(j['finished_at'] as int)
        : null,
    postId: j['post_id'] as int?,
  );

  /// `delegated` or `routine`.
  final String kind;

  /// The brief, or the routine's title.
  final String title;

  /// Who asked, for delegated work.
  final String? caller;

  /// `running`, `done` or `failed`.
  final String status;

  /// Where it opens: the caller's thread, or the routine's session.
  final String sessionId;
  final String runId;
  final int tasks;
  final DateTime startedAt;
  final DateTime? finishedAt;
  final int? postId;
}

/// A routine or channel an impact names: a routine's prompt, a channel's
/// slug.
class Named {
  const Named({required this.id, required this.label});

  factory Named.fromJson(Json j) =>
      Named(id: j['id'] as String, label: j['label'] as String);

  final String id;
  final String label;
}

List<Named> _named(Object? v) => [
  if (v is List<dynamic>)
    for (final n in v) Named.fromJson(n as Json),
];

/// What deleting an agent touches (`GET /v1/agents/{id}/impact`).
class AgentImpact {
  const AgentImpact({
    required this.id,
    required this.slug,
    required this.name,
    this.routines = const [],
    this.ledChannels = const [],
    this.memberChannels = const [],
    this.conversations = 0,
    this.memories = 0,
    this.runningRuns = 0,
    this.pendingOutbox = 0,
    this.telegramBot,
    this.bundles = const [],
    this.blockers = const [],
  });

  factory AgentImpact.fromJson(Json j) => AgentImpact(
    id: j['id'] as String,
    slug: j['slug'] as String,
    name: j['name'] as String,
    routines: _named(j['routines']),
    ledChannels: _named(j['led_channels']),
    memberChannels: _named(j['member_channels']),
    conversations: j['conversations'] as int? ?? 0,
    memories: j['memories'] as int? ?? 0,
    runningRuns: j['running_runs'] as int? ?? 0,
    pendingOutbox: j['pending_outbox'] as int? ?? 0,
    telegramBot: j['telegram_bot'] as String?,
    bundles: [for (final b in j['bundles'] as List<dynamic>? ?? []) b as String],
    blockers: [
      for (final b in j['blockers'] as List<dynamic>? ?? []) b as String,
    ],
  );

  final String id;
  final String slug;
  final String name;

  /// Handed to another agent or deleted with it.
  final List<Named> routines;

  /// Topic channels it leads: the lead moves on.
  final List<Named> ledChannels;
  final List<Named> memberChannels;

  /// They stay in History under its name.
  final int conversations;

  /// Its private memories: deleted with it.
  final int memories;
  final int runningRuns;
  final int pendingOutbox;

  /// The secret naming its own Telegram bot's token: the bot stops, the
  /// secret stays.
  final String? telegramBot;

  /// Bundles that installed it and stop tracking it.
  final List<String> bundles;

  /// Why it can't be deleted now; empty when it can.
  final List<String> blockers;
}

/// What deleting a topic channel touches (`GET /v1/channels/{id}/impact`).
class ChannelImpact {
  const ChannelImpact({
    required this.id,
    required this.slug,
    required this.title,
    this.posts = 0,
    this.conversations = 0,
    this.routines = const [],
    this.members = 0,
    this.runningRuns = 0,
    this.blockers = const [],
  });

  factory ChannelImpact.fromJson(Json j) => ChannelImpact(
    id: j['id'] as String,
    slug: j['slug'] as String,
    title: j['title'] as String,
    posts: j['posts'] as int? ?? 0,
    conversations: j['conversations'] as int? ?? 0,
    routines: _named(j['routines']),
    members: j['members'] as int? ?? 0,
    runningRuns: j['running_runs'] as int? ?? 0,
    blockers: [
      for (final b in j['blockers'] as List<dynamic>? ?? []) b as String,
    ],
  );

  final String id;
  final String slug;
  final String title;

  /// Deleted with the channel.
  final int posts;

  /// They stay in History, under Home.
  final int conversations;

  /// Routines that post here: they post to their agent's DM instead.
  final List<Named> routines;
  final int members;
  final int runningRuns;
  final List<String> blockers;
}
