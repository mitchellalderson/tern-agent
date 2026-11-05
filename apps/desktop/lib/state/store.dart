import 'dart:async';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:tern_client/tern_client.dart';

import 'notifier.dart';
import 'transcript.dart';
import 'version.dart';
import 'workspaces.dart';
import 'workspaces_adapter.dart';

enum Link { connecting, online, offline }

/// A Library page shown in the centre column instead of a conversation.
enum LibraryPage { mcp, connections, routines, ledger, tasks, bundles }

/// Everything the app knows, fed by the daemon. Widgets listen to it.
///
/// The daemon owns all state: this is a cache rebuilt on every (re)connect,
/// since the event stream has no replay.
class TernStore extends ChangeNotifier {
  TernStore(
    this.client, {
    Notifier? notifier,
    Map<String, int>? lastSeen,
    this.onReadChanged,
  }) : notifier = notifier ?? const SilentNotifier(),
       lastSeen = lastSeen ?? {} {
    this.notifier.onAction = _onNotification;
  }

  final TernClient client;
  final Notifier notifier;

  /// Whether the window has focus; notifications only go out when it
  /// doesn't (or when it's showing another conversation).
  bool appActive = true;

  Link link = Link.connecting;
  String? linkError;
  Health? health;
  Info? info;
  List<SessionItem> sessions = [];

  /// The open conversation. Null with [draft] true means a new one that
  /// hasn't been sent yet.
  String? selected;
  bool draft = true;
  final transcripts = <String, Transcript>{};
  final _runSession = <String, String>{};

  /// Sub-agent run → the run that started it.
  final _parentOf = <String, String>{};

  /// Run → the conversation its approvals show in (a sub-agent's go to
  /// the one the user started).
  final _approvalSession = <String, String>{};
  final _secretSession = <String, String>{};
  final usage = <String, Usage>{};
  List<Schedule> schedules = [];

  /// Model lists per provider, loaded on demand by the pickers.
  List<ProviderModels>? modelLists;
  bool loadingModels = false;

  /// MCP servers, when the core has them ([hasMcp]).
  List<McpStatus> mcpServers = [];

  /// Sign-in and connect cards waiting for the user, oldest first
  /// ([hasConnections]). Shown over the whole window, not in a transcript.
  List<AuthPrompt> authPrompts = [];

  /// The accounts per connector or MCP server, once loaded.
  ConnectionList? connectionList;

  /// Installed bundles, once loaded ([canManageBundles]).
  List<InstalledBundle>? bundles;

  /// The Library page in the centre column, and the MCP server open in it.
  LibraryPage? page;
  String? mcpSelected;

  /// Channels, Home first, when the core has them ([hasChannels]).
  List<Channel> channels = [];

  /// Writes waiting for the user's approval (tooling spec §5.2), newest
  /// first. Shown in Home, and an agent's own in its DM too.
  List<OutboxItem> outbox = [];

  /// The channel open in the centre column: [homeId], an agent's DM, or a
  /// topic channel. Null when a plain conversation ([selected]) is open.
  String? channel;

  /// Home's id (and slug).
  static const homeId = 'home';

  /// Where a main channel's stream lives in [transcripts]: one transcript
  /// for all of its segments.
  static String streamKeyOf(String channelId) => '#$channelId';

  /// Where Home's stream lives. Every approval and secret request is
  /// there too, whichever stream is open.
  static final homeKey = streamKeyOf(homeId);

  /// The main channel whose stream is loaded: Home or an agent's DM
  /// (agent profiles §3). One at a time; opening another reloads.
  String streamChannel = homeId;

  String get streamKey => streamKeyOf(streamChannel);

  /// The sessions whose events belong to the loaded stream.
  final _streamSessions = <String>{};

  /// The loaded stream, oldest first, and the cursor for older items.
  List<StreamItem> _streamItems = [];
  String? homeNext;

  /// A page of history: what one request loads, in posts or stream items.
  static const postsPage = 100;

  /// The most a resync re-reads, however far back the reader has paged.
  static const historyCap = 600;

  /// Topic channels with posts older than what is loaded.
  final postsMore = <String, bool>{};

  /// The stream has changed underneath (a carry-over to show, a post that
  /// came in mid-run): rebuild it when its run finishes.
  var _streamDirty = false;

  /// The next send in the open stream starts a new topic.
  bool newTopic = false;

  /// Agent profiles, the default first, when the core has them
  /// ([hasAgents]).
  List<Agent> agents = [];

  /// Each opened topic channel's posts, oldest first.
  final posts = <String, List<Post>>{};

  /// Read state, kept on this device: channel → the newest post seen.
  final Map<String, int> lastSeen;

  /// Called when [lastSeen] changes, to save it.
  final void Function()? onReadChanged;

  /// Run → the channel of its session, from run events.
  final _runChannel = <String, String>{};

  /// Top-level runs in progress → their channel, for the sidebar.
  final _activeIn = <String, String>{};

  /// Runs in progress in a channel, sub-agents too: who and where, and
  /// what each is doing, for its member lines (channels-design §5.2).
  final _live = <String, ({String channel, String author})>{};
  final _phase = <String, String>{};

  /// Sub-agent run → its label, from `subagent_spawned`.
  final _labels = <String, String>{};

  /// The conversation open (to read) in the right pane: a History entry,
  /// a moved conversation, the run behind a post.
  String? thread;

  /// The model a new conversation will use, when chosen before sending.
  String? draftModel;
  String? error;
  var _badge = 0;

  StreamSubscription<TernEvent>? _events;
  Timer? _retry;
  var _backoff = const Duration(milliseconds: 500);
  var _disposed = false;

  Transcript? get current => streamOpen
      ? transcripts[streamKey]
      : selected == null
      ? null
      : transcripts[selected];

  /// Whether the core has channels and Home's stream (T2).
  bool get hasChannels => health?.features.contains('channels') ?? false;

  /// Whether the core has agent profiles (docs/agent-profiles-design.md).
  bool get hasAgents => health?.features.contains('agents') ?? false;

  bool get homeOpen => channel == homeId;

  /// A main channel's stream is open: Home or an agent's DM.
  bool get streamOpen =>
      channel != null &&
      (channel == homeId ||
          (channels.where((c) => c.id == channel).firstOrNull?.isMain ??
              false));

  /// A topic channel's timeline is open.
  bool get topicOpen => channel != null && !streamOpen;

  /// The agent whose DM `channelId` is (the default agent for Home).
  Agent? agentOf(String channelId) =>
      agents.where((a) => a.mainChannelId == channelId).firstOrNull;

  /// The agent of the stream `session` belongs to, if it's the loaded one.
  Agent? streamAgentFor(String session) =>
      _streamSessions.contains(session) ? agentOf(streamChannel) : null;

  /// The agent whose stream is open (the default agent in Home).
  Agent? get streamAgent => streamOpen ? agentOf(channel!) : null;

  /// Who is asking: a sub-agent's label, else its agent profile's name.
  String askerName(Approval a) =>
      a.agentLabel ??
      agents.where((x) => x.id == a.agentId).firstOrNull?.name ??
      'tern';

  /// The agent profile with id `id`.
  Agent? agentById(String? id) =>
      id == null ? null : agents.where((a) => a.id == id).firstOrNull;

  /// The agent `name` names: an id, an @name or a name (what a parent
  /// passed to `agent_spawn {agent}`).
  Agent? agentNamed(String? name) {
    if (name == null) return null;
    final n = name.startsWith('@') ? name.substring(1) : name;
    return agents
        .where(
          (a) =>
              a.id == n ||
              a.slug == n ||
              a.name.toLowerCase() == n.toLowerCase(),
        )
        .firstOrNull;
  }

  /// The agent profile an author id belongs to: `tern:research`, a
  /// sub-agent's `tern:research/label`, or a routine's (`routine:<id>`)
  /// owner. Null for the user.
  Agent? agentOfAuthor(String authorId) {
    final id = switch (authorId.split(':')) {
      ['routine', final s] =>
        schedules.where((x) => x.id == s).firstOrNull?.agentId,
      _ => authorId.split('/').first,
    };
    return agents.where((a) => a.id == id).firstOrNull;
  }

  /// How to show an author: an agent's current name (with a sub-agent's
  /// label), else `fallback` (the user, a routine's title).
  String authorName(String authorId, String fallback) {
    if (!authorId.startsWith(Agent.defaultId)) return fallback;
    final a = agentOfAuthor(authorId);
    if (a == null) return fallback;
    final i = authorId.indexOf('/');
    return i < 0 ? a.name : '${a.name}${authorId.substring(i)}';
  }

  /// A topic channel's members as agents, lead first.
  List<Agent> membersOf(Channel c) => [
    for (final id in c.memberIds) ?agentById(id),
  ];

  /// Who `@` can name in topic channel `c`: its members, then the other
  /// agents (sending to them adds them).
  List<({Agent agent, bool member})> mentionable(Channel c) {
    final members = c.memberIds;
    return [
      for (final a in membersOf(c)) (agent: a, member: true),
      for (final a in agents)
        if (!a.archived && !members.contains(a.id)) (agent: a, member: false),
    ];
  }

