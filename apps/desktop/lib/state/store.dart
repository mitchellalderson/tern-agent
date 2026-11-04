import 'dart:async';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:tern_client/tern_client.dart';

import 'notifier.dart';
import 'transcript.dart';

enum Link { connecting, online, offline }

/// A Library page shown in the centre column instead of a conversation.
enum LibraryPage { mcp }

/// Everything the app knows, fed by the daemon. Widgets listen to it.
///
/// The daemon owns all state: this is a cache rebuilt on every (re)connect,
/// since the event stream has no replay.
class TernStore extends ChangeNotifier {
  TernStore(this.client, {Notifier? notifier})
    : notifier = notifier ?? const SilentNotifier() {
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

  /// The Library page in the centre column, and the MCP server open in it.
  LibraryPage? page;
  String? mcpSelected;

  /// Channels, Home first, when the core has them ([hasChannels]).
  List<Channel> channels = [];

  /// The channel open in the centre column: [homeId], or a topic channel.
  /// Null when a plain conversation ([selected]) is open.
  String? channel;

  /// Home's id (and slug).
  static const homeId = 'home';

  /// Where Home's stream lives in [transcripts]: one transcript for all of
  /// its segments.
  static const homeKey = '#home';

  /// The sessions whose events belong to Home's stream.
  final _homeSessions = <String>{};

  /// Home's stream as loaded, oldest first, and the cursor for older items.
  List<StreamItem> _homeItems = [];
  String? homeNext;

  /// Home has changed underneath (a carry-over to show, a post that came
  /// in mid-run): rebuild it when its run finishes.
  var _homeDirty = false;

  /// The next send in Home starts a new topic.
  bool newTopic = false;

  /// The conversation open in the right pane: a History entry, a side
  /// chat, a post's thread.
  String? thread;

  /// A side chat about to start from a message: what it quotes, and the
  /// message it branched from.
  ({String quote, String? from})? branch;

  /// The model a new conversation will use, when chosen before sending.
  String? draftModel;
  String? error;
  var _badge = 0;

  StreamSubscription<TernEvent>? _events;
  Timer? _retry;
  var _backoff = const Duration(milliseconds: 500);
  var _disposed = false;

  Transcript? get current => homeOpen
      ? transcripts[homeKey]
      : selected == null
      ? null
      : transcripts[selected];

  /// Whether the core has channels and Home's stream (T2).
  bool get hasChannels => health?.features.contains('channels') ?? false;

  bool get homeOpen => channel == homeId;

  Channel? get home => channels.where((c) => c.isHome).firstOrNull;

  /// The transcript showing `session`: Home's for its segments.
  Transcript? transcriptOf(String session) => _homeSessions.contains(session)
      ? transcripts[homeKey]
      : transcripts[session];

  /// The thread pane's transcript.
  Transcript? get threadTranscript =>
      thread == null ? null : transcripts[thread];

  bool get running => current?.activeRun != null;

  /// Approvals waiting anywhere, for badges.
  int get pendingApprovals => transcripts.values
      .expand((t) => t.items)
      .whereType<ApprovalItem>()
      .where((a) => a.outcome == null)
      .length;

  /// Secret requests waiting anywhere.
  int get pendingSecrets => transcripts.values
      .expand((t) => t.items)
      .whereType<SecretRequestItem>()
      .where((r) => r.open)
      .length;

  /// Whether the core can ask for secrets (older ones can't).
  bool get hasSecretRequests =>
      health?.features.contains('secret_requests') ?? false;

  /// Whether the core manages MCP servers over the API (older ones can't).
  bool get hasMcp => health?.features.contains('mcp') ?? false;

  /// Reached as a paired device rather than the local owner socket, so the
  /// core refuses stdio MCP servers unless `api.remote_mcp_stdio` is set.
  bool get remoteCore => health != null && health!.authKind != 'owner';

  String? modelFor(String? session) {
    if (session == null) return draftModel ?? info?.defaultModel;
    final s = sessions.where((x) => x.id == session).firstOrNull;
    return s?.model ?? info?.defaultModel;
  }

  /// No usable model yet: show provider setup instead of the composer.
  bool get setupNeeded => info?.setupNeeded ?? false;

  /// The daemon predates provider setup (`/v1/providers`): it answers 404,
  /// so say why instead.
  bool get daemonTooOld =>
      health != null && !health!.features.contains('providers');

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
      // Home is where the app opens.
      if (channel == null && selected == null && page == null) {
        channel = homeId;
      }
      if (homeOpen) await _loadHome();
    }
    if (selected case final id? when !homeOpen) {
      await _load(id);
    }
    if (thread case final id?) await _load(id);
    for (final r in active) {
      if (r.parentRunId == null) transcriptOf(r.sessionId)?.activeRun = r.runId;
    }
    for (final a in await client.pendingApprovals()) {
      _transcriptFor(a.sessionId)?.apply(ApprovalRequested(a));
    }
    if (hasMcp) await loadMcp(notify: false);
    if (hasSecretRequests) {
      try {
        for (final r in await client.secretRequests()) {
          _routeSecret(r);
          _transcriptFor(r.rootSessionId ?? r.sessionId)
              ?.apply(SecretRequested(r));
        }
      } on TernException {
        // Not available to this device: the event stream still delivers.
      }
    }
  }

  Future<void> _load(String id) async {
    final detail = await client.session(id);
    final previous = transcripts[id];
    final t = Transcript.fromSession(detail)
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

  /// Rebuild Home's stream from the daemon: the newest page (as many items
  /// as were loaded), or with `older` the page before what's loaded.
  Future<void> _loadHome({bool older = false}) async {
    final head = home?.mainSessionId;
    final page = await client.stream(
      homeId,
      before: older ? homeNext : null,
      limit: older ? 200 : max(200, _homeItems.length + 20),
    );
    final fresh = page.items.reversed.toList();
    _homeItems = older ? [...fresh, ..._homeItems] : fresh;
    homeNext = page.next;
    _homeSessions
      ..clear()
      ..addAll([
        for (final i in _homeItems)
          if (i
              case StreamMessage(:final sessionId) ||
                  StreamCut(:final sessionId))
            sessionId,
        ?head,
      ]);
    final previous = transcripts[homeKey];
    final t = Transcript.fromStream(_homeItems)
      ..activeRun = previous?.activeRun
      ..runSince = previous?.runSince;
    for (final a
        in previous?.items.whereType<ApprovalItem>() ?? <ApprovalItem>[]) {
      if (a.outcome == null) t.items.add(a);
    }
    for (final r
        in previous?.items.whereType<SecretRequestItem>() ??
            <SecretRequestItem>[]) {
      if (r.open) t.items.add(r);
    }
    transcripts[homeKey] = t;
    _homeDirty = false;
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
  Future<void> openHome({bool newTopic = false}) async {
    page = null;
    channel = homeId;
    error = null;
    this.newTopic = newTopic;
    selected = home?.mainSessionId;
    draft = selected == null;
    notifyListeners();
    try {
      if (channels.isEmpty) channels = await client.channels();
      await _loadHome();
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

  /// Re-read the channels; follow Home to a new segment started elsewhere.
  Future<void> _refreshChannels() async {
    try {
      channels = await client.channels();
      final head = home?.mainSessionId;
      if (homeOpen && head != null && head != selected) {
        _homeSessions.add(head);
        await _loadHome();
      }
      notifyListeners();
    } on TernException {
      // Refreshed again on reconnect.
    }
  }

  /// A post came in: Home shows it where it happened.
  void _onPost(Post post) {
    unawaited(_refreshChannels());
    if (post.channelId != homeId) return;
    final t = transcripts[homeKey];
    if (t == null) return;
    // Mid-run, appending would split the reply streaming in.
    if (t.activeRun == null) {
      t.items.add(PostItem(post));
      notifyListeners();
    } else {
      _homeDirty = true;
    }
  }

  /// Open a conversation in the right pane (History, a side chat).
  Future<void> openThread(String session) async {
    thread = session;
    branch = null;
    notifyListeners();
    try {
      await _load(session);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  void closeThread() {
    thread = null;
    branch = null;
    notifyListeners();
  }

  /// Start a side chat about `text` in the right pane (sent with its
  /// first message).
  void branchFrom(String text, {int? messageId}) {
    thread = null;
    branch = (
      quote: text,
      from: messageId == null ? null : 'message:$messageId',
    );
    notifyListeners();
  }

  /// Send in the right pane: the side chat's first message, or the next
  /// one in the open conversation (which stops a run in progress there).
  Future<void> sendThread(String text) async {
    error = null;
    try {
      if (branch case final b?) {
        final quoted = '${_quote(b.quote)}\n\n$text';
        final started = await client.send(
          quoted,
          channel: homeId,
          branchedFrom: b.from,
        );
        _runSession[started.runId] = started.sessionId;
        branch = null;
        thread = started.sessionId;
        final t = transcripts.putIfAbsent(started.sessionId, Transcript.new);
        t.items.insert(0, UserItem(quoted));
        t.activeRun ??= started.runId;
        // Its card in the stream.
        if (homeOpen) {
          if (transcripts[homeKey]?.activeRun == null) {
            await _loadHome();
          } else {
            _homeDirty = true;
          }
        }
      } else if (thread case final s?) {
        // Continuing an earlier segment: its run shows here, not in Home.
        if (s != home?.mainSessionId) _homeSessions.remove(s);
        final t = transcripts.putIfAbsent(s, Transcript.new);
        t.items.add(UserItem(text));
        notifyListeners();
        final started = await client.send(text, session: s);
        _runSession[started.runId] = s;
        t.activeRun ??= started.runId;
      }
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  Future<void> stopThread() async {
    if (threadTranscript?.activeRun case final run?) {
      try {
        await client.cancel(run);
      } on TernException catch (e) {
        error = e.message;
        notifyListeners();
      }
    }
  }

  static String _quote(String text) {
    final lines = text.trim().split('\n');
    final kept = lines.take(6).map((l) => '> $l').join('\n');
    return lines.length > 6 ? '$kept\n> …' : kept;
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
    _homeSessions.contains(session) ? homeKey : session,
    Transcript.new,
  );

  @visibleForTesting
  void onEventForTest(TernEvent e) => _onEvent(e);

  void _onEvent(TernEvent e) {
    String? session;
    switch (e) {
      case ApprovalRequested(:final approval):
        session = approval.rootSessionId ?? approval.sessionId;
        _runSession[approval.runId] ??= approval.sessionId;
        _approvalSession[approval.runId] = session;
      case SecretRequested(:final request):
        session = _routeSecret(request);
      case RunStarted(:final runId, :final sessionId):
        session = sessionId;
        _runSession[runId] = sessionId;
        if (e.parentRunId case final p?) _parentOf[runId] = p;
        // A run in Home we haven't placed: maybe a new segment.
        if (e.channelId == homeId &&
            e.parentRunId == null &&
            homeOpen &&
            !_homeSessions.contains(sessionId)) {
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
      case ChannelsChanged():
        unawaited(_refreshChannels());
        return;
      case UnknownEvent():
        return;
    }
    if (session == null) {
      // A run we haven't seen start; learn where it belongs.
      unawaited(_learnRuns());
      return;
    }
    var changed = _transcriptFor(session)!.apply(e);
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
          !(homeOpen && _homeSessions.contains(session)));

  void _notifyApproval(Approval a) {
    if (!_unseen(a.sessionId)) return;
    unawaited(
      notifier.show(
        id: a.id,
        title: '${a.agentLabel ?? 'tern'} wants to run ${a.tool}',
        body: a.headline,
        approval: true,
      ),
    );
  }

  /// Where a request's card belongs: the conversation the user started.
  String _routeSecret(SecretRequest r) {
    final session = r.rootSessionId ?? r.sessionId;
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

  void _notifyFinished(String session, RunFinished e) {
    // A quiet run (`NO_REPLY`) had nothing to say.
    if (!_unseen(session) || e.status == 'interrupted' || e.quiet) return;
    final title = sessions.where((s) => s.id == session).firstOrNull?.title;
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
        title: title ?? 'tern',
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
        session == homeKey || _homeSessions.contains(session)
            ? openHome()
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
      if (homeOpen && _homeDirty && _homeSessions.contains(session)) {
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
    // With Home, a new conversation is a new topic in its stream.
    if (hasChannels) {
      unawaited(openHome(newTopic: true));
      return;
    }
    page = null;
    selected = null;
    draft = true;
    draftModel = null;
    error = null;
    notifyListeners();
  }

  Future<void> send(String text) async {
    if (homeOpen) return _sendHome(text);
    error = null;
    final session = selected;
    if (session != null) {
      _transcriptFor(session)!.items.add(UserItem(text));
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
    }
    notifyListeners();
  }

  /// Send into Home: to the current segment, a new one when the daemon
  /// cuts (or [newTopic]), or queued for the run already working there.
  Future<void> _sendHome(String text) async {
    error = null;
    final topic = newTopic;
    final item = UserItem(text);
    final t = transcripts.putIfAbsent(homeKey, Transcript.new);
    if (!topic) {
      t.items.add(item);
      notifyListeners();
    }
    try {
      final sent = await client.sendToChannel(homeId, text, newTopic: topic);
      newTopic = false;
      _runSession[sent.runId] = sent.sessionId;
      _homeSessions.add(sent.sessionId);
      item.queued = sent.queued;
      if (sent.newSegment || selected != sent.sessionId) {
        // A new segment: show where it was cut. Its carry-over comes with
        // its first run, so look again when that finishes.
        channels = await client.channels();
        await _loadHome();
        _homeDirty = true;
      }
      transcripts[homeKey]?.activeRun ??= sent.runId;
      sessions = await client.sessions();
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  Future<void> stop() async {
    final run = current?.activeRun;
    if (run == null) return;
    try {
      await client.cancel(run);
    } on TernException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  Future<void> resume(String run) async {
    try {
      await client.resume(run);
      current?.items.removeWhere((i) => i is RunEndItem && i.runId == run);
      current?.activeRun = run;
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
        Decision.deny => 'denied',
      };
    } on TernException catch (e) {
      // Usually: the run already moved on and the approval expired.
      item.outcome = 'expired';
      error = e.message;
    }
    notifyListeners();
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
  }) async {
    try {
      if (id == null) {
        await client.createSchedule(
          cron: cron,
          prompt: prompt,
          deliverTo: deliverTo,
          limits: limits,
        );
      } else {
        await client.updateSchedule(
          id,
          cron: cron,
          prompt: prompt,
          deliverTo: deliverTo ?? 'none',
          limits: limits,
        );
      }
      await refreshSchedules();
      return null;
    } on TernException catch (e) {
      return e.message;
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
    _events?.cancel();
    client.close();
    super.dispose();
  }
}