  /// Add agent `agentId` to topic channel `channelId`. Returns the
  /// daemon's reason on failure.
  Future<String?> addToChannel(String channelId, String agentId) async {
    try {
      await client.addChannelAgent(channelId, agentId);
      channels = await client.channels();
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  Future<String?> removeFromChannel(String channelId, String agentId) async {
    try {
      await client.removeChannelAgent(channelId, agentId);
      channels = await client.channels();
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// The default agent, `tern`.
  Agent? get defaultAgent => agents.where((a) => a.isDefault).firstOrNull;

  /// Agents other than the default, for AGENTS in the sidebar.
  List<Agent> get otherAgents => [
    for (final a in agents)
      if (!a.isDefault && !a.archived) a,
  ];

  /// An agent's DM channel.
  Channel? dmOf(Agent a) =>
      channels.where((c) => c.id == a.mainChannelId).firstOrNull;

  /// Someone is working in the agent's DM.
  /// The agent is working anywhere: its DM, a routine, or work delegated
  /// to it from another agent's run.
  bool agentBusy(Agent a) =>
      _activeAgents.containsValue(a.id) ||
      (a.mainChannelId != null && _activeIn.containsValue(a.mainChannelId));

  /// Runs in progress → the agent they run as (from run events).
  final _activeAgents = <String, String>{};

  /// Approvals, secret requests and outbox items waiting from `a`'s runs.
  int waitingFor(Agent a) => switch (a.mainChannelId) {
    final dm? => waitingIn(dm),
    null => 0,
  };

  /// Cards and outbox items waiting in conversation `channelId` (Home, a
  /// DM or a topic channel): its sidebar badge.
  int waitingIn(String channelId) =>
      waitingCards(channelId).length +
      outbox
          .where((o) => o.pending && (o.channelId ?? homeId) == channelId)
          .length;

  Channel? get openChannel =>
      channels.where((c) => c.id == channel).firstOrNull;

  List<Channel> get topics => [
    for (final c in channels)
      if (!c.isMain && !c.archived) c,
  ];

  /// One line per member working in `channelId`: who, and what they are
  /// doing. The main thread's own phase is richer (waiting on agents).
  List<({String runId, String author, String doing})> liveIn(String channelId) {
    final main = channels
        .where((c) => c.id == channelId)
        .firstOrNull
        ?.mainSessionId;
    final mainRun = main == null ? null : transcripts[main]?.activeRun;
    return [
      for (final MapEntry(key: run, value: w) in _live.entries)
        if (w.channel == channelId)
          (
            runId: run,
            author: w.author,
            doing:
                (run == mainRun ? transcripts[main]?.phase?.label : null) ??
                _phase[run] ??
                'Working',
          ),
    ];
  }

  /// Move conversations into a topic channel, or a new one titled
  /// `newTitle`, and open it. Returns the daemon's reason on failure.
  Future<String?> moveToChannel(
    List<String> sessions, {
    String? channelId,
    String? newTitle,
  }) async {
    try {
      final r = await client.moveToChannel(
        sessions,
        channel: channelId,
        newChannelTitle: newTitle,
      );
      // A run shown as working in the stream goes with its session; the
      // stream's next segment starts idle. (The daemon refuses to move a
      // conversation with work running, so this is normally stale state,
      // which the resync below clears.)
      final streamRun = transcripts[streamKey]?.activeRun;
      for (final s in sessions) {
        _streamSessions.remove(s);
        if (streamRun != null && _runSession[streamRun] == s) {
          transcripts[streamKey]!.activeRun = null;
          transcripts.putIfAbsent(s, Transcript.new).activeRun = streamRun;
        }
      }
      unawaited(resyncRuns());
      channels = await client.channels();
      this.sessions = await client.sessions();
      posts.remove(r.channel.id);
      await openChannelView(r.channel.id);
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Accept an agent's channel suggestion: the channel is created, and
  /// the conversation it came from and `routines` move there unless left
  /// out. Opens the new channel. Returns the daemon's reason on failure.
  Future<String?> acceptSuggestion(
    ChannelSuggestion s, {
    bool moveConversation = true,
    List<String>? routines,
  }) async {
    try {
      final id = await client.acceptSuggestion(
        s.id,
        moveConversation: moveConversation,
        routines: routines,
      );
      if (s.sessionId case final moved? when moveConversation) {
        _streamSessions.remove(moved);
      }
      channels = await client.channels();
      sessions = await client.sessions();
      schedules = await client.schedules();
      posts.remove(id);
      await openChannelView(id);
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  Future<String?> declineSuggestion(ChannelSuggestion s) async {
    try {
      await client.declineSuggestion(s.id);
      channels = await client.channels();
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Mark a needs-you post (or its pointer in Home) done.
  Future<void> resolvePost(Post post) async {
    try {
      await client.resolvePost(post.id);
    } on TernException catch (e) {
      // Already done elsewhere: show it closed anyway.
      if (e.status != 404) {
        error = e.message;
        notifyListeners();
        return;
      }
    }
    bool pair(Post p) =>
        p.id == post.id || p.id == post.pointsTo || p.pointsTo == post.id;
    for (final list in posts.values) {
      for (var i = 0; i < list.length; i++) {
        if (pair(list[i])) list[i] = list[i].resolved();
      }
    }
    for (final item
        in transcripts[homeKey]?.items.whereType<PostItem>() ??
            const <PostItem>[]) {
      if (pair(item.post)) item.post = item.post.resolved();
    }
    await _refreshChannels();
  }

  /// Show outbox item [id]'s needs-you post and pointer closed.
  void _resolveOutboxPosts(int id) {
    bool of(Post p) => p.outboxId == id && p.resolvedAt == null;
    for (final list in posts.values) {
      for (var i = 0; i < list.length; i++) {
        if (of(list[i])) list[i] = list[i].resolved();
      }
    }
    for (final t in transcripts.values) {
      for (final item in t.items.whereType<PostItem>()) {
        if (of(item.post)) item.post = item.post.resolved();
      }
    }
  }

  /// New posts since this device last looked.
  bool unread(Channel c) =>
      c.id != channel && (c.lastPostId ?? 0) > (lastSeen[c.id] ?? 0);

  /// Someone is working in the channel.
  bool busy(Channel c) => _activeIn.containsValue(c.id);

  /// Approvals and secret requests from runs in the right pane's thread
  /// that wait in another conversation than the open one: where, and how
  /// many.
  ({String channel, int count})? waitingForThread() {
    final t = threadTranscript;
    if (t == null) return null;
    final runs = {?t.activeRun, for (final c in t.children) ?c.runId};
    for (final ch in channels) {
      if (ch.id == channel) continue;
      final n = waitingCards(ch.id)
          .where(
            (i) => switch (i) {
              ApprovalItem(:final approval) => runs.contains(approval.runId),
              SecretRequestItem(:final request) => runs.contains(request.runId),
              _ => false,
            },
          )
          .length;
      if (n > 0) return (channel: ch.id, count: n);
    }
    return null;
  }

  /// Open the conversation `channelId`: Home, an agent's DM, or a topic
  /// channel's timeline.
  Future<void> openConversation(String channelId) async {
    final ch = channels.where((c) => c.id == channelId).firstOrNull;
    if (channelId == homeId || ch == null) return openHome();
    if (ch.isMain) {
      if (agentOf(channelId) case final a?) return openAgent(a);
      return openHome();
    }
    return openChannelView(channelId);
  }

  /// What to call conversation `channelId`: Home, an agent's name, or
  /// `#slug`.
  String conversationName(String channelId) {
    if (channelId == homeId) return 'Home';
    final ch = channels.where((c) => c.id == channelId).firstOrNull;
    if (ch == null) return 'Home';
    if (ch.isMain) return agentOf(channelId)?.name ?? ch.title;
    return '#${ch.slug}';
  }

  Channel? get home => channels.where((c) => c.isHome).firstOrNull;

  /// The main channel whose stream is loaded.
  Channel? get stream =>
      channels.where((c) => c.id == streamChannel).firstOrNull;

  /// The transcript showing `session`: the stream's for its segments.
  Transcript? transcriptOf(String session) => _streamSessions.contains(session)
      ? transcripts[streamKey]
      : transcripts[session];

  /// The thread pane's transcript.
  Transcript? get threadTranscript =>
      thread == null ? null : transcripts[thread];

  bool get running => current?.activeRun != null;

  /// Approvals and outbox items waiting anywhere, for badges. A card
  /// mirrored into an agent's DM is the same item as Home's, counted once.
  int get pendingApprovals =>
      transcripts.values
          .expand((t) => t.items)
          .whereType<ApprovalItem>()
          .where((a) => a.outcome == null)
          .toSet()
          .length +
      outbox.where((o) => o.pending).length;

  /// Secret requests waiting anywhere.
  int get pendingSecrets => transcripts.values
      .expand((t) => t.items)
      .whereType<SecretRequestItem>()
      .where((r) => r.open)
      .toSet()
      .length;

  /// Whether the core can ask for secrets (older ones can't).
  bool get hasSecretRequests =>
      health?.features.contains('secret_requests') ?? false;

  /// Whether the core manages MCP servers over the API (older ones can't).
  bool get hasMcp => health?.features.contains('mcp') ?? false;

  /// Whether the core manages accounts per connector or MCP server.
  bool get hasConnections => health?.features.contains('connections') ?? false;

  /// The Connections page is owner-only.
  bool get canManageConnections => hasConnections && (health?.isOwner ?? false);

  /// Whether the core installs bundles (tooling spec §6.5). Owner-only.
  bool get canManageBundles =>
      (health?.features.contains('bundles') ?? false) &&
      (health?.isOwner ?? false);

  /// Whether the core defers unanswered writes to an outbox.
  bool get hasOutbox => health?.features.contains('outbox') ?? false;

  /// The Tasks view (tooling spec §8.2).
  bool get hasTasks => health?.features.contains('tasks') ?? false;

  /// What waits for you anywhere: open approval and secret cards in every
  /// conversation this app has, and the outbox.
  int get needsYouCount {
    var n = outbox.where((o) => o.pending).length;
    for (final t in transcripts.values) {
      for (final i in t.items) {
        if ((i is ApprovalItem && i.outcome == null) ||
            (i is SecretRequestItem && i.open)) {
          n++;
        }
      }
    }
    return n;
  }

  /// The folder API, or null when this core has none (an older one) or the
  /// adapter is not wired: every folder control hides then.
  WorkspaceApi? get workspaces => _wsSet
      ? _ws
      : (health?.features.contains(workspacesFeature) ?? false)
      ? (_ws ??= workspaceApiFor(this))
      : null;
  set workspaces(WorkspaceApi? api) {
    _ws = api;
    _wsSet = true;
  }

  WorkspaceApi? _ws;
  var _wsSet = false;

  /// A folder chosen for the next Home topic before it exists.
  String? pendingFolder;
  void setPendingFolder(String? path) {
    pendingFolder = path;
    notifyListeners();
  }

  /// `tern security audit` on the core (tooling spec §8.4): owner-only.
  bool get hasSecurityAudit =>
      (health?.features.contains('security_audit') ?? false) &&
      (health?.isOwner ?? false);

  /// The activity ledger (tooling spec §8.1): owner-only.
  bool get hasLedger =>
      (health?.features.contains('ledger') ?? false) &&
      (health?.isOwner ?? false);

  /// Reached as a paired device rather than the local owner socket, so the
  /// core refuses stdio MCP servers unless `api.remote_mcp_stdio` is set.
  bool get remoteCore => health != null && health!.authKind != 'owner';

  /// The session's own model, else its agent's, else the default.
  String? modelFor(String? session) {
    if (session == null) {
      return draftModel ?? streamAgent?.model.firstOrNull ?? info?.defaultModel;
    }
    final s = sessions.where((x) => x.id == session).firstOrNull;
    final agent = switch (s?.channelId) {
      final c? => agentOf(c),
      null => null,
    };
    return s?.model ?? agent?.model.firstOrNull ?? info?.defaultModel;
  }

  /// A provider's display name (`OpenRouter`) from the loaded model lists,
  /// else its config name.
  String providerLabel(String name) =>
      modelLists?.where((l) => l.provider == name).firstOrNull?.label ?? name;

  /// A failed model call in plain words, for the model `model`
  /// (`provider:id`): `OpenRouter couldn't find that endpoint or model...`.
  /// Null when [raw] isn't an error we can explain.
  FriendlyError? explainFailure(String? model, String raw) {
    final provider = model != null && model.contains(':')
        ? model.substring(0, model.indexOf(':'))
        : (model ?? 'the provider');
    return friendlyCallError(raw, providerLabel(provider));
  }

  /// No usable model yet: show provider setup instead of the composer.
  bool get setupNeeded => info?.setupNeeded ?? false;

  /// The daemon predates provider setup (`/v1/providers`): it answers 404,
  /// so say why instead.
  bool get daemonTooOld =>
      health != null && !health!.features.contains('providers');

  /// The core running here is not the version this app was built with:
  /// what to tell the user, else null (see [coreSkewMessage]).
  String? get coreSkew {
    final h = health;
    if (h == null) return null;
    return coreSkewMessage(
      appVersion: appVersion,
      coreVersion: h.version,
      local: h.isLocal,
    );
  }

  static const daemonTooOldMessage =
      'The running tern daemon is older than this app and has no model '
      'setup yet. Restart it to update: stop `tern daemon` and start it '
      'again (or quit it and press Start tern here).';

  Future<void> connect() async {
    _retry?.cancel();
    await _events?.cancel();
    _events = null;
    if (link != Link.online) {
      link = Link.connecting;
      notifyListeners();
    }
    try {
      health = await client.health();
      info = await client.info();
      // Subscribe before resyncing, so nothing falls between the two.
      _events = client.events().listen(
        _onEvent,
        onError: (Object e) => _lost(e),
        onDone: () => _lost(null),
      );
      await _resync();
      _staleTimer ??= Timer.periodic(
        const Duration(seconds: 15),
        (_) => _checkStale(),
      );
      link = Link.online;
      linkError = null;
      _backoff = const Duration(milliseconds: 500);
    } on TernException catch (e) {
      _lost(e);
      return;
    }
    notifyListeners();
  }

  void _lost(Object? e) {
    if (_disposed) return;
    _events?.cancel();
    _events = null;
    link = Link.offline;
    linkError = e?.toString() ?? 'connection closed';
    notifyListeners();
    _retry?.cancel();
    _retry = Timer(_backoff, connect);
    final next = _backoff * 2;
    _backoff = next > const Duration(seconds: 10)
        ? const Duration(seconds: 10)
        : next;
  }

  Future<void> _resync() async {
    sessions = await client.sessions();
    schedules = await client.schedules();
    final active = await client.activeRuns();
    for (final r in active) {
      _runSession[r.runId] = r.sessionId;
      if (r.parentRunId case final p?) _parentOf[r.runId] = p;
    }
    if (hasChannels) {
      channels = await client.channels();
      if (hasAgents) agents = await client.agents();
      // Home is where the app opens.
      if (channel == null && selected == null && page == null) {
        channel = homeId;
      }
      if (streamOpen) {
        streamChannel = channel!;
        await _loadHome();
      }
    }
    if (selected case final id? when !streamOpen) {
      await _load(id);
    }
    if (thread case final id?) await _load(id, limit: threadPage);
    if (topicOpen) await _loadChannel(channel!);
    _activeIn.clear();
    _activeAgents.clear();
    _live.clear();
    for (final r in active) {
      if (r.channelId case final ch?) {
        _live[r.runId] = (channel: ch, author: r.authorId ?? 'tern');
        if (r.label case final l?) _labels[r.runId] = l;
      }
      if (agentOfAuthor(r.authorId ?? Agent.defaultId) case final a?) {
        _activeAgents[r.runId] = a.id;
      }
      if (r.parentRunId != null) continue;
      transcriptOf(r.sessionId)?.activeRun = r.runId;
      final c = sessions.where((s) => s.id == r.sessionId).firstOrNull;
      if (c?.channelId case final ch?) {
        _activeIn[r.runId] = ch;
        _runChannel[r.runId] = ch;
      }
    }
    // A run that finished while this app was away still shows as working
    // in the views it kept: it isn't.
    final live = {for (final r in active) r.runId};
    for (final t in transcripts.values) {
      if (t.activeRun case final run? when !live.contains(run)) t.endRun(run);
    }
    for (final a in await client.pendingApprovals()) {
      final key = hasChannels ? _requestKey(a.channelId) : a.sessionId;
      _transcriptFor(key)?.apply(ApprovalRequested(a));
      if (hasChannels) _approvalSession[a.runId] = key;
    }
    if (hasMcp) await loadMcp(notify: false);
    if (hasOutbox) await refreshOutbox(notify: false);
    if (hasConnections) {
      try {
        authPrompts = await client.authPrompts();
      } on TernException {
        // Not available to this device: the event stream still delivers.
      }
      if (canManageConnections) await loadConnections(notify: false);
    }
    if (hasSecretRequests) {
      try {
        for (final r in await client.secretRequests()) {
          _transcriptFor(_routeSecret(r))?.apply(SecretRequested(r));
        }
      } on TernException {
        // Not available to this device: the event stream still delivers.
      }
    }
  }

  /// The messages of each session read so far, oldest first, and the index
  /// of the first one among the session's: a long conversation opened in
  /// the thread pane loads a page at a time.
  final _sessionMessages = <String, List<SessionMessage>>{};
  final _sessionOffset = <String, int>{};

  /// Messages one request loads for a conversation read in the thread pane.
  static const threadPage = 200;

  /// Whether the open thread has messages before what is loaded.
  bool get threadHasEarlier => (_sessionOffset[thread] ?? 0) > 0;

  /// How many messages precede what is loaded of the open thread.
  int get threadEarlierCount => _sessionOffset[thread] ?? 0;

  /// Load a session: all of it, or with `limit` the newest page (widened to
  /// what was already read, up to the cap), or with `older` the page
  /// before what is loaded.
  Future<void> _load(String id, {int? limit, bool older = false}) async {
    final read = _sessionMessages[id];
    final SessionDetail detail;
    final List<SessionMessage> messages;
    if (limit == null) {
      detail = await client.session(id);
      messages = detail.messages;
    } else if (older) {
      detail = await client.session(
        id,
        limit: limit,
        before: _sessionOffset[id] ?? 0,
      );
      messages = [...detail.messages, ...?read];
    } else {
      detail = await client.session(
        id,
        limit: min(max(limit, (read?.length ?? 0) + 20), historyCap),
      );
      messages = detail.messages;
    }
    _sessionMessages[id] = messages;
    _sessionOffset[id] = detail.offset;
    final previous = transcripts[id];
    final t =
        Transcript.fromSession(
            SessionDetail(id: id, messages: messages, offset: detail.offset),
          )
          ..activeRun = previous?.activeRun
          ..runSince = previous?.runSince;
    // Keep approval cards: the stored history doesn't contain them.
    for (final a
        in previous?.items.whereType<ApprovalItem>() ?? <ApprovalItem>[]) {
      if (a.outcome == null) t.items.add(a);
    }
    for (final r
        in previous?.items.whereType<SecretRequestItem>() ??
            <SecretRequestItem>[]) {
      if (r.open) t.items.add(r);
    }
    transcripts[id] = t;
    usage[id] = await client.usage(id);
    if (t.children.isNotEmpty) {
      try {
        final runs = await client.subagents(id);
        for (final r in runs) {
          _runSession[r.runId] = r.sessionId;
          _parentOf[r.runId] = r.parentRunId;
        }
        t.mergeChildren(runs);
      } on TernException {
        // A daemon without sub-agents: the calls alone will do.
      }
    }
  }

  /// Where a run's approvals and secret requests wait: the conversation
  /// its tree is in, by the root session's channel (DECISIONS.md
  /// "Approvals land where the work is"). Home and DMs show the cards in
  /// their stream; a topic channel's are held under the same key and shown
  /// in its timeline. Home gets the daemon's pointer post for the others.
  static String _requestKey(String? channelId) =>
      channelId == null || channelId == homeId
      ? homeKey
      : streamKeyOf(channelId);

  /// The open approval and secret request cards waiting in `channelId`.
  List<Item> waitingCards(String channelId) => [
    for (final i in transcripts[_requestKey(channelId)]?.items ?? const [])
      if ((i is ApprovalItem && i.outcome == null) ||
          (i is SecretRequestItem && i.open))
        i,
  ];

  /// The live card for a request post or pointer, if this app has it.
  Item? requestCard(Post post) {
    for (final t in transcripts.values) {
      for (final i in t.items) {
        if (i is ApprovalItem &&
                post.approvalId != null &&
                i.approval.id == post.approvalId ||
            i is SecretRequestItem &&
                post.secretRequestId != null &&
                i.request.id == post.secretRequestId) {
          return i;
        }
      }
    }
    return null;
  }

  /// Whether [run] is in a session of the open stream (or one this app
  /// can't place yet).
  bool _inStream(String run) => switch (_runSession[run]) {
    final s? => _streamSessions.contains(s),
    null => true,
  };

  /// Rebuild the open stream (Home or a DM) from the daemon: the newest
  /// page (as many items as were loaded), or with `older` the page before
  /// what's loaded.
  Future<void> _loadHome({bool older = false}) async {
    final id = streamChannel;
    final head = stream?.mainSessionId;
    final page = await client.stream(
      id,
      before: older ? homeNext : null,
      limit: older ? 200 : min(max(200, _streamItems.length + 20), historyCap),
    );
    if (id != streamChannel) return;
    final fresh = page.items.reversed.toList();
    _streamItems = older ? [...fresh, ..._streamItems] : fresh;
    homeNext = page.next;
    _streamSessions
      ..clear()
      ..addAll([
        for (final i in _streamItems)
          if (i
              case StreamMessage(:final sessionId) ||
                  StreamCut(:final sessionId))
            sessionId,
        ?head,
      ]);
    final previous = transcripts[streamKey];
    // Busy state belongs to the run's session: keep it only while that
    // session is still part of this stream (it may have moved away).
    final kept = switch (previous?.activeRun) {
      final run? when _inStream(run) => run,
      _ => null,
    };
    final t = Transcript.fromStream(_streamItems)
      ..activeRun = kept
      ..runSince = kept == null ? null : previous?.runSince;
    for (final a
        in previous?.items.whereType<ApprovalItem>() ?? <ApprovalItem>[]) {
      if (a.outcome == null) t.items.add(a);
    }
    for (final r
        in previous?.items.whereType<SecretRequestItem>() ??
            <SecretRequestItem>[]) {
      if (r.open) t.items.add(r);
    }
    transcripts[streamKey] = t;
    _streamDirty = false;
    selected = head;
    draft = head == null;
    if (head == null) return;
    usage[head] = await client.usage(head);
    if (t.children.isNotEmpty) {
      try {
        final runs = await client.subagents(head);
        for (final r in runs) {
          _runSession[r.runId] = r.sessionId;
          _parentOf[r.runId] = r.parentRunId;
        }
        t.mergeChildren(runs);
      } on TernException {
        // The calls alone will do.
      }
    }
  }

  /// Open Home's stream in the centre column; with `newTopic`, the next
  /// send starts a new segment.
  Future<void> openHome({bool newTopic = false}) =>
      openStream(homeId, newTopic: newTopic);

  /// Open an agent's DM: the same stream view as Home.
  Future<void> openAgent(Agent a) async {
    if (a.mainChannelId case final id?) await openStream(id);
  }

  /// Open a main channel's stream (Home or a DM) in the centre column.
  Future<void> openStream(String id, {bool newTopic = false}) async {
    page = null;
    channel = id;
    error = null;
    pendingFolder = null;
    this.newTopic = newTopic;
    if (streamChannel != id) {
      streamChannel = id;
      _streamItems = [];
      _streamSessions.clear();
      homeNext = null;
    }
    selected = stream?.mainSessionId;
    draft = selected == null;
    notifyListeners();
    try {
      if (channels.isEmpty) channels = await client.channels();
      if (stream?.lastPostId case final p?) _markRead(id, p);
      await _loadHome();
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Load the page of a topic channel's posts before what's loaded.
  Future<void> loadOlderPosts(String id) async {
    final loaded = posts[id];
    if (loaded == null || loaded.isEmpty || postsMore[id] != true) return;
    try {
      final page = await client.posts(
        id,
        before: loaded.first.id,
        limit: postsPage,
      );
      posts[id] = [...page.reversed, ...loaded];
      postsMore[id] = page.length >= postsPage;
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Load the page of Home's stream before what's shown.
  Future<void> loadOlderHome() async {
    if (homeNext == null) return;
    try {
      await _loadHome(older: true);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  void toggleNewTopic() {
    newTopic = !newTopic;
    notifyListeners();
  }

  /// Re-read the channels; follow the open stream to a new segment
  /// started elsewhere.
  Future<void> _refreshChannels() async {
    try {
      channels = await client.channels();
      final head = stream?.mainSessionId;
      if (streamOpen && head != null && head != selected) {
        _streamSessions.add(head);
        await _loadHome();
      }
      notifyListeners();
    } on TernException {
      // Refreshed again on reconnect.
    }
  }

  /// Re-read the agents (and the channels, for their DMs).
  Future<void> _refreshAgents() async {
    try {
      agents = await client.agents();
      channels = await client.channels();
      notifyListeners();
    } on TernException {
      // Refreshed again on reconnect.
    }
  }

  /// A post was rewritten (a delegation's pointer as its work goes on):
  /// replace it wherever it shows.
  void _onPostUpdated(Post post) {
    for (final list in posts.values) {
      for (var i = 0; i < list.length; i++) {
        if (list[i].id == post.id) list[i] = post;
      }
    }
    for (final t in transcripts.values) {
      for (final item in t.items.whereType<PostItem>()) {
        if (item.post.id == post.id) item.post = post;
      }
      // An answered request's pointer leaves Home.
      if (!showsInStream(post)) {
        t.items.removeWhere((i) => i is PostItem && i.post.id == post.id);
      }
    }
    workVersion++;
    notifyListeners();
  }

  /// Bumped when an agent's recent work may have changed, so the side
  /// panel reloads it.
  var workVersion = 0;

  /// A post came in: the open stream (Home or a DM) shows it where it
  /// happened; an open topic channel adds it to its timeline.
  void _onPost(Post post) {
    unawaited(_refreshChannels());
    if (post.kind == 'delegated') workVersion++;
    final main =
        post.channelId == homeId ||
        (channels.where((c) => c.id == post.channelId).firstOrNull?.isMain ??
            false);
    if (main) {
      if (post.channelId != streamChannel || !streamOpen) return;
      _markRead(post.channelId, post.id);
    } else {
      final list = posts[post.channelId];
      if (list != null && !list.any((p) => p.id == post.id)) {
        list.add(post);
      }
      if (post.channelId == channel) _markRead(post.channelId, post.id);
      notifyListeners();
      return;
    }
    final t = transcripts[streamKey];
    if (t == null || !showsInStream(post)) return;
    // Mid-run, appending would split the reply streaming in.
    if (t.activeRun == null) {
      t.items.add(PostItem(post));
      notifyListeners();
    } else {
      _streamDirty = true;
    }
  }

  /// Open a topic channel's timeline in the centre column.
  Future<void> openChannelView(String id) async {
    page = null;
    channel = id;
    error = null;
    selected = openChannel?.mainSessionId;
    draft = false;
    notifyListeners();
    try {
      await _loadChannel(id);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  Future<void> _loadChannel(String id) async {
    // The newest page, widened to what the reader has paged in (up to a
    // cap, so a resync stays cheap however far back they have read).
    final have = posts[id]?.length ?? 0;
    final limit = min(max(postsPage, have + 20), historyCap);
    final page = await client.posts(id, limit: limit);
    posts[id] = page.reversed.toList();
    postsMore[id] = page.length >= limit;
    if (page.isNotEmpty) _markRead(id, page.first.id);
    final main = channels.where((c) => c.id == id).firstOrNull?.mainSessionId;
    if (main != null && channel == id) {
      selected = main;
      transcripts.putIfAbsent(main, Transcript.new);
      usage[main] = await client.usage(main);
    }
  }

  void _markRead(String id, int post) {
    if ((lastSeen[id] ?? 0) >= post) return;
    lastSeen[id] = post;
    onReadChanged?.call();
  }

  // ---- agent profiles (docs/agent-profiles-design.md) ----

  /// Create an agent (and, when given, the routine its template suggests)
  /// and open its DM. Returns the daemon's reason on failure.
  Future<String?> createAgent({
    required String name,
    AvatarSpec? avatar,
    required String persona,
    List<String> model = const [],
    List<String> subagentModel = const [],
    ToolScope? tools,
    required String memory,
    String? egress,
    String? autonomy,
    String? template,
    ({String cron, String prompt})? routine,
  }) async {
    try {
      final a = await client.createAgent(
        name: name,
        avatar: avatar,
        persona: persona,
        model: model,
        subagentModel: subagentModel,
        tools: tools,
        memory: memory,
        egress: egress,
        autonomy: autonomy,
        template: template,
      );
      if (routine != null) {
        await client.createSchedule(
          cron: routine.cron,
          prompt: routine.prompt,
          agent: a.id,
        );
        schedules = await client.schedules();
      }
      agents = await client.agents();
      channels = await client.channels();
      await openAgent(a);
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Save an edited agent: the fields that changed, and its persona when
  /// it did. Applies from its next run. Returns the daemon's reason on
  /// failure.
  Future<String?> updateAgent(
    Agent a, {
    Map<String, Object?> fields = const {},
    String? persona,
    List<String>? defaultChain,
  }) async {
    try {
      // The default agent's fallbacks live in `default_model` (the agent
      // update takes only its primary), so they go through the config.
      if (defaultChain != null) {
        try {
          await client.setDefaultModelChain(defaultChain);
        } on TernException catch (e) {
          return chainSaveMessage(e);
        }
        await refreshInfo(notify: false);
      }
      if (fields.isNotEmpty) await client.updateAgent(a.id, fields);
      if (persona != null) await client.setPersona(a.id, persona);
      agents = await client.agents();
      channels = await client.channels();
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Archive an agent: hidden, its routines paused, nothing deleted.
  Future<String?> archiveAgent(Agent a) async {
    final err = await updateAgent(a, fields: {'archived': true});
    if (err == null && channel == a.mainChannelId) await openHome();
    return err;
  }

  /// Delete an agent for good. Its conversations stay in History; its
  /// routines go to `reassignTo` or are deleted (`deleteRoutines`), as the
  /// impact said. Returns the daemon's reason on failure.
  Future<String?> deleteAgent(
    Agent a, {
    String? reassignTo,
    bool deleteRoutines = false,
  }) async {
    try {
      await client.deleteAgent(
        a.id,
        reassignTo: reassignTo,
        deleteRoutines: deleteRoutines,
      );
      final viewing = channel == a.mainChannelId;
      agents = await client.agents();
      channels = await client.channels();
      schedules = await client.schedules();
      if (viewing) {
        await openHome();
      } else {
        notifyListeners();
      }
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Bring an archived agent back (its routines stay paused).
  Future<String?> unarchiveAgent(Agent a) async {
    try {
      await client.updateAgent(a.id, {'archived': false});
      agents = await client.agents();
      channels = await client.channels();
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Archived agents and topic channels, for the Archived list.
  Future<({List<Agent> agents, List<Channel> channels})> archivedItems() async {
    final a = await client.agents(archived: true);
    final c = await client.channels(archived: true);
    return (
      agents: [
        for (final x in a)
          if (x.archived && !x.isDefault) x,
      ],
      channels: [
        for (final x in c)
          if (x.archived && !x.isMain) x,
      ],
    );
  }

  /// The routines of `a` (`tern` owns those from before agents).
  List<Schedule> schedulesOf(Agent a) => [
    for (final s in schedules)
      if (s.agentId == a.id) s,
  ];

  /// The routines that post to channel `id`.
  List<Schedule> schedulesIn(String id) => [
    for (final s in schedules)
      if (s.channelId == id) s,
  ];

  /// Create a topic channel and open it. Returns the daemon's reason on
  /// failure.
  Future<String?> createChannel({
    required String slug,
    String? title,
    String? instructions,
    String? workspace,
  }) async {
    try {
      final c = await client.createChannel(
        slug: slug,
        title: title,
        instructions: instructions,
        workspace: workspace,
      );
      channels = await client.channels();
      await openChannelView(c.id);
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Rename a channel or change its instructions (empty clears them).
  Future<String?> updateChannel(
    String id, {
    String? slug,
    String? title,
    String? instructions,
    String? lead,
    bool? agentWakes,
    bool? letAgentsDecide,
  }) async {
    try {
      await client.updateChannel(
        id,
        slug: slug,
        title: title,
        instructions: instructions?.trim().isEmpty ?? false
            ? null
            : instructions,
        clearInstructions: instructions?.trim().isEmpty ?? false,
        lead: lead,
        agentWakes: agentWakes,
        letAgentsDecide: letAgentsDecide,
      );
      channels = await client.channels();
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  Future<void> archiveChannel(String id) async {
    try {
      await client.updateChannel(id, archived: true);
      channels = await client.channels();
      if (channel == id) {
        await openHome();
        return;
      }
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Delete a topic channel for good: its posts go, its conversations stay
  /// in History. Returns the daemon's reason on failure.
  Future<String?> deleteChannel(String id) async {
    try {
      await client.deleteChannel(id);
      channels = await client.channels();
      schedules = await client.schedules();
      if (channel == id) {
        await openHome();
      } else {
        notifyListeners();
      }
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Bring an archived channel back.
  Future<String?> unarchiveChannel(String id) async {
    try {
      await client.updateChannel(id, archived: false);
      channels = await client.channels();
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Send into the open topic channel: a post and a run in its main
  /// thread, or queued for the run already working there.
  Future<bool> _sendChannel(String text) async {
    error = null;
    final id = channel!;
    try {
      final sent = await client.sendToChannel(id, text);
      _runSession[sent.runId] = sent.sessionId;
      _runChannel[sent.runId] = id;
      if (sent.post case final p?) {
        final list = posts.putIfAbsent(id, () => []);
        if (!list.any((x) => x.id == p.id)) list.add(p);
        _markRead(id, p.id);
      }
      if (selected != sent.sessionId) {
        // The main thread was just created.
        channels = await client.channels();
        selected = sent.sessionId;
      }
      transcripts.putIfAbsent(sent.sessionId, Transcript.new).activeRun ??=
          sent.runId;
    } on TernException catch (e) {
      error = e.message;
      notifyListeners();
      return false;
    }
    notifyListeners();
    return true;
  }

  /// Open a conversation to read in the right pane (a History entry, a
  /// moved conversation, the run behind a post). It has no composer:
  /// conversations go on in their stream or channel.
  Future<void> openThread(String session) async {
    thread = session;
    notifyListeners();
    try {
      await _load(session, limit: threadPage);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Load the page of the open thread before what is shown.
  Future<void> loadOlderThread() async {
    final id = thread;
    if (id == null || !threadHasEarlier) return;
    try {
      await _load(id, limit: threadPage, older: true);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  void closeThread() {
    thread = null;
    notifyListeners();
  }

  /// Load a sub-agent's conversation, to show inside its parent's.
  Future<void> loadChild(String session) async {
    try {
      await _load(session);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Stop one sub-agent (and any of its own).
  Future<void> cancelRun(String run) async {
    try {
      await client.cancel(run);
    } on TernException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  /// Say something to a running sub-agent. Returns the daemon's reason on
  /// failure.
  Future<String?> messageRun(String run, String text) async {
    try {
      await client.messageRun(run, text);
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// The transcript holding [run]'s agents card, if [run] is a sub-agent.
  Transcript? _parentTranscript(String run) => switch (_parentOf[run]) {
    final p? => switch (_runSession[p]) {
      final s? => transcriptOf(s),
      null => null,
    },
    null => null,
  };

  Transcript? _transcriptFor(String session) => transcripts.putIfAbsent(
    _streamSessions.contains(session) ? streamKey : session,
    Transcript.new,
  );

  @visibleForTesting
  void onEventForTest(TernEvent e) => _onEvent(e);

  void _onEvent(TernEvent e) {
    _lastEvent = DateTime.now();
    if (e is EventsDropped) {
      // The daemon skipped events this app was too slow for: a
      // run_finished may be among them.
      unawaited(resyncRuns());
      return;
    }
    if (e case RunEvent(:final runId, :final channelId?)) {
      _runChannel[runId] = channelId;
      if (e is RunStarted && e.parentRunId == null) {
        _activeIn[runId] = channelId;
      }
    }
    if (e case RunStarted(:final runId, :final channelId?)) {
      final label = _labels[runId];
      final agent = e.agentId ?? Agent.defaultId;
      _live[runId] = (
        channel: channelId,
        author: e.parentRunId != null && label != null
            ? '$agent/$label'
            : agent,
      );
    }
    switch (e) {
      case SubagentSpawned(:final childRunId, :final label):
        _labels[childRunId] = label;
      case ToolStarted(:final runId, :final name):
        _phase[runId] = 'Running $name';
      case TextDelta(:final runId):
        _phase[runId] = 'Writing';
      case ToolFinished(:final runId) || AssistantMessage(:final runId):
        _phase[runId] = 'Thinking';
      default:
        break;
    }
    if (e case RunStarted(:final runId)) {
      _activeAgents[runId] = e.agentId ?? Agent.defaultId;
    }
    if (e is RunFinished) {
      _activeIn.remove(e.runId);
      _live.remove(e.runId);
      _phase.remove(e.runId);
      if (_activeAgents.remove(e.runId) != null) workVersion++;
    }
    if (e is ToolFinished) _closeAuthCalls(e.id);
    String? session;
    switch (e) {
      case AuthPromptOpened(:final prompt):
        _openAuthPrompt(prompt);
        return;
      case AuthPromptClosed(:final id):
        if (_dropAuthPrompts((p) => p.id == id)) notifyListeners();
        return;
      case ConnectionsChanged():
        if (canManageConnections) unawaited(loadConnections());
        return;
      case ApprovalRequested(:final approval):
        // With channels, in the conversation the run is in.
        session = hasChannels
            ? _requestKey(approval.channelId)
            : approval.rootSessionId ?? approval.sessionId;
        _runSession[approval.runId] ??= approval.sessionId;
        _approvalSession[approval.runId] = session;
      case SecretRequested(:final request):
        session = _routeSecret(request);
      case RunStarted(:final runId, :final sessionId):
        session = sessionId;
        _runSession[runId] = sessionId;
        if (e.parentRunId case final p?) _parentOf[runId] = p;
        // A run in the open stream we haven't placed: maybe a new segment.
        if (e.channelId == streamChannel &&
            e.parentRunId == null &&
            streamOpen &&
            !_streamSessions.contains(sessionId)) {
          unawaited(_refreshChannels());
        }
      case SubagentSpawned(
        :final runId,
        :final childRunId,
        :final childSessionId,
      ):
        session = e.sessionId ?? _runSession[runId];
        _runSession[childRunId] = childSessionId;
        _parentOf[childRunId] = runId;
      case ApprovalAnswered(:final runId) when _approvalSession[runId] != null:
        session = _approvalSession[runId];
      case RunEvent(:final runId, :final sessionId):
        session = sessionId ?? _runSession[runId];
        if (sessionId != null) _runSession[runId] = sessionId;
        if (e.parentRunId case final p?) _parentOf[runId] ??= p;
      case ProvidersChanged():
        unawaited(refreshInfo());
        return;
      case ModelsChanged():
        // Only re-fetch lists someone is looking at.
        if (modelLists != null) unawaited(loadModels());
        return;
      case McpChanged():
        // State changes also add or drop tools.
        unawaited(loadMcp());
        unawaited(refreshInfo());
        return;
      case PostCreated(:final post):
        _onPost(post);
        return;
      case PostUpdated(:final post):
        _onPostUpdated(post);
        return;
      case ChannelsChanged():
        unawaited(_refreshChannels());
        return;
      case AgentsChanged():
        unawaited(_refreshAgents());
      case OutboxChanged(:final id, :final state):
        // Its needs-you post and pointer close with the decision.
        if (state != 'pending') _resolveOutboxPosts(id);
        unawaited(refreshOutbox());
        return;
      case SchedulesChanged():
        unawaited(refreshSchedules());
        return;
      case UnknownEvent() || EventsDropped():
        return;
    }
    if (session == null) {
      // A run we haven't seen start; learn where it belongs.
      unawaited(_learnRuns());
      return;
    }
    var changed = _transcriptFor(session)!.apply(e);
    // A run's cards wait in its conversation, which may not be the
    // transcript its events land in: a run ending closes them there too.
    if (e is RunFinished && hasChannels) {
      for (final t in transcripts.values) {
        if (identical(t, transcripts[session])) continue;
        t
          ..expireApprovals(e.runId)
          ..expireSecretRequests(e.runId);
      }
    }
    if (e is ToolFinished) changed = _closeSecrets(e) || changed;
    if (e is RunEvent && _parentOf.containsKey(e.runId)) {
      changed = _onChildEvent(e, session) || changed;
    } else if (e is RunFinished) {
      unawaited(_afterRun(session, e));
    } else if (e is UsageEvent) {
      unawaited(_refreshUsage(session));
    }
    if (e is ApprovalRequested) _notifyApproval(e.approval);
    if (e is SecretRequested) _notifySecret(e.request, session);
    if (changed) notifyListeners();
  }

  /// Reflect a sub-agent's progress on its parent's agents card.
  bool _onChildEvent(RunEvent e, String session) {
    final parent = _parentTranscript(e.runId);
    switch (e) {
      case RunStarted(:final runId):
        return parent?.updateChild(runId, status: 'running') ?? false;
      case RunFinished(:final runId, :final status, :final error):
        // Its approvals live in the conversation the user started.
        if (_approvalSession[runId] case final root?) {
          transcriptOf(root)?.expireApprovals(runId);
        }
        if (_secretSession[runId] case final root?) {
          transcriptOf(root)?.expireSecretRequests(runId);
        }
        final reply = transcripts[session]?.items
            .whereType<AssistantItem>()
            .lastOrNull
            ?.text;
        return parent?.updateChild(
              runId,
              status: switch (status) {
                'interrupted' when error != null => 'cancelled',
                _ => status,
              },
              result: reply,
              error: error,
              finished: DateTime.now(),
            ) ??
            false;
      default:
        // Its own transcript changed; the card shows that too.
        return parent != null;
    }
  }

  /// Notify only when the user can't already see it.
  bool _unseen(String session) =>
      !appActive ||
      (selected != session &&
          thread != session &&
          !(streamOpen && _streamSessions.contains(session)));

  void _notifyApproval(Approval a) {
    if (!_unseen(a.sessionId)) return;
    unawaited(
      notifier.show(
        id: a.id,
        title: '${askerName(a)} wants to run ${a.tool}',
        body: [a.headline, ?a.taintNote].join('\n'),
        approval: true,
      ),
    );
  }

  /// Where a request's card belongs: the conversation its run is in, or
  /// without channels the conversation the user started.
  String _routeSecret(SecretRequest r) {
    final session = hasChannels
        ? _requestKey(r.channelId)
        : r.rootSessionId ?? r.sessionId;
    _runSession[r.runId] ??= r.sessionId;
    _secretSession[r.runId] = session;
    return session;
  }

  /// A `secret_request` call ended: its card closes, whoever answered.
  /// (A sub-agent's event lands in its own transcript; the card is in the
  /// root's.)
  bool _closeSecrets(ToolFinished e) {
    var changed = false;
    for (final t in transcripts.values) {
      changed = t.closeSecretRequests(e.runId, e.id, e.output) || changed;
    }
    return changed;
  }

  // The notification names the request, never a value field.
  void _notifySecret(SecretRequest r, String session) {
    if (!_unseen(session)) return;
    final names = r.secrets.map((s) => s.name).join(', ');
    unawaited(
      notifier.show(
        id: r.id,
        title: '${r.agentLabel ?? 'The agent'} is asking for a secret',
        body: names,
      ),
    );
  }

  /// A card arrived (a new one for the same call replaces the old: the
  /// browser page, then a device code).
  void _openAuthPrompt(AuthPrompt p) {
    _dropAuthPrompts(
      (o) => o.id == p.id || (p.callId != null && o.callId == p.callId),
    );
    authPrompts = [...authPrompts, p];
    notifyListeners();
    if (appActive) return;
    unawaited(
      notifier.show(
        id: p.id,
        title: p.card.title,
        body: p.card.choose
            ? 'Choose how to connect'
            : 'Finish signing in to continue',
      ),
    );
  }

  bool _dropAuthPrompts(bool Function(AuthPrompt) test) {
    final n = authPrompts.length;
    authPrompts = [
      for (final p in authPrompts)
        if (!test(p)) p,
    ];
    return authPrompts.length != n;
  }

  /// The tool call that asked ended: its card closes, whoever answered.
  void _closeAuthCalls(String callId) {
    if (_dropAuthPrompts((p) => p.callId == callId)) notifyListeners();
  }

  void _notifyFinished(String session, RunFinished e) {
    // A quiet run (`NO_REPLY`) had nothing to say.
    if (!_unseen(session) || e.status == 'interrupted' || e.quiet) return;
    final s = sessions.where((s) => s.id == session).firstOrNull;
    final agent =
        (s?.channelId == null ? null : agentOf(s!.channelId!)) ??
        streamAgentFor(session) ??
        defaultAgent;
    final name = agent?.name ?? 'tern';
    // Another agent's chat says whose it is.
    final title = switch ((s?.title, agent?.isDefault ?? true)) {
      (final t?, true) => t,
      (final t?, false) => '$name · $t',
      (null, _) => name,
    };
    final reply = transcriptOf(session)?.items
        .whereType<AssistantItem>()
        .lastOrNull
        ?.text
        .trim()
        .split('\n')
        .first;
    unawaited(
      notifier.show(
        id: session,
        title: title,
        body: e.status == 'failed'
            ? 'Failed: ${e.error ?? 'unknown error'}'
            : reply ?? 'Done',
      ),
    );
  }

  void _onNotification(String id, NotificationAction action) {
    if (action == NotificationAction.open) {
      // An approval id opens the conversation it belongs to.
      final session =
          transcripts.entries
              .where(
                (t) => t.value.items.any(
                  (i) =>
                      (i is ApprovalItem && i.approval.id == id) ||
                      (i is SecretRequestItem && i.request.id == id),
                ),
              )
              .map((t) => t.key)
              .firstOrNull ??
          id;
      unawaited(
        session == homeKey
            ? openHome()
            : _streamSessions.contains(session)
            ? openStream(streamChannel)
            : open(session),
      );
      return;
    }
    final item = transcripts.values
        .expand((t) => t.items)
        .whereType<ApprovalItem>()
        .where((a) => a.approval.id == id && a.outcome == null)
        .firstOrNull;
    if (item != null) {
      unawaited(
        answer(
          item,
          action == NotificationAction.allow ? Decision.once : Decision.deny,
        ),
      );
    }
  }

  @override
  void notifyListeners() {
    final pending = pendingApprovals + pendingSecrets;
    if (pending != _badge) {
      _badge = pending;
      unawaited(notifier.badge(pending));
    }
    super.notifyListeners();
  }

  /// Make every "working" state match the daemon's active runs: a run it
  /// no longer has (its `run_finished` was missed) stops showing as busy,
  /// and the open views reload what it did. On reconnect, when the event
  /// stream says it dropped events, from Stop, and when a run has been
  /// silent too long.
  Future<void> resyncRuns() async {
    final List<ActiveRun> active;
    try {
      active = await client.activeRuns();
    } on TernException {
      return;
    }
    _lastEvent = DateTime.now();
    final live = {for (final r in active) r.runId};
    var reload = false;
    for (final MapEntry(key: key, value: t) in transcripts.entries) {
      if (t.activeRun case final run? when !live.contains(run)) {
        t.endRun(run);
        reload |= key == streamKey || key == selected || key == thread;
      }
    }
    _activeIn.removeWhere((run, _) => !live.contains(run));
    _live.removeWhere((run, _) => !live.contains(run));
    _phase.removeWhere((run, _) => !live.contains(run));
    for (final r in active) {
      _runSession[r.runId] = r.sessionId;
      if (r.parentRunId case final p?) {
        _parentOf[r.runId] = p;
        continue;
      }
      _transcriptFor(r.sessionId)!.activeRun = r.runId;
      if (r.channelId case final ch?) _activeIn[r.runId] = ch;
    }
    if (reload) {
      try {
        if (streamOpen) await _loadHome();
        if (selected case final id? when !streamOpen) await _load(id);
        if (thread case final id?) await _load(id, limit: threadPage);
      } on TernException {
        // The next resync or reconnect tries again.
      }
    }
    notifyListeners();
  }

  /// When the last event arrived, for [_checkStale].
  DateTime _lastEvent = DateTime.now();
  Timer? _staleTimer;

  /// How long a run may be silent before the app asks the daemon whether
  /// it is still working. Runs waiting on a tool or a model can be quiet
  /// for a while; asking costs one request.
  static const staleAfter = Duration(seconds: 45);

  void _checkStale() {
    final busy = transcripts.values.any((t) => t.activeRun != null);
    if (busy &&
        link == Link.online &&
        DateTime.now().difference(_lastEvent) > staleAfter) {
      unawaited(resyncRuns());
    }
  }

  Future<void> _learnRuns() async {
    try {
      for (final r in await client.activeRuns()) {
        _runSession[r.runId] = r.sessionId;
        _transcriptFor(r.sessionId)!.activeRun = r.runId;
      }
      notifyListeners();
    } on TernException {
      // The event stream will report the connection problem.
    }
  }

  /// After each model call, so the context meter moves during a run.
  Future<void> _refreshUsage(String session) async {
    try {
      usage[session] = await client.usage(session);
      notifyListeners();
    } on TernException {
      // Refreshed again when the run finishes.
    }
  }

  Future<void> _afterRun(String session, RunFinished e) async {
    try {
      if (streamOpen && _streamDirty && _streamSessions.contains(session)) {
        await _loadHome();
      }
      sessions = await client.sessions();
      usage[session] = await client.usage(session);
      if (schedules.any((s) => s.sessionId == session)) {
        schedules = await client.schedules();
      }
      _notifyFinished(session, e);
      notifyListeners();
    } on TernException {
      // Refreshed again on reconnect.
    }
  }

  Future<void> open(String id) async {
    page = null;
    channel = null;
    selected = id;
    draft = false;
    error = null;
    notifyListeners();
    try {
      await _load(id);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  void newConversation() {
    // With Home, a new conversation is a new topic in its stream (or in
    // the open agent's DM).
    if (hasChannels) {
      unawaited(openStream(streamOpen ? channel! : homeId, newTopic: true));
      return;
    }
    page = null;
    selected = null;
    draft = true;
    draftModel = null;
    error = null;
    notifyListeners();
  }

  /// Send [text]. False when it failed and no message was left in the
  /// transcript to retry (a new conversation or topic, a channel post): the
  /// caller should give the text back to the composer.
  Future<bool> send(String text) async {
    if (streamOpen) return _sendHome(text);
    if (topicOpen) return _sendChannel(text);
    error = null;
    final session = selected;
    final shown = session == null ? null : UserItem(text);
    if (session != null) {
      _transcriptFor(session)!.items.add(shown!);
      notifyListeners();
    }
    try {
      final started = await client.send(
        text,
        session: session,
        model: session == null ? draftModel : null,
      );
      _runSession[started.runId] = started.sessionId;
      if (session == null) {
        // Events may have arrived for the new session already.
        final t = _transcriptFor(started.sessionId)!;
        t.items.insert(0, UserItem(text));
        selected = started.sessionId;
        draft = false;
        draftModel = null;
        sessions = await client.sessions();
      }
      _transcriptFor(started.sessionId)!.activeRun ??= started.runId;
    } on TernException catch (e) {
      error = e.message;
      shown?.failed = true;
      notifyListeners();
      return shown != null;
    }
    notifyListeners();
    return true;
  }

  /// Send a message that failed again: it leaves the transcript and goes
  /// through [send] like a new one.
  Future<void> retrySend(UserItem item) async {
    for (final t in transcripts.values) {
      t.items.remove(item);
    }
    item.failed = false;
    notifyListeners();
    await send(item.text);
  }

  /// Send into the open stream (Home or a DM): to the current segment, a
  /// new one when the daemon cuts (or [newTopic]), or queued for the run
  /// already working there.
  Future<bool> _sendHome(String text) async {
    error = null;
    final topic = newTopic;
    final item = UserItem(text);
    final id = streamChannel;
    final t = transcripts.putIfAbsent(streamKey, Transcript.new);
    if (!topic) {
      t.items.add(item);
      notifyListeners();
    }
    try {
      // The folder chosen for a new topic rides on the send, so the first
      // run already uses it.
      final folder = topic ? pendingFolder : null;
      final sent = await client.sendToChannel(
        id,
        text,
        newTopic: topic,
        workspace: folder,
      );
      newTopic = false;
      pendingFolder = null;
      _runSession[sent.runId] = sent.sessionId;
      _runChannel[sent.runId] = id;
      _streamSessions.add(sent.sessionId);
      item.queued = sent.queued;
      if (sent.newSegment || selected != sent.sessionId) {
        // A new segment: show where it was cut. Its carry-over comes with
        // its first run, so look again when that finishes.
        channels = await client.channels();
        await _loadHome();
        _streamDirty = true;
      }
      transcripts[streamKey]?.activeRun ??= sent.runId;
      sessions = await client.sessions();
    } on TernException catch (e) {
      error = e.message;
      if (!topic) item.failed = true;
      notifyListeners();
      return !topic;
    }
    notifyListeners();
    return true;
  }

  Future<void> stop() async {
    final run = current?.activeRun;
    if (run == null) return;
    try {
      // Not cancelled: the daemon has no such run working (it finished and
      // this app missed it). Don't leave Stop doing nothing.
      if (!await client.cancel(run)) await resyncRuns();
    } on TernException catch (e) {
      if (e.status == 404) {
        await resyncRuns();
        return;
      }
      error = e.message;
      notifyListeners();
    }
  }

  Future<void> resume(String run) async {
    try {
      await client.resume(run);
      current?.items.removeWhere((i) => i is RunEndItem && i.runId == run);
      current?.resumeRun(run);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  Future<void> answer(ApprovalItem item, Decision d) async {
    try {
      await client.answer(item.approval.id, d);
      item.outcome = switch (d) {
        Decision.once => 'approved',
        Decision.always => 'always allowed',
        Decision.run when item.approval.grant != null => 'allowed for this run',
        Decision.run => 'allowed all sites for this run',
        Decision.session => 'allowed for this conversation',
        Decision.host => 'site always allowed',
        Decision.deny => 'denied',
      };
    } on TernException catch (e) {
      // Usually: the run already moved on and the approval expired.
      item.outcome = 'expired';
      error = e.message;
    }
    notifyListeners();
  }

  /// Answer every card of a [PendingApprovalGroupItem] at once.
  ///
  /// [Decision.once] and [Decision.deny] go to each card. [Decision.run]
  /// ("Allow all for this run") goes to one card per distinct read scope,
  /// and the daemon releases the others of that scope (its run's other read
  /// tools and sub-agents); cards without a scope are allowed once.
  Future<void> answerGroup(PendingApprovalGroupItem group, Decision d) async {
    final members = [
      for (final m in group.members)
        if (m.outcome == null) m,
    ];
    if (d != Decision.run) {
      for (final m in members) {
        await answer(m, d);
      }
      return;
    }
    final reps = group.grantRepresentatives;
    for (final m in members) {
      final key = m.approval.grant?.key;
      if (key == null) {
        await answer(m, Decision.once);
      } else if (reps.contains(m)) {
        await answer(m, Decision.run);
      } else {
        // Same scope as a card answered above: released by that answer.
        m.outcome = 'allowed for this run';
      }
    }
    notifyListeners();
  }

  /// Pending outbox items for the open stream: the ones its runs queued
  /// (Home's own, or an agent's in its DM). Items from elsewhere wait
  /// there, with a pointer in Home. A core without channels: all of them.
  List<OutboxItem> outboxHere() => [
    for (final o in outbox)
      if (o.pending &&
          (!hasChannels || (o.channelId ?? homeId) == (channel ?? homeId)))
        o,
  ];

  /// Re-read the items waiting in the outbox.
  Future<void> refreshOutbox({bool notify = true}) async {
    try {
      outbox = await client.outbox();
    } on TernException {
      // A device without read access, or an older core.
      return;
    }
    if (notify) notifyListeners();
  }

  /// Approve outbox item [item] (run it once), with [input] replacing its
  /// arguments, or reject it with an optional [note].
  Future<void> decideOutbox(
    OutboxItem item, {
    required bool approve,
    Object? input,
    String? note,
  }) async {
    try {
      if (approve) {
        await client.approveOutbox(item.id, input: input);
      } else {
        await client.rejectOutbox(item.id, note: note);
      }
    } on TernException catch (e) {
      error = e.message;
    }
    await refreshOutbox();
  }

  /// Send what the user entered (only non-empty values; an empty map
  /// skips the request). The values live in the card's fields and the
  /// request body, nowhere else.
  Future<void> answerSecret(
    SecretRequestItem item,
    Map<String, String> values,
  ) async {
    try {
      await client.answerSecretRequest(item.request.id, values);
      item.saved = values.keys.toList();
      item.outcome = values.isEmpty ? 'skipped' : 'saved';
    } on TernException catch (e) {
      switch (e.status) {
        case 403:
          item.readOnly = true;
        case 404:
          // Answered elsewhere, or the run moved on.
          item.outcome = 'expired';
        default:
          error = e.message;
      }
    }
    notifyListeners();
  }

  /// Answer card [p] with an option id, `device` or `cancel`; it closes
  /// here at once (the daemon's close event would too). Returns the
  /// daemon's reason on failure.
  Future<String?> answerAuthPrompt(AuthPrompt p, String answer) async {
    try {
      await client.answerAuthPrompt(p.id, answer);
    } on TernException catch (e) {
      // Gone already: answered elsewhere.
      if (e.status != 404) return e.message;
    }
    // A device answer swaps the card for a code card on the same call; a
    // choice, or a cancel, ends this one.
    if (answer != 'device' && _dropAuthPrompts((o) => o.id == p.id)) {
      notifyListeners();
    }
    return null;
  }

  Future<void> loadConnections({bool notify = true}) async {
    try {
      connectionList = await client.connections();
    } on TernException catch (e) {
      if (notify) error = e.message;
    }
    if (notify && !_disposed) notifyListeners();
  }

  /// Run an account change and re-read the listing. Returns the daemon's
  /// reason on failure. Adding with OAuth takes minutes: the sign-in card
  /// is what the user answers meanwhile.
  Future<String?> connectionsWrite(
    Future<Object?> Function(TernClient c) write,
  ) async {
    try {
      await write(client);
      return null;
    } on TernException catch (e) {
      return e.message;
    } finally {
      await loadConnections();
    }
  }

  /// Show the Connections page.
  void openConnections() {
    page = LibraryPage.connections;
    notifyListeners();
    unawaited(loadConnections());
  }

  Future<void> refreshSchedules() async {
    try {
      schedules = await client.schedules();
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Create or edit a routine. Returns the daemon's reason on failure.
  Future<String?> saveSchedule({
    String? id,
    required String cron,
    required String prompt,
    String? deliverTo,
    ScheduleLimits limits = const ScheduleLimits(),
    String? channel,
    String? agent,
    RoutineOptions? options,
  }) async {
    try {
      if (id == null) {
        await client.createSchedule(
          cron: cron,
          prompt: prompt,
          deliverTo: deliverTo,
          limits: limits,
          channel: channel,
          agent: agent,
          options: options,
        );
      } else {
        await client.updateSchedule(
          id,
          cron: cron,
          prompt: prompt,
          deliverTo: deliverTo ?? 'none',
          limits: limits,
          channel: channel,
          agent: agent,
          options: options,
        );
      }
      await refreshSchedules();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Create a webhook routine (T1c). Returns its hook, secret included
  /// (shown once), or the daemon's reason on failure.
  Future<({Webhook? webhook, String? error})> createWebhook({
    required String prompt,
    required String auth,
    String? deliverTo,
    ScheduleLimits limits = const ScheduleLimits(),
    String? channel,
    String? agent,
    RoutineOptions? options,
  }) async {
    try {
      final created = await client.createWebhook(
        prompt: prompt,
        auth: auth,
        deliverTo: deliverTo,
        limits: limits,
        channel: channel,
        agent: agent,
        options: options,
      );
      await refreshSchedules();
      return (webhook: created.webhook, error: null);
    } on TernException catch (e) {
      return (webhook: null, error: e.message);
    }
  }

  /// Pause or resume a routine. Resuming also lifts a budget pause.
  Future<void> setScheduleEnabled(Schedule s, bool enabled) async {
    try {
      await client.updateSchedule(
        s.id,
        enabled: enabled,
        resume: enabled && s.pausedByBudget,
      );
    } on TernException catch (e) {
      error = e.message;
    }
    await refreshSchedules();
  }

  Future<void> deleteSchedule(Schedule s) async {
    try {
      await client.deleteSchedule(s.id);
    } on TernException catch (e) {
      error = e.message;
    }
    await refreshSchedules();
  }

  /// Run a routine now and open its conversation.
  Future<void> runSchedule(Schedule s) async {
    try {
      final started = await client.runSchedule(s.id);
      _runSession[started.runId] = started.sessionId;
      _transcriptFor(started.sessionId)!.activeRun = started.runId;
      sessions = await client.sessions();
      await open(started.sessionId);
    } on TernException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  /// Validate and set the model for the open conversation.
  /// Use `model` for this conversation (or the next new one), and with
  /// `makeDefault` for new conversations too. Returns the daemon's reason
  /// on failure; a warning (unlisted model) lands in [modelWarning].
  Future<String?> setModel(String model, {bool makeDefault = false}) async {
    try {
      final r = await client.checkModel(model);
      modelWarning = r.warning;
      if (selected case final id?) {
        await client.setModel(id, r.model);
        sessions = await client.sessions();
      } else {
        draftModel = r.model;
      }
      if (makeDefault) {
        await client.setDefaultModel(r.model);
        await refreshInfo(notify: false);
      }
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Set when the last model chosen isn't in its provider's list.
  String? modelWarning;

  /// Re-read the default model and whether setup is needed.
  /// The default model with its fallbacks (T6a), primary first: what
  /// `default_model` holds (read from the config, which only an owner may),
  /// else the default agent's model, else just the default model.
  Future<List<String>> loadDefaultChain() async {
    try {
      final view = await client.configSettings();
      final s = view.settings.where((s) => s.key == 'default_model');
      final chain = chainFromJson(s.firstOrNull?.value);
      if (chain.isNotEmpty) return chain;
    } on TernException {
      // Not an owner, or an older daemon: what the agent list says.
    }
    final fromAgent = defaultAgent?.model ?? const <String>[];
    if (fromAgent.isNotEmpty) return fromAgent;
    return [?info?.defaultModel];
  }

  /// Save `default_model` as [chain] (the primary first). Returns a
  /// sentence on failure, null on success.
  Future<String?> saveDefaultChain(List<String> chain) async {
    try {
      await client.setDefaultModelChain(chain);
    } on TernException catch (e) {
      return chainSaveMessage(e);
    }
    await refreshInfo(notify: false);
    await _refreshAgents();
    notifyListeners();
    return null;
  }

  Future<void> refreshInfo({bool notify = true}) async {
    try {
      info = await client.info();
    } on TernException catch (e) {
      error = e.message;
    }
    if (notify) notifyListeners();
  }

  /// Fetch every provider's models (cached by the daemon unless `refresh`).
  Future<void> loadModels({bool refresh = false}) async {
    if (daemonTooOld) {
      modelLists = [];
      return;
    }
    loadingModels = true;
    notifyListeners();
    try {
      modelLists = await client.models(refresh: refresh);
    } on TernException catch (e) {
      error = e.message;
      modelLists ??= [];
    } finally {
      loadingModels = false;
      if (!_disposed) notifyListeners();
    }
  }

  /// Show the Tasks view.
  void openTasks() {
    page = LibraryPage.tasks;
    notifyListeners();
  }

  /// Show the activity ledger.
  void openLedger() {
    page = LibraryPage.ledger;
    notifyListeners();
  }

  /// Show the Bundles page.
  void openBundles() {
    page = LibraryPage.bundles;
    notifyListeners();
    unawaited(loadBundles());
  }

  Future<void> loadBundles() async {
    try {
      bundles = await client.bundles();
    } on TernException catch (e) {
      error = e.message;
      bundles ??= [];
    }
    if (!_disposed) notifyListeners();
  }

  /// Install what a preview showed, then reload bundles and routines (it
  /// may have added paused ones). The error's text, or null.
  Future<String?> installBundle(
    String source,
    BundlePlan plan, {
    bool acceptNewKey = false,
  }) async {
    try {
      await client.installBundle(
        source,
        sha256: plan.sha256,
        acceptNewKey: acceptNewKey,
      );
      return null;
    } on TernException catch (e) {
      return e.message;
    } finally {
      await loadBundles();
      await refreshSchedules();
    }
  }

  Future<String?> removeBundle(String name) async {
    try {
      await client.removeBundle(name);
      return null;
    } on TernException catch (e) {
      return e.message;
    } finally {
      await loadBundles();
      await refreshSchedules();
    }
  }

  /// Show the Routines page: every routine, by every agent.
  void openRoutines() {
    page = LibraryPage.routines;
    notifyListeners();
    unawaited(refreshSchedules());
  }

  /// Show the MCP servers page, at `server`'s details when given.
  void openMcp([String? server]) {
    page = LibraryPage.mcp;
    mcpSelected = server;
    notifyListeners();
  }

  Future<void> loadMcp({bool notify = true}) async {
    try {
      mcpServers = await client.mcpServers();
    } on TernException catch (e) {
      if (notify) error = e.message;
    }
    if (notify && !_disposed) notifyListeners();
  }

  /// Run an MCP write and re-read the list. Returns the daemon's reason on
  /// failure.
  Future<String?> mcpWrite(Future<Object?> Function(TernClient c) write) async {
    try {
      await write(client);
      return null;
    } on TernException catch (e) {
      return e.message;
    } finally {
      await loadMcp();
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _retry?.cancel();
    _staleTimer?.cancel();
    _events?.cancel();
    client.close();
    super.dispose();
  }
}
